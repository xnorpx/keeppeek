use crate::{
    api::{HomeKitAccessorySettings, HomeKitSettings},
    cameras::{Camera, VideoEncoding},
    config::{HomeKitConfig, write_private_file_atomically},
    keeppeek::StreamKind,
    shutdown::Shutdown,
    webrtc::{HomeKitTransportState, Source, WebRtc},
};

mod legacy_rtp;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hap_video::{
    AccessoryCategory, AccessoryDatabase, AccessoryId, AccessoryIdentity, AccessoryInformation,
    Action as WebRtcAction, AudioTier, BonjourStatus, Characteristic as WebRtcCharacteristic,
    ContentType, ControllerPairing, DecodeResult, Endpoint, Event as WebRtcEvent, HttpParseResult,
    Input as WebRtcInput, Method, Output as WebRtcOutput, PairSetup, PairSetupInput,
    PairSetupOutput, PairSetupState, PairVerify, PairVerifyInput, PairVerifyOutput,
    PairVerifyState, PairingStoreResult, Pairings, PairingsInput, PairingsOutput,
    PairingsStoreResult, RecordDecoder, RecordEncoder, Request, RequestId as WebRtcRequestId,
    Response, SessionId as WebRtcSessionId, SessionKeys, SessionState as WebRtcSessionState,
    SetupCode, SetupId, SetupPayload, Status, VideoCodec, VideoQuality, VideoTier, WebRtcDevice,
    encode_event,
};
use hap_video::{decode_selected_stream, decode_setup_endpoints};
use image::{ColorType, codecs::jpeg::JpegEncoder};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use qrcode::{QrCode, render::svg};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{ErrorKind, Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::JoinHandle,
    time::Duration,
};

const STATE_DIRECTORY: &str = "homekit";
const STATE_INDEX_FILE: &str = "accessories.json";
const STATE_VERSION: u32 = 1;
const CONFIGURATION_NUMBER_INITIAL: u32 = 1;
const MAX_PAIRINGS: usize = 256;
const MAX_CONNECTION_BYTES: usize = 512 * 1024;
const CONNECTION_TIMEOUT: Duration = Duration::from_millis(250);
const EVENT_QUEUE_CAPACITY: usize = 64;
const MAX_SNAPSHOT_PIXELS: u64 = 16 * 1024 * 1024;
const HAP_SERVICE_COMMUNICATION_FAILURE: i64 = -70402;
const HAP_INVALID_VALUE_IN_REQUEST: i64 = -70410;

pub(crate) fn settings_snapshot(
    config: &HomeKitConfig,
    config_path: &Path,
    exported_camera_count: usize,
) -> anyhow::Result<HomeKitSettings> {
    if !config.enabled {
        return Ok(HomeKitSettings {
            enabled: false,
            name: config.name.clone(),
            bind: config.bind.to_string(),
            port: config.port,
            exported_camera_count,
            accessories: Vec::new(),
        });
    }
    let directory = config_path.parent().unwrap_or_else(|| Path::new("."));
    let state_directory = directory.join(STATE_DIRECTORY);
    let index_path = state_directory.join(STATE_INDEX_FILE);
    let index = if index_path.exists() {
        serde_json::from_slice::<PersistedAccessoryIndex>(&std::fs::read(index_path)?)?
    } else {
        PersistedAccessoryIndex::default()
    };
    let accessories = index
        .accessories
        .iter()
        .map(|entry| {
            let state: PersistedState =
                serde_json::from_slice(&std::fs::read(state_directory.join(&entry.state_file))?)?;
            let paired = !state.pairings.is_empty();
            Ok(HomeKitAccessorySettings {
                camera_id: state.camera_id,
                name: state.name,
                paired,
                pairing_count: state.pairings.len(),
                port: state.port,
                setup_code: (!paired).then_some(state.setup_code),
                setup_qr_svg_base64: if paired {
                    None
                } else {
                    Some(
                        STANDARD.encode(std::fs::read(state_directory.join(&entry.setup_qr_file))?),
                    )
                },
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(HomeKitSettings {
        enabled: true,
        name: config.name.clone(),
        bind: config.bind.to_string(),
        port: config.port,
        exported_camera_count,
        accessories,
    })
}

pub(crate) fn reset_pairings(config_path: &Path, camera_id: &str) -> anyhow::Result<()> {
    let directory = config_path.parent().unwrap_or_else(|| Path::new("."));
    let state_directory = directory.join(STATE_DIRECTORY);
    let index: PersistedAccessoryIndex =
        serde_json::from_slice(&std::fs::read(state_directory.join(STATE_INDEX_FILE))?)?;
    for entry in index.accessories {
        let path = state_directory.join(entry.state_file);
        let data: PersistedState = serde_json::from_slice(&std::fs::read(&path)?)?;
        if data.camera_id != camera_id {
            continue;
        }
        if data.version != STATE_VERSION {
            anyhow::bail!("unsupported HomeKit state version {}", data.version);
        }
        let mut store = StateStore { path, data };
        store.data.pairings.clear();
        store.save()?;
        return Ok(());
    }
    anyhow::bail!("HomeKit accessory for camera {camera_id} was not found")
}

/// Running standalone HomeKit camera services.
pub struct HomeKitService {
    accessories: Vec<RunningAccessory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeKitProbeProfile {
    Legacy,
    WebRtc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeKitProbeRequestKind {
    LegacySetupEndpoints,
    WebRtcSolicitOffer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeKitProbeRequest {
    pub kind: HomeKitProbeRequestKind,
    pub camera_ip: IpAddr,
    pub name: String,
}

struct RunningAccessory {
    address: SocketAddr,
    handle: JoinHandle<anyhow::Result<()>>,
}

impl HomeKitService {
    /// Starts HomeKit when enabled and returns `None` otherwise.
    pub fn start(
        config: &HomeKitConfig,
        config_path: &Path,
        cameras: &HashMap<IpAddr, Camera>,
        webrtc: WebRtc,
        shutdown: Shutdown,
    ) -> anyhow::Result<Option<Self>> {
        Self::start_inner(
            config,
            config_path,
            cameras,
            webrtc,
            shutdown,
            HomeKitProbeProfile::WebRtc,
            None,
            true,
            None,
        )
    }

    /// Starts one HomeKit camera backed by an FFmpeg-readable file.
    #[allow(clippy::too_many_arguments)]
    pub fn start_legacy_file(
        config: &HomeKitConfig,
        config_path: &Path,
        cameras: &HashMap<IpAddr, Camera>,
        webrtc: WebRtc,
        ffmpeg: &Path,
        input: &Path,
        shutdown: Shutdown,
        profile: HomeKitProbeProfile,
        force_hevc: bool,
    ) -> anyhow::Result<Option<Self>> {
        Self::start_inner(
            config,
            config_path,
            cameras,
            webrtc,
            shutdown,
            profile,
            None,
            true,
            Some(LegacyFileSource {
                ffmpeg: ffmpeg.to_path_buf(),
                input: input.to_path_buf(),
                force_hevc,
            }),
        )
    }

    pub fn start_probe(
        config: &HomeKitConfig,
        config_path: &Path,
        cameras: &HashMap<IpAddr, Camera>,
        webrtc: WebRtc,
        shutdown: Shutdown,
        profile: HomeKitProbeProfile,
        requests: SyncSender<HomeKitProbeRequest>,
    ) -> anyhow::Result<Option<Self>> {
        Self::start_inner(
            config,
            config_path,
            cameras,
            webrtc,
            shutdown,
            profile,
            Some(requests),
            false,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_inner(
        config: &HomeKitConfig,
        config_path: &Path,
        cameras: &HashMap<IpAddr, Camera>,
        webrtc: WebRtc,
        shutdown: Shutdown,
        profile: HomeKitProbeProfile,
        probe_requests: Option<SyncSender<HomeKitProbeRequest>>,
        write_accessory_index: bool,
        legacy_file: Option<LegacyFileSource>,
    ) -> anyhow::Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        validate_config(config)?;
        let directory = config_path.parent().unwrap_or_else(|| Path::new("."));
        let state_directory = directory.join(STATE_DIRECTORY);
        std::fs::create_dir_all(&state_directory)?;
        let camera_exports = sorted_camera_exports(cameras);
        let mut running = Vec::with_capacity(camera_exports.len());
        let mut persisted_index = PersistedAccessoryIndex::default();

        for (index, (camera_key, camera)) in camera_exports.into_iter().enumerate() {
            let state_stem = state_stem(&camera_key);
            let state_file = format!("{state_stem}.json");
            let setup_file = format!("{state_stem}-setup.txt");
            let setup_qr_file = format!("{state_stem}-setup.svg");
            let camera_id = camera.config.ip.to_string();
            let name = camera
                .config
                .display_name()
                .unwrap_or("KeepPeek Camera")
                .to_owned();
            let mut store = StateStore::load_or_create(
                state_directory.join(&state_file),
                &camera_key,
                &camera_id,
                &name,
            )?;
            let camera_config = camera_accessory(camera, store.data.sensor_uuid);
            let accessories = match profile {
                HomeKitProbeProfile::Legacy => AccessoryDatabase::legacy_camera(camera_config),
                HomeKitProbeProfile::WebRtc => AccessoryDatabase::camera(camera_config),
            }?
            .to_json()?;
            store.update_accessory_database(&accessories);

            let offset =
                u16::try_from(index).map_err(|_| anyhow::anyhow!("too many HomeKit cameras"))?;
            let port = if config.port == 0 {
                0
            } else {
                config
                    .port
                    .checked_add(offset)
                    .ok_or_else(|| anyhow::anyhow!("HomeKit camera port range exceeds 65535"))?
            };
            let listener = TcpListener::bind((config.bind, port))?;
            listener.set_nonblocking(true)?;
            let address = listener.local_addr()?;
            store.data.port = address.port();
            store.save()?;

            let setup = store.setup_payload()?;
            write_setup_artifacts(&state_directory, &setup_file, &setup_qr_file, &setup)?;
            let identity = AccessoryIdentity::new(
                setup.accessory_id().to_string().into_bytes(),
                store.data.signing_seed,
            )?;
            let status = if store.data.pairings.is_empty() {
                BonjourStatus::NotPaired
            } else {
                BonjourStatus::Paired
            };
            let advertiser = MdnsPublisher::start(
                &advertised_name(&name, &setup),
                address.port(),
                store.data.configuration_number,
                status,
                &setup,
            )?;
            let shared = Arc::new(Shared {
                name: name.clone(),
                identity,
                setup_code: setup.code(),
                characteristic_values: Mutex::new(extract_characteristic_values(&accessories)?),
                accessories,
                store: Mutex::new(store),
                advertiser: Mutex::new(Some(advertiser)),
                webrtc: webrtc.clone(),
                camera_sources: HashMap::from([(
                    1,
                    Source {
                        camera_ip: camera.config.ip,
                        stream: StreamKind::Main,
                    },
                )]),
                webrtc_devices: Mutex::new(HashMap::from([(1, WebRtcDevice::new())])),
                legacy_rtp: Mutex::new(legacy_rtp::LegacyRtpManager::new(
                    legacy_file
                        .as_ref()
                        .map_or_else(default_ffmpeg_executable, |source| source.ffmpeg.clone()),
                    legacy_file
                        .as_ref()
                        .map(|source| legacy_rtp::FfmpegInput::File(source.input.clone()))
                        .or_else(|| legacy_ffmpeg_input(camera)),
                    legacy_file.as_ref().is_some_and(|source| source.force_hevc),
                )),
                subscribers: Mutex::new(HashMap::new()),
                next_connection: AtomicU64::new(1),
                next_webrtc_request: AtomicU64::new(1),
                probe_requests: probe_requests.clone(),
            });

            let service_shutdown = shutdown.clone();
            let error_shutdown = shutdown.clone();
            let thread_name = format!("homekit-{}", &state_stem[..8]);
            let handle = std::thread::Builder::new()
                .name(thread_name)
                .spawn(move || {
                    let result = run(listener, shared, service_shutdown);
                    if result.is_err() {
                        error_shutdown.cancel();
                    }
                    result
                })?;
            tracing::info!(
                camera = %camera_id,
                accessory = %name,
                %address,
                setup = %state_directory.join(&setup_qr_file).display(),
                "HomeKit camera accessory is discoverable",
            );
            persisted_index
                .accessories
                .push(PersistedAccessoryIndexEntry {
                    state_file,
                    setup_qr_file,
                });
            running.push(RunningAccessory { address, handle });
        }

        if write_accessory_index {
            let mut index = serde_json::to_vec_pretty(&persisted_index)?;
            index.push(b'\n');
            write_private_file_atomically(&state_directory.join(STATE_INDEX_FILE), &index)?;
        }
        Ok(Some(Self {
            accessories: running,
        }))
    }

    pub fn addresses(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.accessories.iter().map(|accessory| accessory.address)
    }

    pub fn join(self) {
        for accessory in self.accessories {
            match accessory.handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error, "HomeKit service stopped with error"),
                Err(_) => tracing::warn!("HomeKit service panicked"),
            }
        }
    }
}

#[derive(Clone)]
struct LegacyFileSource {
    ffmpeg: PathBuf,
    input: PathBuf,
    force_hevc: bool,
}

fn default_ffmpeg_executable() -> PathBuf {
    std::env::var_os("KEEPPEEK_FFMPEG").map_or_else(
        || {
            ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
                .into_iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
                .unwrap_or_else(|| PathBuf::from("ffmpeg"))
        },
        PathBuf::from,
    )
}

fn legacy_ffmpeg_input(camera: &Camera) -> Option<legacy_rtp::FfmpegInput> {
    let stream_uri = camera
        .config
        .main_rtsp_url
        .as_deref()
        .filter(|uri| !uri.trim().is_empty())
        .or_else(|| camera.profiles.first()?.stream_uri.as_deref())?;
    let mut url = url::Url::parse(stream_uri).ok()?;
    if !matches!(url.scheme(), "rtsp" | "rtsps") {
        return None;
    }
    if url.username().is_empty() && !camera.config.username.is_empty() {
        url.set_username(&camera.config.username).ok()?;
    }
    if url.password().is_none() && !camera.config.password.is_empty() {
        url.set_password(Some(&camera.config.password)).ok()?;
    }
    let transport = match camera.config.transport {
        crate::cameras::CameraTransport::Tcp => "tcp",
        crate::cameras::CameraTransport::Udp => "udp",
    };
    Some(legacy_rtp::FfmpegInput::Rtsp {
        url: url.into(),
        transport,
    })
}

fn validate_config(config: &HomeKitConfig) -> anyhow::Result<()> {
    if config.name.trim().is_empty() {
        anyhow::bail!("HomeKit name must not be empty");
    }
    if config.name.len() > 63 {
        anyhow::bail!("HomeKit name must be at most 63 UTF-8 bytes");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedState {
    version: u32,
    camera_key: String,
    camera_id: String,
    name: String,
    port: u16,
    accessory_id: [u8; 6],
    signing_seed: [u8; 32],
    setup_code: String,
    setup_id: String,
    configuration_number: u32,
    #[serde(default)]
    accessory_database_hash: Option<[u8; 20]>,
    sensor_uuid: [u8; 16],
    #[serde(default)]
    pairings: Vec<PersistedPairing>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedAccessoryIndex {
    #[serde(default = "state_version")]
    version: u32,
    #[serde(default)]
    accessories: Vec<PersistedAccessoryIndexEntry>,
}

impl Default for PersistedAccessoryIndex {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            accessories: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedAccessoryIndexEntry {
    state_file: String,
    setup_qr_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedPairing {
    identifier: Vec<u8>,
    public_key: [u8; 32],
    administrator: bool,
}

impl From<ControllerPairing> for PersistedPairing {
    fn from(pairing: ControllerPairing) -> Self {
        Self {
            identifier: pairing.identifier,
            public_key: pairing.public_key,
            administrator: pairing.administrator,
        }
    }
}

impl From<&PersistedPairing> for ControllerPairing {
    fn from(pairing: &PersistedPairing) -> Self {
        Self {
            identifier: pairing.identifier.clone(),
            public_key: pairing.public_key,
            administrator: pairing.administrator,
        }
    }
}

struct StateStore {
    path: PathBuf,
    data: PersistedState,
}

impl StateStore {
    fn load_or_create(
        path: PathBuf,
        camera_key: &str,
        camera_id: &str,
        name: &str,
    ) -> anyhow::Result<Self> {
        if path.exists() {
            let mut data: PersistedState = serde_json::from_slice(&std::fs::read(&path)?)?;
            if data.version != STATE_VERSION {
                anyhow::bail!("unsupported HomeKit state version {}", data.version);
            }
            if data.camera_key != camera_key {
                anyhow::bail!("HomeKit state belongs to a different camera");
            }
            data.camera_id = camera_id.to_owned();
            data.name = name.to_owned();
            data.setup_payload()?;
            return Ok(Self { path, data });
        }
        let data = PersistedState::generate(camera_key, camera_id, name);
        let store = Self { path, data };
        store.save()?;
        Ok(store)
    }

    fn setup_payload(&self) -> anyhow::Result<SetupPayload> {
        self.data.setup_payload()
    }

    fn save(&self) -> anyhow::Result<()> {
        let mut value = serde_json::to_vec_pretty(&self.data)?;
        value.push(b'\n');
        write_private_file_atomically(&self.path, &value)?;
        Ok(())
    }

    const fn is_paired(&self) -> bool {
        !self.data.pairings.is_empty()
    }

    fn pairing(&self, identifier: &[u8]) -> Option<ControllerPairing> {
        self.data
            .pairings
            .iter()
            .find(|pairing| pairing.identifier == identifier)
            .map(ControllerPairing::from)
    }

    fn pairings(&self) -> Vec<ControllerPairing> {
        self.data
            .pairings
            .iter()
            .map(ControllerPairing::from)
            .collect()
    }

    fn store_pairing(&mut self, pairing: ControllerPairing) -> anyhow::Result<()> {
        if let Some(existing) = self
            .data
            .pairings
            .iter_mut()
            .find(|existing| existing.identifier == pairing.identifier)
        {
            *existing = pairing.into();
        } else {
            self.data.pairings.push(pairing.into());
        }
        self.save()
    }

    fn remove_pairing(&mut self, identifier: &[u8]) -> anyhow::Result<()> {
        self.data
            .pairings
            .retain(|pairing| pairing.identifier != identifier);
        if !self
            .data
            .pairings
            .iter()
            .any(|pairing| pairing.administrator)
        {
            self.data.pairings.clear();
        }
        self.save()
    }

    fn update_accessory_database(&mut self, accessories: &[u8]) {
        let hash: [u8; 20] = Sha1::digest(accessories).into();
        if self
            .data
            .accessory_database_hash
            .is_some_and(|previous| previous != hash)
        {
            self.data.configuration_number = self
                .data
                .configuration_number
                .checked_add(1)
                .unwrap_or(CONFIGURATION_NUMBER_INITIAL);
        }
        self.data.accessory_database_hash = Some(hash);
    }
}

impl PersistedState {
    fn generate(camera_key: &str, camera_id: &str, name: &str) -> Self {
        let mut accessory_id: [u8; 6] = rand::random();
        accessory_id[0] = (accessory_id[0] | 0x02) & 0xfe;
        let setup_code = loop {
            let value = rand::random::<u32>() % 100_000_000;
            let digits = format!("{value:08}");
            let candidate = format!("{}-{}-{}", &digits[..3], &digits[3..5], &digits[5..]);
            if SetupCode::parse(&candidate).is_ok() {
                break candidate;
            }
        };
        const SETUP_ID_ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let random: [u8; 4] = rand::random();
        let setup_id = random
            .into_iter()
            .map(|value| char::from(SETUP_ID_ALPHABET[usize::from(value) % 36]))
            .collect();
        Self {
            version: STATE_VERSION,
            camera_key: camera_key.to_owned(),
            camera_id: camera_id.to_owned(),
            name: name.to_owned(),
            port: 0,
            accessory_id,
            signing_seed: rand::random(),
            setup_code,
            setup_id,
            configuration_number: CONFIGURATION_NUMBER_INITIAL,
            accessory_database_hash: None,
            sensor_uuid: rand::random(),
            pairings: Vec::new(),
        }
    }

    fn setup_payload(&self) -> anyhow::Result<SetupPayload> {
        Ok(SetupPayload::new(
            SetupCode::parse(&self.setup_code)?,
            SetupId::parse(&self.setup_id)?,
            AccessoryId::new(self.accessory_id),
            AccessoryCategory::IpCamera,
        ))
    }
}

const fn state_version() -> u32 {
    STATE_VERSION
}

fn write_setup_artifacts(
    directory: &Path,
    setup_file: &str,
    setup_qr_file: &str,
    setup: &SetupPayload,
) -> anyhow::Result<()> {
    let uri = setup.uri();
    let text = format!("Setup code: {}\nSetup URI: {uri}\n", setup.code());
    write_private_file_atomically(&directory.join(setup_file), text.as_bytes())?;
    let qr = QrCode::new(uri.as_bytes())?;
    let image = qr
        .render::<svg::Color>()
        .min_dimensions(512, 512)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    write_private_file_atomically(&directory.join(setup_qr_file), image.as_bytes())?;
    Ok(())
}

fn sorted_camera_exports(cameras: &HashMap<IpAddr, Camera>) -> Vec<(String, &Camera)> {
    let keyed = cameras
        .values()
        .map(|camera| (camera_key_base(camera), camera))
        .collect::<Vec<_>>();
    let key_counts = keyed.iter().fold(HashMap::new(), |mut counts, (key, _)| {
        *counts.entry(key.clone()).or_insert(0_usize) += 1;
        counts
    });
    let mut sorted = keyed
        .into_iter()
        .map(|(key, camera)| {
            let key = if key_counts[&key] > 1 {
                format!("{key}@{}", camera_key_disambiguator(camera))
            } else {
                key
            };
            (key, camera)
        })
        .collect::<Vec<_>>();
    sorted.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    sorted
}

fn state_stem(camera_key: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha1::digest(camera_key.as_bytes());
    let mut stem = String::with_capacity(digest.len() * 2);
    for byte in digest {
        stem.push(char::from(HEX[usize::from(byte >> 4)]));
        stem.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    stem
}

fn camera_key_base(camera: &Camera) -> String {
    camera.config.uid.as_ref().map_or_else(
        || {
            camera.device.serial_number.as_ref().map_or_else(
                || {
                    camera.mac_address.as_ref().map_or_else(
                        || format!("ip:{}", camera.config.ip),
                        |mac| format!("mac:{mac}"),
                    )
                },
                |serial| format!("serial:{serial}"),
            )
        },
        |uid| format!("uid:{uid}"),
    )
}

fn camera_key_disambiguator(camera: &Camera) -> String {
    camera
        .config
        .uid
        .as_deref()
        .or(camera.device.serial_number.as_deref())
        .or(camera.mac_address.as_deref())
        .map_or_else(|| camera.config.ip.to_string(), str::to_owned)
}

fn camera_accessory(camera: &Camera, sensor_uuid: [u8; 16]) -> hap_video::CameraConfig {
    let profile = camera
        .profiles
        .iter()
        .filter_map(|profile| profile.video.as_ref())
        .max_by_key(|video| u64::from(video.width) * u64::from(video.height));
    let codec = match profile.map(|video| &video.encoding) {
        Some(VideoEncoding::H265) => VideoCodec::H265,
        _ => VideoCodec::H264,
    };
    let width = profile
        .map_or(1920, |video| video.width)
        .clamp(1, u32::from(u16::MAX)) as u16;
    let height = profile
        .map_or(1080, |video| video.height)
        .clamp(1, u32::from(u16::MAX)) as u16;
    let frame_rate = profile
        .map_or(30, |video| video.framerate.round() as u32)
        .clamp(1, u32::from(u8::MAX)) as u8;
    let high_bitrate = profile
        .and_then(|video| video.bitrate_kbps)
        .unwrap_or(2_000)
        .max(1);
    let name = camera
        .config
        .display_name()
        .unwrap_or("KeepPeek Camera")
        .to_owned();
    let manufacturer = camera
        .config
        .manufacturer_override()
        .map(str::to_owned)
        .or_else(|| camera.reported_manufacturer.clone())
        .or_else(|| camera.device.manufacturer.clone())
        .unwrap_or_else(|| "KeepPeek".to_owned());
    hap_video::CameraConfig {
        information: AccessoryInformation {
            name,
            manufacturer,
            model: camera
                .device
                .model
                .clone()
                .unwrap_or_else(|| "IP Camera".to_owned()),
            serial_number: camera
                .device
                .serial_number
                .clone()
                .unwrap_or_else(|| camera.config.ip.to_string()),
            firmware_revision: camera
                .device
                .firmware_version
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
        },
        sensor_uuid,
        video_codec: codec,
        video_payload_type: 99,
        video_tiers: vec![
            VideoTier {
                identifier: 1,
                quality: VideoQuality::High,
                target_average_bitrate_kbps: high_bitrate,
                width,
                height,
                frame_rate,
            },
            VideoTier {
                identifier: 2,
                quality: VideoQuality::Medium,
                target_average_bitrate_kbps: high_bitrate.min(1_000),
                width: width.min(1280),
                height: height.min(720),
                frame_rate,
            },
            VideoTier {
                identifier: 3,
                quality: VideoQuality::Low,
                target_average_bitrate_kbps: high_bitrate.min(300),
                width: width.min(640),
                height: height.min(360),
                frame_rate: frame_rate.min(15),
            },
        ],
        opus_payload_type: 111,
        audio_tier: AudioTier {
            identifier: 1,
            target_average_bitrate_bps: 24_000,
        },
    }
}

struct MdnsPublisher {
    daemon: ServiceDaemon,
    fullname: String,
    name: String,
    hostname: String,
    port: u16,
    configuration_number: u32,
    setup: SetupPayload,
}

impl MdnsPublisher {
    fn start(
        name: &str,
        port: u16,
        configuration_number: u32,
        status: BonjourStatus,
        setup: &SetupPayload,
    ) -> anyhow::Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let hostname = format!(
            "keeppeek-{}.local.",
            setup
                .accessory_id()
                .to_string()
                .replace(':', "")
                .to_ascii_lowercase()
        );
        let mut publisher = Self {
            daemon,
            fullname: String::new(),
            name: name.to_owned(),
            hostname,
            port,
            configuration_number,
            setup: setup.clone(),
        };
        publisher.publish(status)?;
        Ok(publisher)
    }

    fn publish(&mut self, status: BonjourStatus) -> anyhow::Result<()> {
        let txt = self
            .setup
            .bonjour_txt(&self.name, self.configuration_number, status);
        let properties = txt
            .iter()
            .filter_map(|record| record.split_once('='))
            .collect::<Vec<_>>();
        let service = ServiceInfo::new(
            "_hap._tcp.local.",
            &self.name,
            &self.hostname,
            "",
            self.port,
            properties.as_slice(),
        )?
        .enable_addr_auto();
        self.fullname = service.get_fullname().to_owned();
        self.daemon.register(service)?;
        Ok(())
    }

    fn shutdown(self) {
        if let Ok(receiver) = self.daemon.unregister(&self.fullname) {
            let _ = receiver.recv_timeout(Duration::from_secs(1));
        }
        if let Ok(receiver) = self.daemon.shutdown() {
            let _ = receiver.recv_timeout(Duration::from_secs(1));
        }
    }
}

struct Shared {
    name: String,
    identity: AccessoryIdentity,
    setup_code: SetupCode,
    accessories: Vec<u8>,
    characteristic_values: Mutex<HashMap<(u64, u64), serde_json::Value>>,
    store: Mutex<StateStore>,
    advertiser: Mutex<Option<MdnsPublisher>>,
    webrtc: WebRtc,
    camera_sources: HashMap<u64, Source>,
    webrtc_devices: Mutex<HashMap<u64, WebRtcDevice>>,
    legacy_rtp: Mutex<legacy_rtp::LegacyRtpManager>,
    subscribers: Mutex<HashMap<u64, EventSubscriber>>,
    next_connection: AtomicU64,
    next_webrtc_request: AtomicU64,
    probe_requests: Option<SyncSender<HomeKitProbeRequest>>,
}

#[derive(Debug, Clone)]
struct CharacteristicEvent {
    aid: u64,
    iid: u64,
    value: serde_json::Value,
}

struct EventSubscriber {
    characteristics: HashSet<(u64, u64)>,
    sender: SyncSender<CharacteristicEvent>,
}

struct ConnectionRegistration {
    shared: Arc<Shared>,
    connection_id: u64,
}

impl ConnectionRegistration {
    fn new(
        shared: Arc<Shared>,
        connection_id: u64,
        sender: SyncSender<CharacteristicEvent>,
    ) -> Self {
        shared.subscribers.lock().unwrap().insert(
            connection_id,
            EventSubscriber {
                characteristics: HashSet::new(),
                sender,
            },
        );
        Self {
            shared,
            connection_id,
        }
    }
}

impl Drop for ConnectionRegistration {
    fn drop(&mut self) {
        self.shared
            .subscribers
            .lock()
            .unwrap()
            .remove(&self.connection_id);
    }
}

fn run(listener: TcpListener, shared: Arc<Shared>, shutdown: Shutdown) -> anyhow::Result<()> {
    let mut connections = Vec::new();
    while !shutdown.is_cancelled() {
        match listener.accept() {
            Ok((stream, peer)) => {
                let connection_id = shared.next_connection.fetch_add(1, Ordering::Relaxed);
                let local = stream.local_addr()?;
                tracing::info!(accessory = %shared.name, connection_id, %peer, %local, "HomeKit TCP connection accepted");
                let connection_shared = shared.clone();
                let connection_shutdown = shutdown.clone();
                match std::thread::Builder::new()
                    .name(format!("homekit-{peer}"))
                    .spawn(move || {
                        match serve_connection(
                            stream,
                            connection_shared,
                            connection_shutdown,
                            connection_id,
                        ) {
                            Ok(()) => {
                                tracing::info!(connection_id, %peer, "HomeKit TCP connection closed");
                            }
                            Err(error) => {
                                tracing::warn!(connection_id, %peer, %error, "HomeKit connection failed");
                            }
                        }
                    }) {
                    Ok(handle) => connections.push(handle),
                    Err(error) => {
                        tracing::warn!(%peer, %error, "unable to start HomeKit connection");
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                shutdown.wait_timeout(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
        let mut index = 0;
        while index < connections.len() {
            if connections[index].is_finished() {
                let connection = connections.swap_remove(index);
                let _ = connection.join();
            } else {
                index += 1;
            }
        }
    }
    for connection in connections {
        let _ = connection.join();
    }
    let advertiser = shared.advertiser.lock().unwrap().take();
    if let Some(advertiser) = advertiser {
        advertiser.shutdown();
    }
    Ok(())
}

fn serve_connection(
    mut stream: TcpStream,
    shared: Arc<Shared>,
    shutdown: Shutdown,
    connection_id: u64,
) -> anyhow::Result<()> {
    let local_ip = stream.local_addr()?.ip();
    let (event_sender, event_receiver) = sync_channel(EVENT_QUEUE_CAPACITY);
    let _registration = ConnectionRegistration::new(shared.clone(), connection_id, event_sender);
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(CONNECTION_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.set_nodelay(true)?;
    let mut pair_setup = None;
    let mut pair_verify = None;
    let mut authenticated = None;
    let mut decoder: Option<RecordDecoder> = None;
    let mut encoder: Option<RecordEncoder> = None;
    let mut wire = Vec::new();
    let mut plaintext = Vec::new();
    let mut buffer = [0_u8; 8192];

    while !shutdown.is_cancelled() {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                tracing::trace!(connection_id, bytes = read, "HomeKit TCP bytes received");
                wire.extend_from_slice(&buffer[..read]);
                if wire.len() > MAX_CONNECTION_BYTES {
                    anyhow::bail!("HomeKit connection buffer exceeded limit");
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(error) => return Err(error.into()),
        }

        if let Some(record_decoder) = decoder.as_mut() {
            loop {
                match record_decoder.decode(&wire)? {
                    DecodeResult::NeedMore { .. } => break,
                    DecodeResult::Decoded {
                        plaintext: block,
                        consumed,
                    } => {
                        tracing::trace!(
                            connection_id,
                            encrypted_bytes = consumed,
                            plaintext_bytes = block.len(),
                            "HomeKit encrypted record decoded"
                        );
                        wire.drain(..consumed);
                        plaintext.extend_from_slice(&block);
                    }
                }
            }
        } else {
            plaintext.append(&mut wire);
        }

        while let HttpParseResult::Complete { request, consumed } = Request::parse(&plaintext)? {
            plaintext.drain(..consumed);
            tracing::debug!(
                connection_id,
                method = ?request.method,
                endpoint = ?request.endpoint,
                target = %request.target,
                body_bytes = request.body.len(),
                encrypted = encoder.is_some(),
                authenticated = authenticated.is_some(),
                "HomeKit HAP request received"
            );
            let dispatch = dispatch_request(
                request,
                &shared,
                &mut pair_setup,
                &mut pair_verify,
                authenticated.as_ref(),
                encoder.is_some(),
                connection_id,
                local_ip,
            )?;
            let close = dispatch.close;
            tracing::debug!(
                connection_id,
                close,
                establishes_encryption = dispatch.verified.is_some(),
                "HomeKit HAP request processed"
            );
            let encoded = dispatch.response.encode_with_connection_close(close);
            tracing::trace!(
                connection_id,
                bytes = encoded.len(),
                "HomeKit HAP response encoded"
            );
            if let Some(record_encoder) = encoder.as_mut() {
                stream.write_all(&record_encoder.encode(&encoded)?)?;
            } else {
                stream.write_all(&encoded)?;
            }
            if let Some((session_keys, controller)) = dispatch.verified {
                decoder = Some(session_keys.decoder());
                encoder = Some(session_keys.encoder());
                authenticated = Some(controller);
                if !plaintext.is_empty() || !wire.is_empty() {
                    anyhow::bail!("unexpected bytes at HAP encryption transition");
                }
            }
            if close {
                return Ok(());
            }
        }

        if let Some(record_encoder) = encoder.as_mut() {
            if let Err(error) = sync_all_webrtc_transports(&shared) {
                tracing::warn!(connection_id, %error, "unable to synchronize HomeKit transport events");
            }
            flush_connection_events(&mut stream, record_encoder, &event_receiver, connection_id)?;
        }
    }
    Ok(())
}

fn flush_connection_events(
    stream: &mut TcpStream,
    encoder: &mut RecordEncoder,
    receiver: &Receiver<CharacteristicEvent>,
    connection_id: u64,
) -> anyhow::Result<()> {
    let mut pending = HashMap::new();
    while let Ok(event) = receiver.try_recv() {
        pending.insert((event.aid, event.iid), event.value);
    }
    if pending.is_empty() {
        return Ok(());
    }
    let characteristics = pending
        .into_iter()
        .map(|((aid, iid), value)| serde_json::json!({ "aid": aid, "iid": iid, "value": value }))
        .collect::<Vec<_>>();
    let body = serde_json::to_vec(&serde_json::json!({
        "characteristics": characteristics,
    }))?;
    let plaintext = encode_event(&body);
    stream.write_all(&encoder.encode(&plaintext)?)?;
    tracing::debug!(
        connection_id,
        event_count = characteristics.len(),
        body_bytes = body.len(),
        "HomeKit characteristic event delivered"
    );
    Ok(())
}

fn publish_characteristic_event(
    shared: &Shared,
    aid: u64,
    iid: u64,
    value: serde_json::Value,
    exclude_connection: Option<u64>,
) {
    let event = CharacteristicEvent { aid, iid, value };
    let mut disconnected = Vec::new();
    let mut subscribers = shared.subscribers.lock().unwrap();
    for (connection_id, subscriber) in subscribers.iter() {
        if exclude_connection == Some(*connection_id)
            || !subscriber.characteristics.contains(&(aid, iid))
        {
            continue;
        }
        match subscriber.sender.try_send(event.clone()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                tracing::warn!(connection_id, aid, iid, "HomeKit event queue is full");
            }
            Err(TrySendError::Disconnected(_)) => disconnected.push(*connection_id),
        }
    }
    for connection_id in disconnected {
        subscribers.remove(&connection_id);
    }
}

fn advertised_name(name: &str, setup: &SetupPayload) -> String {
    let identifier = setup.accessory_id().to_string().replace(':', "");
    let suffix = &identifier[identifier.len() - 6..];
    let maximum_name_bytes = 63 - suffix.len() - 1;
    let mut end = name.len().min(maximum_name_bytes);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} {suffix}", &name[..end])
}

struct Dispatch {
    response: Response,
    verified: Option<(SessionKeys, ControllerPairing)>,
    close: bool,
}

#[allow(clippy::too_many_arguments)]
fn dispatch_request<'a>(
    request: Request,
    shared: &'a Shared,
    pair_setup: &mut Option<PairSetup<'a>>,
    pair_verify: &mut Option<PairVerify<'a>>,
    authenticated: Option<&ControllerPairing>,
    encrypted: bool,
    connection_id: u64,
    local_ip: IpAddr,
) -> anyhow::Result<Dispatch> {
    let mut verified = None;
    let mut close = false;
    let response = match (request.method, request.endpoint) {
        (Method::Post, Endpoint::PairSetup) if !encrypted => {
            handle_pair_setup(pair_setup, shared, &request.body)?
        }
        (Method::Post, Endpoint::PairVerify) if !encrypted => {
            let (response, keys) = handle_pair_verify(pair_verify, shared, &request.body)?;
            verified = keys;
            response
        }
        (Method::Post, Endpoint::Identify) if !shared.store.lock().unwrap().is_paired() => {
            Response::empty(Status::NoContent)
        }
        (_, Endpoint::Accessories) if authenticated.is_none() => unauthorized(),
        (Method::Get, Endpoint::Accessories) => {
            tracing::info!(
                bytes = shared.accessories.len(),
                "HomeKit accessory database served"
            );
            Response::new(Status::Ok, ContentType::HapJson, shared.accessories.clone())
        }
        (_, Endpoint::Resource) if authenticated.is_none() => unauthorized(),
        (Method::Post, Endpoint::Resource) => snapshot_resource(shared, &request.body)?,
        (_, Endpoint::Characteristics) if authenticated.is_none() => unauthorized(),
        (Method::Get, Endpoint::Characteristics) => read_characteristics(shared, &request.target)?,
        (Method::Put, Endpoint::Characteristics) => {
            write_characteristics_at(shared, &request.body, connection_id, local_ip)?
        }
        (_, Endpoint::Pairings) if authenticated.is_none() => unauthorized(),
        (Method::Post, Endpoint::Pairings) => {
            let result =
                handle_pairings(shared, authenticated.expect("checked above"), &request.body)?;
            close = result.close;
            result.response
        }
        _ => Response::empty(Status::NotFound),
    };
    Ok(Dispatch {
        response,
        verified,
        close,
    })
}

#[derive(Deserialize)]
struct SnapshotResourceRequest {
    #[serde(default = "default_snapshot_aid")]
    aid: u64,
    #[serde(rename = "resource-type")]
    resource_type: String,
    #[serde(rename = "image-width")]
    width: u32,
    #[serde(rename = "image-height")]
    height: u32,
}

const fn default_snapshot_aid() -> u64 {
    1
}

fn snapshot_resource(shared: &Shared, body: &[u8]) -> anyhow::Result<Response> {
    let Ok(request) = serde_json::from_slice::<SnapshotResourceRequest>(body) else {
        return Ok(Response::empty(Status::BadRequest));
    };
    let pixels = u64::from(request.width) * u64::from(request.height);
    if request.resource_type != "image"
        || !shared.camera_sources.contains_key(&request.aid)
        || pixels == 0
        || pixels > MAX_SNAPSHOT_PIXELS
    {
        return Ok(Response::empty(Status::BadRequest));
    }

    let image = vec![0_u8; pixels as usize * 3];
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 80).encode(
        &image,
        request.width,
        request.height,
        ColorType::Rgb8.into(),
    )?;
    tracing::info!(
        aid = request.aid,
        width = request.width,
        height = request.height,
        bytes = jpeg.len(),
        "HomeKit camera snapshot served"
    );
    Ok(Response::new(Status::Ok, ContentType::Jpeg, jpeg))
}

fn handle_pair_setup<'a>(
    machine: &mut Option<PairSetup<'a>>,
    shared: &'a Shared,
    body: &[u8],
) -> anyhow::Result<Response> {
    if machine.is_none() {
        let already_paired = shared.store.lock().unwrap().is_paired();
        *machine = Some(PairSetup::new(
            &shared.identity,
            &shared.setup_code.to_string(),
            rand::random(),
            rand::random(),
            already_paired,
        )?);
    }
    let state_machine = machine.as_mut().expect("initialized");
    state_machine.handle_input(PairSetupInput::Message(body))?;
    let mut response = None;
    loop {
        match state_machine.poll_output() {
            PairSetupOutput::Idle => break,
            PairSetupOutput::Response(value) => response = Some(value),
            PairSetupOutput::StorePairing { pairing } => {
                let stored = {
                    let mut store = shared.store.lock().unwrap();
                    if store.is_paired() {
                        Err(anyhow::anyhow!(
                            "accessory was paired by another connection"
                        ))
                    } else {
                        store.store_pairing(pairing)
                    }
                };
                let result = match stored {
                    Ok(()) => PairingStoreResult::Stored,
                    Err(error) => {
                        tracing::error!(%error, "unable to persist HomeKit pairing");
                        PairingStoreResult::Failed
                    }
                };
                state_machine.handle_input(PairSetupInput::PairingStored(result))?;
            }
            PairSetupOutput::Paired {
                response: value, ..
            } => {
                response = Some(value);
                tracing::info!("HomeKit Pair Setup completed");
                if let Err(error) = update_advertisement(shared) {
                    tracing::warn!(%error, "unable to refresh paired HomeKit advertisement");
                }
            }
        }
    }
    let complete = state_machine.state() == PairSetupState::Complete;
    let response = response.ok_or_else(|| anyhow::anyhow!("Pair Setup produced no response"))?;
    if complete {
        *machine = None;
    }
    Ok(Response::new(
        Status::Ok,
        ContentType::PairingTlv8,
        response,
    ))
}

fn handle_pair_verify<'a>(
    machine: &mut Option<PairVerify<'a>>,
    shared: &'a Shared,
    body: &[u8],
) -> anyhow::Result<(Response, Option<(SessionKeys, ControllerPairing)>)> {
    if machine.is_none() {
        *machine = Some(PairVerify::new(&shared.identity, rand::random()));
    }
    let state_machine = machine.as_mut().expect("initialized");
    state_machine.handle_input(PairVerifyInput::Message(body))?;
    let mut response = None;
    let mut verified = None;
    loop {
        match state_machine.poll_output() {
            PairVerifyOutput::Idle => break,
            PairVerifyOutput::Response(value) => response = Some(value),
            PairVerifyOutput::PairingRequired { identifier } => {
                let pairing = shared.store.lock().unwrap().pairing(&identifier);
                state_machine.handle_input(PairVerifyInput::Pairing(pairing.as_ref()))?;
            }
            PairVerifyOutput::Verified {
                response: value,
                session_keys,
                controller,
            } => {
                response = Some(value);
                verified = Some((session_keys, controller));
                tracing::info!("HomeKit Pair Verify completed");
            }
        }
    }
    let complete = state_machine.state() == PairVerifyState::Complete;
    let response = response.ok_or_else(|| anyhow::anyhow!("Pair Verify produced no response"))?;
    if complete {
        *machine = None;
    }
    Ok((
        Response::new(Status::Ok, ContentType::PairingTlv8, response),
        verified,
    ))
}

struct PairingsResult {
    response: Response,
    close: bool,
}

fn handle_pairings(
    shared: &Shared,
    controller: &ControllerPairing,
    body: &[u8],
) -> anyhow::Result<PairingsResult> {
    let mut machine = Pairings::new(controller.administrator);
    machine.handle_input(PairingsInput::Message(body))?;
    let mut response = None;
    loop {
        match machine.poll_output() {
            PairingsOutput::Idle => break,
            PairingsOutput::AddPairing { pairing } => {
                let result = {
                    let mut store = shared.store.lock().unwrap();
                    match store.pairing(&pairing.identifier) {
                        Some(existing) if existing.public_key != pairing.public_key => {
                            PairingsStoreResult::Failed
                        }
                        Some(_) => match store.store_pairing(pairing) {
                            Ok(()) => PairingsStoreResult::Stored,
                            Err(error) => {
                                tracing::error!(%error, "unable to update HomeKit pairing");
                                PairingsStoreResult::Failed
                            }
                        },
                        None if store.data.pairings.len() >= MAX_PAIRINGS => {
                            PairingsStoreResult::MaxPeers
                        }
                        None => match store.store_pairing(pairing) {
                            Ok(()) => PairingsStoreResult::Stored,
                            Err(error) => {
                                tracing::error!(%error, "unable to add HomeKit pairing");
                                PairingsStoreResult::Failed
                            }
                        },
                    }
                };
                machine.handle_input(PairingsInput::StoreResult(result))?;
            }
            PairingsOutput::RemovePairing { identifier } => {
                let removed = shared.store.lock().unwrap().remove_pairing(&identifier);
                let result = match removed {
                    Ok(()) => PairingsStoreResult::Stored,
                    Err(error) => {
                        tracing::error!(%error, "unable to remove HomeKit pairing");
                        PairingsStoreResult::Failed
                    }
                };
                machine.handle_input(PairingsInput::StoreResult(result))?;
                if let Err(error) = update_advertisement(shared) {
                    tracing::warn!(%error, "unable to refresh HomeKit advertisement");
                }
            }
            PairingsOutput::ListPairings => {
                let pairings = shared.store.lock().unwrap().pairings();
                machine.handle_input(PairingsInput::PairingList(&pairings))?;
            }
            PairingsOutput::Response(value) => response = Some(value),
        }
    }
    let close = shared
        .store
        .lock()
        .unwrap()
        .pairing(&controller.identifier)
        .is_none();
    Ok(PairingsResult {
        response: Response::new(
            Status::Ok,
            ContentType::PairingTlv8,
            response.ok_or_else(|| anyhow::anyhow!("pairings request produced no response"))?,
        ),
        close,
    })
}

fn update_advertisement(shared: &Shared) -> anyhow::Result<()> {
    let status = if shared.store.lock().unwrap().is_paired() {
        BonjourStatus::Paired
    } else {
        BonjourStatus::NotPaired
    };
    if let Some(advertiser) = shared.advertiser.lock().unwrap().as_mut() {
        advertiser.publish(status)?;
    }
    Ok(())
}

fn unauthorized() -> Response {
    Response::new(
        Status::ConnectionAuthorizationRequired,
        ContentType::HapJson,
        br#"{"status":-70401}"#.to_vec(),
    )
}

fn extract_characteristic_values(
    accessories: &[u8],
) -> anyhow::Result<HashMap<(u64, u64), serde_json::Value>> {
    let database: serde_json::Value = serde_json::from_slice(accessories)?;
    let mut values = HashMap::new();
    for accessory in database["accessories"].as_array().into_iter().flatten() {
        let Some(aid) = accessory["aid"].as_u64() else {
            continue;
        };
        for service in accessory["services"].as_array().into_iter().flatten() {
            for characteristic in service["characteristics"].as_array().into_iter().flatten() {
                let Some(iid) = characteristic["iid"].as_u64() else {
                    continue;
                };
                if let Some(value) = characteristic.get("value") {
                    values.insert((aid, iid), value.clone());
                }
            }
        }
    }
    Ok(values)
}

fn read_characteristics(shared: &Shared, target: &str) -> anyhow::Result<Response> {
    sync_all_webrtc_transports(shared)?;
    let ids = target
        .split_once('?')
        .map(|(_, query)| query)
        .and_then(|query| query.split('&').find_map(|part| part.strip_prefix("id=")))
        .ok_or_else(|| anyhow::anyhow!("characteristic request is missing ids"))?;
    let values = shared.characteristic_values.lock().unwrap();
    let characteristics = ids
        .split(',')
        .map(|id| {
            let (aid, iid) = id
                .split_once('.')
                .ok_or_else(|| anyhow::anyhow!("invalid characteristic id {id}"))?;
            let aid = aid.parse::<u64>()?;
            let iid = iid.parse::<u64>()?;
            Ok(values.get(&(aid, iid)).map_or_else(
                || serde_json::json!({ "aid": aid, "iid": iid, "status": -70409 }),
                |value| serde_json::json!({ "aid": aid, "iid": iid, "value": value }),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Response::new(
        Status::Ok,
        ContentType::HapJson,
        serde_json::to_vec(&serde_json::json!({ "characteristics": characteristics }))?,
    ))
}

#[cfg(test)]
fn write_characteristics(
    shared: &Shared,
    body: &[u8],
    connection_id: u64,
) -> anyhow::Result<Response> {
    write_characteristics_at(
        shared,
        body,
        connection_id,
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
    )
}

fn write_characteristics_at(
    shared: &Shared,
    body: &[u8],
    connection_id: u64,
    local_ip: IpAddr,
) -> anyhow::Result<Response> {
    let request: serde_json::Value = serde_json::from_slice(body)?;
    let writes = request["characteristics"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("characteristic write has no entries"))?;
    tracing::debug!(
        count = writes.len(),
        body_bytes = body.len(),
        "HomeKit characteristic writes received"
    );
    let mut failed = false;
    let mut has_write_response = false;
    let characteristics = writes
        .iter()
        .map(|write| {
            let result = apply_characteristic_write_at(shared, write, connection_id, local_ip)
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "HomeKit characteristic write failed");
                    CharacteristicWriteResult::error(HAP_SERVICE_COMMUNICATION_FAILURE)
                });
            let response_requested =
                write.get("r").and_then(serde_json::Value::as_bool) == Some(true);
            failed |= result.status != 0;
            has_write_response |=
                result.status == 0 && response_requested && result.value.is_some();
            let mut response = serde_json::json!({
                "aid": write["aid"],
                "iid": write["iid"],
                "status": result.status,
            });
            if response_requested
                && result.status == 0
                && let Some(value) = result.value
            {
                response["value"] = serde_json::Value::String(STANDARD.encode(value));
            }
            response
        })
        .collect::<Vec<_>>();
    if !failed && !has_write_response {
        return Ok(Response::empty(Status::NoContent));
    }
    Ok(Response::new(
        Status::MultiStatus,
        ContentType::HapJson,
        serde_json::to_vec(&serde_json::json!({ "characteristics": characteristics }))?,
    ))
}

struct CharacteristicWriteResult {
    status: i64,
    value: Option<Vec<u8>>,
}

impl CharacteristicWriteResult {
    const fn success() -> Self {
        Self {
            status: 0,
            value: None,
        }
    }

    const fn response(value: Vec<u8>) -> Self {
        Self {
            status: 0,
            value: Some(value),
        }
    }

    const fn error(status: i64) -> Self {
        Self {
            status,
            value: None,
        }
    }
}

#[cfg(test)]
fn apply_characteristic_write(
    shared: &Shared,
    write: &serde_json::Value,
    connection_id: u64,
) -> anyhow::Result<CharacteristicWriteResult> {
    apply_characteristic_write_at(
        shared,
        write,
        connection_id,
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
    )
}

fn apply_characteristic_write_at(
    shared: &Shared,
    write: &serde_json::Value,
    connection_id: u64,
    local_ip: IpAddr,
) -> anyhow::Result<CharacteristicWriteResult> {
    let (Some(aid), Some(iid)) = (write["aid"].as_u64(), write["iid"].as_u64()) else {
        return Ok(CharacteristicWriteResult::error(
            HAP_INVALID_VALUE_IN_REQUEST,
        ));
    };
    tracing::debug!(
        aid,
        iid,
        has_value = write.get("value").is_some(),
        has_event_subscription = write.get("ev").is_some(),
        "HomeKit characteristic write dispatching"
    );
    let mut has_operation = false;
    let mut result = if let Some(value) = write.get("value") {
        has_operation = true;
        match (aid, iid, value) {
            (1, 2, serde_json::Value::Bool(_)) => CharacteristicWriteResult::success(),
            (1, iid @ (9 | 23 | 24 | 27), value @ serde_json::Value::Bool(_)) => {
                let enabled = value.as_bool().expect("matched boolean");
                let mut devices = shared.webrtc_devices.lock().unwrap();
                let Some(device) = devices.get_mut(&aid) else {
                    return Ok(CharacteristicWriteResult::error(
                        HAP_INVALID_VALUE_IN_REQUEST,
                    ));
                };
                device.set_enabled(enabled);
                shared
                    .characteristic_values
                    .lock()
                    .unwrap()
                    .insert((aid, iid), value.clone());
                publish_characteristic_event(shared, aid, iid, value.clone(), Some(connection_id));
                CharacteristicWriteResult::success()
            }
            (1, 25, value @ serde_json::Value::Bool(_)) => {
                shared
                    .characteristic_values
                    .lock()
                    .unwrap()
                    .insert((aid, iid), value.clone());
                publish_characteristic_event(shared, aid, iid, value.clone(), Some(connection_id));
                CharacteristicWriteResult::success()
            }
            (1, 36, value @ serde_json::Value::Bool(_)) => {
                shared
                    .characteristic_values
                    .lock()
                    .unwrap()
                    .insert((aid, iid), value.clone());
                publish_characteristic_event(shared, aid, iid, value.clone(), Some(connection_id));
                CharacteristicWriteResult::success()
            }
            (aid, iid @ (44 | 52), serde_json::Value::String(value))
                if shared.probe_requests.is_some() =>
            {
                let value = STANDARD.decode(value)?;
                if value.is_empty() {
                    CharacteristicWriteResult::error(HAP_INVALID_VALUE_IN_REQUEST)
                } else {
                    report_probe_request(
                        shared,
                        aid,
                        HomeKitProbeRequestKind::LegacySetupEndpoints,
                    );
                    tracing::info!(
                        aid,
                        iid,
                        "HomeKit controller requested legacy Setup Endpoints"
                    );
                    CharacteristicWriteResult::success()
                }
            }
            (aid, iid @ (44 | 52), serde_json::Value::String(value)) => {
                apply_legacy_setup_write(shared, aid, iid, value, connection_id, local_ip)?
            }
            (aid, iid @ (43 | 51), serde_json::Value::String(value)) => {
                apply_legacy_selected_write(shared, aid, iid, value, connection_id)?
            }
            (1, iid @ (42 | 50 | 58 | 66 | 74), serde_json::Value::Number(value))
                if value.as_u64().is_some_and(|value| value <= 1) =>
            {
                let value = serde_json::Value::Number(value.clone());
                shared
                    .characteristic_values
                    .lock()
                    .unwrap()
                    .insert((aid, iid), value.clone());
                publish_characteristic_event(shared, aid, iid, value, Some(connection_id));
                CharacteristicWriteResult::success()
            }
            (_, 2 | 9 | 23 | 24 | 25 | 27 | 36 | 42 | 50 | 58 | 66 | 74, _) => {
                CharacteristicWriteResult::error(HAP_INVALID_VALUE_IN_REQUEST)
            }
            (aid, iid, serde_json::Value::String(value))
                if web_rtc_characteristic(iid).is_some() =>
            {
                let value = STANDARD.decode(value)?;
                apply_webrtc_write(shared, aid, iid, &value)?
            }
            _ => CharacteristicWriteResult::error(HAP_SERVICE_COMMUNICATION_FAILURE),
        }
    } else {
        CharacteristicWriteResult::success()
    };
    if result.status == 0
        && let Some(event_subscription) = write.get("ev")
    {
        has_operation = true;
        let Some(enabled) = event_subscription.as_bool() else {
            return Ok(CharacteristicWriteResult::error(
                HAP_INVALID_VALUE_IN_REQUEST,
            ));
        };
        if aid != 1 || !supports_events(iid) {
            return Ok(CharacteristicWriteResult::error(
                HAP_INVALID_VALUE_IN_REQUEST,
            ));
        }
        let mut subscribers = shared.subscribers.lock().unwrap();
        let Some(subscriber) = subscribers.get_mut(&connection_id) else {
            return Ok(CharacteristicWriteResult::error(
                HAP_SERVICE_COMMUNICATION_FAILURE,
            ));
        };
        if enabled {
            subscriber.characteristics.insert((aid, iid));
        } else {
            subscriber.characteristics.remove(&(aid, iid));
        }
        tracing::debug!(
            accessory = %shared.name,
            connection_id,
            aid,
            iid,
            enabled,
            "HomeKit characteristic event subscription changed"
        );
    }
    if !has_operation {
        result = CharacteristicWriteResult::error(HAP_INVALID_VALUE_IN_REQUEST);
    }
    Ok(result)
}

fn apply_legacy_setup_write(
    shared: &Shared,
    aid: u64,
    iid: u64,
    encoded: &str,
    connection_id: u64,
    local_ip: IpAddr,
) -> anyhow::Result<CharacteristicWriteResult> {
    if !shared.camera_sources.contains_key(&aid) {
        return Ok(CharacteristicWriteResult::error(
            HAP_INVALID_VALUE_IN_REQUEST,
        ));
    }
    let value = STANDARD.decode(encoded)?;
    let request = match decode_setup_endpoints(&value) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(aid, iid, %error, "invalid HomeKit Setup Endpoints request");
            return Ok(CharacteristicWriteResult::error(
                HAP_INVALID_VALUE_IN_REQUEST,
            ));
        }
    };
    let response = shared
        .legacy_rtp
        .lock()
        .unwrap()
        .prepare(iid, local_ip, request)?;
    shared.characteristic_values.lock().unwrap().insert(
        (aid, iid),
        serde_json::Value::String(STANDARD.encode(&response)),
    );
    set_legacy_streaming_status(shared, aid, iid, true, Some(connection_id));
    tracing::info!(aid, iid, %local_ip, "HomeKit legacy RTP endpoints prepared");
    Ok(CharacteristicWriteResult::response(response))
}

fn apply_legacy_selected_write(
    shared: &Shared,
    aid: u64,
    iid: u64,
    encoded: &str,
    connection_id: u64,
) -> anyhow::Result<CharacteristicWriteResult> {
    if !shared.camera_sources.contains_key(&aid) {
        return Ok(CharacteristicWriteResult::error(
            HAP_INVALID_VALUE_IN_REQUEST,
        ));
    }
    let value = STANDARD.decode(encoded)?;
    let configuration = match decode_selected_stream(&value) {
        Ok(configuration) => configuration,
        Err(error) => {
            tracing::warn!(aid, iid, %error, "invalid HomeKit Selected RTP Stream request");
            return Ok(CharacteristicWriteResult::error(
                HAP_INVALID_VALUE_IN_REQUEST,
            ));
        }
    };
    let in_use = shared
        .legacy_rtp
        .lock()
        .unwrap()
        .apply_selected(iid, configuration)?;
    shared
        .characteristic_values
        .lock()
        .unwrap()
        .insert((aid, iid), serde_json::Value::String(encoded.to_owned()));
    set_legacy_streaming_status(shared, aid, iid + 1, in_use, Some(connection_id));
    Ok(CharacteristicWriteResult::success())
}

fn set_legacy_streaming_status(
    shared: &Shared,
    aid: u64,
    setup_iid: u64,
    in_use: bool,
    exclude_connection: Option<u64>,
) {
    let status_iid = match setup_iid {
        44 => 38,
        52 => 46,
        _ => return,
    };
    let value = serde_json::Value::String(STANDARD.encode([1, 1, u8::from(in_use)]));
    shared
        .characteristic_values
        .lock()
        .unwrap()
        .insert((aid, status_iid), value.clone());
    publish_characteristic_event(shared, aid, status_iid, value, exclude_connection);
}

const fn supports_events(iid: u64) -> bool {
    matches!(
        iid,
        9 | 13 | 16 | 17 | 23 | 24 | 25 | 27
            ..=30 | 36 | 38 | 42 | 46 | 50 | 54 | 58 | 62 | 66 | 70 | 74
    )
}

const fn web_rtc_characteristic(iid: u64) -> Option<WebRtcCharacteristic> {
    match iid {
        10 => Some(WebRtcCharacteristic::SolicitOffer),
        11 => Some(WebRtcCharacteristic::ProvideAnswer),
        12 => Some(WebRtcCharacteristic::StreamingControl),
        14 => Some(WebRtcCharacteristic::Reoffer),
        15 => Some(WebRtcCharacteristic::UpdateSession),
        _ => None,
    }
}

fn apply_webrtc_write(
    shared: &Shared,
    aid: u64,
    iid: u64,
    value: &[u8],
) -> anyhow::Result<CharacteristicWriteResult> {
    let characteristic = web_rtc_characteristic(iid)
        .ok_or_else(|| anyhow::anyhow!("unknown WebRTC characteristic IID {iid}"))?;
    let request_id = WebRtcRequestId(shared.next_webrtc_request.fetch_add(1, Ordering::Relaxed));
    tracing::info!(
        aid,
        iid,
        characteristic = ?characteristic,
        request_id = ?request_id,
        tlv_bytes = value.len(),
        "HomeKit WebRTC characteristic write received"
    );
    let mut devices = shared.webrtc_devices.lock().unwrap();
    let device = devices
        .get_mut(&aid)
        .ok_or_else(|| anyhow::anyhow!("unknown HomeKit camera AID {aid}"))?;
    sync_webrtc_transport(shared, aid, device)?;
    let input = match characteristic {
        WebRtcCharacteristic::SolicitOffer => WebRtcInput::SolicitOffer {
            request_id,
            session_id: WebRtcSessionId::new(rand::random()),
            value,
        },
        WebRtcCharacteristic::ProvideAnswer => WebRtcInput::ProvideAnswer { request_id, value },
        WebRtcCharacteristic::StreamingControl => {
            WebRtcInput::StreamingControl { request_id, value }
        }
        WebRtcCharacteristic::Reoffer => WebRtcInput::Reoffer { request_id, value },
        WebRtcCharacteristic::UpdateSession => WebRtcInput::UpdateSession { request_id, value },
    };
    drive_webrtc_device(shared, aid, device, input).map(|response| {
        response.map_or_else(
            CharacteristicWriteResult::success,
            CharacteristicWriteResult::response,
        )
    })
}

enum OwnedWebRtcInput {
    OfferCreated {
        request_id: WebRtcRequestId,
        session_id: WebRtcSessionId,
        offer: Option<hap_video::OfferDescription>,
    },
    AnswerApplied {
        request_id: WebRtcRequestId,
        session_id: WebRtcSessionId,
        success: bool,
    },
    ReofferAnswered {
        request_id: WebRtcRequestId,
        session_id: WebRtcSessionId,
        answer: Option<String>,
    },
    TransportConnected(WebRtcSessionId),
    TransportClosed(WebRtcSessionId),
}

impl OwnedWebRtcInput {
    fn apply(self, device: &mut WebRtcDevice) -> Result<(), hap_video::Error> {
        match self {
            Self::OfferCreated {
                request_id,
                session_id,
                offer,
            } => device.handle_input(WebRtcInput::OfferCreated {
                request_id,
                session_id,
                offer,
            }),
            Self::AnswerApplied {
                request_id,
                session_id,
                success,
            } => device.handle_input(WebRtcInput::AnswerApplied {
                request_id,
                session_id,
                success,
            }),
            Self::ReofferAnswered {
                request_id,
                session_id,
                answer,
            } => device.handle_input(WebRtcInput::ReofferAnswered {
                request_id,
                session_id,
                answer,
            }),
            Self::TransportConnected(session_id) => {
                device.handle_input(WebRtcInput::TransportConnected { session_id })
            }
            Self::TransportClosed(session_id) => {
                device.handle_input(WebRtcInput::TransportClosed { session_id })
            }
        }
    }
}

fn drive_webrtc_device(
    shared: &Shared,
    aid: u64,
    device: &mut WebRtcDevice,
    input: WebRtcInput<'_>,
) -> anyhow::Result<Option<Vec<u8>>> {
    device.handle_input(input)?;
    let mut followups = VecDeque::new();
    let mut response = None;
    loop {
        loop {
            match device.poll_output() {
                WebRtcOutput::Idle => break,
                WebRtcOutput::WriteResponse(write_response) => {
                    tracing::debug!(
                        aid,
                        response_bytes = write_response.value.len(),
                        "HomeKit WebRTC write response produced"
                    );
                    response = Some(write_response.value);
                }
                WebRtcOutput::Event(WebRtcEvent::ActiveSessionsChanged(count)) => {
                    tracing::info!(
                        aid,
                        active_sessions = count,
                        "HomeKit camera active session count changed"
                    );
                    shared
                        .characteristic_values
                        .lock()
                        .unwrap()
                        .insert((aid, 13), serde_json::Value::from(count));
                    publish_characteristic_event(
                        shared,
                        aid,
                        13,
                        serde_json::Value::from(count),
                        None,
                    );
                }
                WebRtcOutput::Action(action) => {
                    if let Some(followup) = execute_webrtc_action(shared, aid, action) {
                        followups.push_back(followup);
                    }
                }
            }
        }
        let Some(followup) = followups.pop_front() else {
            return Ok(response);
        };
        followup.apply(device)?;
    }
}

fn execute_webrtc_action(
    shared: &Shared,
    aid: u64,
    action: WebRtcAction,
) -> Option<OwnedWebRtcInput> {
    match action {
        WebRtcAction::CreateOffer {
            request_id,
            session_id,
            ..
        } => {
            tracing::info!(
                aid,
                request_id = ?request_id,
                session_id = ?session_id,
                "HomeKit controller requested an SDP offer"
            );
            if shared.probe_requests.is_some() {
                report_probe_request(shared, aid, HomeKitProbeRequestKind::WebRtcSolicitOffer);
                return Some(OwnedWebRtcInput::OfferCreated {
                    request_id,
                    session_id,
                    offer: None,
                });
            }
            let offer = shared
                .camera_sources
                .get(&aid)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("camera source is unavailable"))
                .and_then(|source| {
                    tracing::debug!(aid, ?source, session_id = ?session_id, "creating HomeKit WebRTC transport");
                    shared.webrtc.create_homekit_offer(session_id, source)
                });
            match &offer {
                Ok(offer) => {
                    let path = save_homekit_offer(shared, &offer.sdp);
                    if let Err(error) = &path {
                        tracing::warn!(aid, request_id = ?request_id, session_id = ?session_id, %error, "unable to save HomeKit SDP offer");
                    }
                    tracing::info!(
                        aid,
                        request_id = ?request_id,
                        session_id = ?session_id,
                        sdp_bytes = offer.sdp.len(),
                        candidate_count = offer.candidates.len(),
                        path = ?path.as_ref().ok(),
                        "HomeKit SDP offer created for controller:\n{}",
                        offer.sdp
                    );
                }
                Err(error) => {
                    tracing::warn!(aid, request_id = ?request_id, session_id = ?session_id, %error, "unable to create HomeKit WebRTC offer");
                }
            }
            Some(OwnedWebRtcInput::OfferCreated {
                request_id,
                session_id,
                offer: offer.ok(),
            })
        }
        WebRtcAction::ApplyAnswer {
            request_id,
            session_id,
            sdp,
            candidates,
        } => {
            let path = save_homekit_answer(shared, &sdp);
            if let Err(error) = &path {
                tracing::warn!(aid, request_id = ?request_id, session_id = ?session_id, %error, "unable to save HomeKit controller SDP answer");
            }
            tracing::info!(
                aid,
                request_id = ?request_id,
                session_id = ?session_id,
                sdp_bytes = sdp.len(),
                candidate_count = candidates.len(),
                path = ?path.as_ref().ok(),
                "HomeKit controller SDP answer received:\n{}",
                sdp
            );
            let result = shared
                .webrtc
                .apply_homekit_answer(session_id, sdp, candidates);
            match &result {
                Ok(()) => tracing::info!(
                    aid,
                    request_id = ?request_id,
                    session_id = ?session_id,
                    "HomeKit controller SDP answer applied"
                ),
                Err(error) => {
                    tracing::warn!(aid, request_id = ?request_id, session_id = ?session_id, %error, "unable to apply HomeKit WebRTC answer");
                }
            }
            Some(OwnedWebRtcInput::AnswerApplied {
                request_id,
                session_id,
                success: result.is_ok(),
            })
        }
        WebRtcAction::AcceptReoffer {
            request_id,
            session_id,
            sdp,
        } => {
            tracing::info!(
                aid,
                request_id = ?request_id,
                session_id = ?session_id,
                sdp_bytes = sdp.len(),
                "HomeKit controller SDP reoffer received"
            );
            let answer = shared.webrtc.accept_homekit_reoffer(session_id, sdp);
            match &answer {
                Ok(answer) => tracing::info!(
                    aid,
                    request_id = ?request_id,
                    session_id = ?session_id,
                    sdp_bytes = answer.len(),
                    "HomeKit SDP reoffer answered"
                ),
                Err(error) => {
                    tracing::warn!(aid, request_id = ?request_id, session_id = ?session_id, %error, "unable to accept HomeKit WebRTC reoffer");
                }
            }
            Some(OwnedWebRtcInput::ReofferAnswered {
                request_id,
                session_id,
                answer: answer.ok(),
            })
        }
        WebRtcAction::EndSession { session_id } => {
            let closed = shared.webrtc.close_homekit_session(session_id);
            tracing::info!(aid, session_id = ?session_id, closed, "HomeKit WebRTC session end requested");
            None
        }
    }
}

fn report_probe_request(shared: &Shared, aid: u64, kind: HomeKitProbeRequestKind) {
    let Some(requests) = &shared.probe_requests else {
        return;
    };
    let Some(source) = shared.camera_sources.get(&aid) else {
        return;
    };
    let _ = requests.try_send(HomeKitProbeRequest {
        kind,
        camera_ip: source.camera_ip,
        name: shared.name.clone(),
    });
}

fn save_homekit_offer(shared: &Shared, sdp: &str) -> anyhow::Result<PathBuf> {
    save_homekit_sdp(shared, "last-offer.sdp", sdp)
}

fn save_homekit_answer(shared: &Shared, sdp: &str) -> anyhow::Result<PathBuf> {
    save_homekit_sdp(shared, "last-answer.sdp", sdp)
}

fn save_homekit_sdp(shared: &Shared, extension: &str, sdp: &str) -> anyhow::Result<PathBuf> {
    let path = shared
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .path
        .with_extension(extension);
    write_private_file_atomically(&path, sdp.as_bytes())?;
    Ok(path)
}

fn sync_all_webrtc_transports(shared: &Shared) -> anyhow::Result<()> {
    let mut devices = shared.webrtc_devices.lock().unwrap();
    for (aid, device) in devices.iter_mut() {
        sync_webrtc_transport(shared, *aid, device)?;
    }
    Ok(())
}

fn sync_webrtc_transport(
    shared: &Shared,
    aid: u64,
    device: &mut WebRtcDevice,
) -> anyhow::Result<()> {
    for session_id in device.session_ids() {
        let state = device.session_state(session_id);
        let transport = shared.webrtc.homekit_transport_state(session_id);
        let input = match (state, transport) {
            (Some(WebRtcSessionState::Connecting), Some(HomeKitTransportState::Connected)) => {
                Some(OwnedWebRtcInput::TransportConnected(session_id))
            }
            (_, Some(HomeKitTransportState::Closed) | None) => {
                Some(OwnedWebRtcInput::TransportClosed(session_id))
            }
            _ => None,
        };
        if let Some(input) = input {
            tracing::info!(
                aid,
                session_id = ?session_id,
                protocol_state = ?state,
                transport_state = ?transport,
                "HomeKit WebRTC protocol synchronized with transport"
            );
            drive_webrtc_device_owned(shared, aid, device, input)?;
        }
    }
    Ok(())
}

fn drive_webrtc_device_owned(
    shared: &Shared,
    aid: u64,
    device: &mut WebRtcDevice,
    input: OwnedWebRtcInput,
) -> anyhow::Result<()> {
    input.apply(device)?;
    loop {
        match device.poll_output() {
            WebRtcOutput::Idle => return Ok(()),
            WebRtcOutput::Event(WebRtcEvent::ActiveSessionsChanged(count)) => {
                tracing::info!(
                    aid,
                    active_sessions = count,
                    "HomeKit camera active session count changed"
                );
                shared
                    .characteristic_values
                    .lock()
                    .unwrap()
                    .insert((aid, 13), serde_json::Value::from(count));
                publish_characteristic_event(shared, aid, 13, serde_json::Value::from(count), None);
            }
            WebRtcOutput::Action(action) => {
                let _ = execute_webrtc_action(shared, aid, action);
            }
            WebRtcOutput::WriteResponse(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE_FILE: &str = "camera.json";
    const SETUP_FILE: &str = "camera-setup.txt";
    const SETUP_QR_FILE: &str = "camera-setup.svg";

    fn test_store(path: PathBuf) -> StateStore {
        StateStore::load_or_create(path, "test-camera-key", "test-camera", "Test Camera").unwrap()
    }

    fn test_cameras() -> HashMap<IpAddr, Camera> {
        let first = crate::cameras::CameraConfig {
            ip: "192.0.2.10".parse().unwrap(),
            name: Some("test-camera".to_owned()),
            display_name: Some("Test Camera".to_owned()),
            manufacturer: Some("KeepPeek".to_owned()),
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: Some("TESTCAMERA0001".to_owned()),
            backend: crate::cameras::CameraBackend::Auto,
            transport: crate::cameras::CameraTransport::Tcp,
        };
        let mut second = first.clone();
        second.ip = "192.0.2.11".parse().unwrap();
        second.name = Some("side-camera".to_owned());
        second.display_name = Some("Side Camera".to_owned());
        second.uid = Some("TESTCAMERA0002".to_owned());
        crate::cameras::configured_cameras(&HashMap::from([(
            "cameras".to_owned(),
            vec![first, second],
        )]))
    }

    fn first_tlv_value(input: &[u8], item_type: u8) -> Vec<u8> {
        let mut offset = 0;
        let mut value = Vec::new();
        let mut found = false;
        while offset + 2 <= input.len() {
            let current_type = input[offset];
            let length = usize::from(input[offset + 1]);
            offset += 2;
            if offset + length > input.len() {
                break;
            }
            if current_type == item_type {
                value.extend_from_slice(&input[offset..offset + length]);
                found = true;
            } else if found {
                break;
            }
            offset += length;
        }
        value
    }

    fn push_tlv(output: &mut Vec<u8>, item_type: u8, value: &[u8]) {
        if value.is_empty() {
            output.extend_from_slice(&[item_type, 0]);
            return;
        }
        for fragment in value.chunks(u8::MAX as usize) {
            output.push(item_type);
            output.push(fragment.len() as u8);
            output.extend_from_slice(fragment);
        }
    }

    fn test_shared(path: &Path) -> Arc<Shared> {
        test_shared_with_probe(path, None)
    }

    fn test_shared_with_probe(
        path: &Path,
        probe_requests: Option<SyncSender<HomeKitProbeRequest>>,
    ) -> Arc<Shared> {
        let store = test_store(path.to_path_buf());
        let setup = store.setup_payload().unwrap();
        Arc::new(Shared {
            name: "Test Camera".to_owned(),
            identity: AccessoryIdentity::new(
                setup.accessory_id().to_string().into_bytes(),
                store.data.signing_seed,
            )
            .unwrap(),
            setup_code: setup.code(),
            accessories: br#"{"accessories":[]}"#.to_vec(),
            characteristic_values: Mutex::new(HashMap::new()),
            store: Mutex::new(store),
            advertiser: Mutex::new(None),
            webrtc: WebRtc::new(),
            camera_sources: HashMap::from([(
                1,
                Source {
                    camera_ip: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    stream: StreamKind::Main,
                },
            )]),
            webrtc_devices: Mutex::new(HashMap::from([(1, WebRtcDevice::new())])),
            legacy_rtp: Mutex::new(legacy_rtp::LegacyRtpManager::new(
                PathBuf::from("ffmpeg"),
                None,
                false,
            )),
            subscribers: Mutex::new(HashMap::new()),
            next_connection: AtomicU64::new(1),
            next_webrtc_request: AtomicU64::new(1),
            probe_requests,
        })
    }

    #[test]
    fn persistent_identity_and_setup_material_survive_reload() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-homekit-state-{}", rand::random::<u64>()));
        let path = directory.join(STATE_FILE);
        let first = test_store(path.clone());
        let first_uri = first.setup_payload().unwrap().uri();
        let first_seed = first.data.signing_seed;
        drop(first);

        let second = test_store(path);
        assert_eq!(second.setup_payload().unwrap().uri(), first_uri);
        assert_eq!(second.data.signing_seed, first_seed);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn accessory_database_changes_increment_configuration_number() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-configuration-{}",
            rand::random::<u64>()
        ));
        let path = directory.join(STATE_FILE);
        let mut store = test_store(path);
        store.update_accessory_database(b"first");
        assert_eq!(store.data.configuration_number, 1);
        store.update_accessory_database(b"first");
        assert_eq!(store.data.configuration_number, 1);
        store.update_accessory_database(b"second");
        assert_eq!(store.data.configuration_number, 2);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn standalone_camera_pairings_and_identities_are_independent() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-independent-{}",
            rand::random::<u64>()
        ));
        let mut first = StateStore::load_or_create(
            directory.join("first.json"),
            "first-key",
            "first",
            "First Camera",
        )
        .unwrap();
        let second = StateStore::load_or_create(
            directory.join("second.json"),
            "second-key",
            "second",
            "Second Camera",
        )
        .unwrap();
        first
            .store_pairing(ControllerPairing {
                identifier: b"controller-one".to_vec(),
                public_key: [9; 32],
                administrator: true,
            })
            .unwrap();

        assert_ne!(
            first.setup_payload().unwrap().accessory_id(),
            second.setup_payload().unwrap().accessory_id()
        );
        assert!(first.is_paired());
        assert!(!second.is_paired());
        assert_eq!(first.pairings().len(), 1);
        assert!(second.pairings().is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pair_setup_m1_is_orchestrated_into_m2() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-homekit-pair-{}", rand::random::<u64>()));
        let shared = test_shared(&directory.join(STATE_FILE));
        let mut setup = None;
        let response = handle_pair_setup(&mut setup, &shared, &[0, 1, 0, 6, 1, 1]).unwrap();

        assert_eq!(response.status, Status::Ok);
        assert!(response.body.windows(3).any(|value| value == [6, 1, 2]));
        assert_eq!(setup.unwrap().state(), PairSetupState::AwaitingM3);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn accessories_require_pair_verify() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-homekit-auth-{}", rand::random::<u64>()));
        let shared = test_shared(&directory.join(STATE_FILE));
        let request = Request {
            method: Method::Get,
            target: "/accessories".to_owned(),
            endpoint: Endpoint::Accessories,
            body: Vec::new(),
        };
        let response = dispatch_request(
            request,
            &shared,
            &mut None,
            &mut None,
            None,
            false,
            1,
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        )
        .unwrap()
        .response;

        assert_eq!(response.status, Status::ConnectionAuthorizationRequired);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn authenticated_snapshot_request_returns_requested_jpeg_dimensions() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-snapshot-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        let request = Request {
            method: Method::Post,
            target: "/resource".to_owned(),
            endpoint: Endpoint::Resource,
            body: br#"{"resource-type":"image","image-width":320,"image-height":240}"#.to_vec(),
        };
        let controller = ControllerPairing {
            identifier: b"controller".to_vec(),
            public_key: [7; 32],
            administrator: true,
        };

        let response = dispatch_request(
            request,
            &shared,
            &mut None,
            &mut None,
            Some(&controller),
            true,
            1,
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        )
        .unwrap()
        .response;

        assert_eq!(response.status, Status::Ok);
        assert_eq!(response.content_type, Some(ContentType::Jpeg));
        let image = image::load_from_memory(&response.body).unwrap();
        assert_eq!((image.width(), image.height()), (320, 240));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn characteristic_reads_return_database_values_and_missing_statuses() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-characteristics-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        shared
            .characteristic_values
            .lock()
            .unwrap()
            .insert((1, 9), serde_json::Value::Bool(true));

        let response = read_characteristics(&shared, "/characteristics?id=1.9,1.99").unwrap();
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["characteristics"][0]["value"], true);
        assert_eq!(body["characteristics"][1]["status"], -70409);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn safe_characteristic_writes_succeed_before_transport_writes() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-homekit-writes-{}", rand::random::<u64>()));
        let shared = test_shared(&directory.join(STATE_FILE));
        let enabled = br#"{"characteristics":[{"aid":1,"iid":9,"value":false}]}"#;
        let response = write_characteristics(&shared, enabled, 1).unwrap();
        assert_eq!(response.status, Status::NoContent);
        assert_eq!(
            shared.characteristic_values.lock().unwrap().get(&(1, 9)),
            Some(&serde_json::Value::Bool(false))
        );

        let multi_tier_enabled = br#"{"characteristics":[{"aid":1,"iid":27,"value":true}]}"#;
        let response = write_characteristics(&shared, multi_tier_enabled, 1).unwrap();
        assert_eq!(response.status, Status::NoContent);
        assert_eq!(
            shared.characteristic_values.lock().unwrap().get(&(1, 27)),
            Some(&serde_json::Value::Bool(true))
        );

        let solicit = br#"{"characteristics":[{"aid":1,"iid":10,"value":""}]}"#;
        let response = write_characteristics(&shared, solicit, 1).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(response.status, Status::MultiStatus);
        assert_eq!(body["characteristics"][0]["status"], -70402);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn solicit_offer_returns_str0m_sdp_write_response() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-solicit-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        let options = STANDARD.encode([1, 3, 1, 1, 0]);
        let body = serde_json::to_vec(&serde_json::json!({
            "characteristics": [{ "aid": 1, "iid": 10, "value": options, "r": true }]
        }))
        .unwrap();

        let response = write_characteristics(&shared, &body, 1).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let encoded = body["characteristics"][0]["value"].as_str().unwrap();
        let tlv = STANDARD.decode(encoded).unwrap();
        assert_eq!(response.status, Status::MultiStatus);
        assert_eq!(body["characteristics"][0]["status"], 0);
        assert!(tlv.windows(5).any(|value| value == b"v=0\r\n"));

        let session_id: [u8; 16] = first_tlv_value(&tlv, 1).try_into().unwrap();
        let offer = String::from_utf8(first_tlv_value(&tlv, 2)).unwrap();
        let controller_candidate =
            str0m::Candidate::host("127.0.0.1:42000".parse().unwrap(), "udp").unwrap();
        let controller_rtc = crate::webrtc::rtc_config().build(std::time::Instant::now());
        let (_controller, answer) = hap_video::Str0mSession::accept_video_offer(
            controller_rtc,
            vec![controller_candidate],
            &offer,
        )
        .unwrap();
        let mut answer_tlv = Vec::new();
        push_tlv(&mut answer_tlv, 1, &session_id);
        push_tlv(&mut answer_tlv, 2, answer.as_bytes());
        let body = serde_json::to_vec(&serde_json::json!({
            "characteristics": [{
                "aid": 1,
                "iid": 11,
                "value": STANDARD.encode(answer_tlv),
                "r": true
            }]
        }))
        .unwrap();
        let response = write_characteristics(&shared, &body, 1).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let answer_response = STANDARD
            .decode(body["characteristics"][0]["value"].as_str().unwrap())
            .unwrap();
        assert_eq!(body["characteristics"][0]["status"], 0);
        assert_eq!(first_tlv_value(&answer_response, 2), [0]);

        let session_ids = shared.webrtc_devices.lock().unwrap()[&1].session_ids();
        assert_eq!(session_ids.len(), 1);
        assert_eq!(
            shared.webrtc.homekit_transport_state(session_ids[0]),
            Some(HomeKitTransportState::Connecting)
        );
        assert!(shared.webrtc.close_homekit_session(session_ids[0]));
        shared.webrtc.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn homekit_sdp_is_saved_beside_camera_state() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-sdp-offer-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        let offer = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=KeepPeek Offer\r\n";
        let answer = "v=0\r\no=- 2 2 IN IP4 127.0.0.1\r\ns=Controller Answer\r\n";

        let offer_path = save_homekit_offer(&shared, offer).unwrap();
        let answer_path = save_homekit_answer(&shared, answer).unwrap();

        assert_eq!(offer_path, directory.join("camera.last-offer.sdp"));
        assert_eq!(answer_path, directory.join("camera.last-answer.sdp"));
        assert_eq!(std::fs::read_to_string(offer_path).unwrap(), offer);
        assert_eq!(std::fs::read_to_string(answer_path).unwrap(), answer);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn offer_probe_reports_valid_request_without_creating_transport() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-offer-probe-{}",
            rand::random::<u64>()
        ));
        let (sender, receiver) = sync_channel(1);
        let shared = test_shared_with_probe(&directory.join(STATE_FILE), Some(sender));
        let session_id = WebRtcSessionId::new([9; 16]);

        let followup = execute_webrtc_action(
            &shared,
            1,
            WebRtcAction::CreateOffer {
                request_id: WebRtcRequestId(7),
                session_id,
                options: hap_video::OfferOptions {
                    sframe_enabled: false,
                },
            },
        );

        assert!(matches!(
            followup,
            Some(OwnedWebRtcInput::OfferCreated { offer: None, .. })
        ));
        assert_eq!(
            receiver.try_recv().unwrap(),
            HomeKitProbeRequest {
                kind: HomeKitProbeRequestKind::WebRtcSolicitOffer,
                camera_ip: "127.0.0.1".parse().unwrap(),
                name: "Test Camera".to_owned(),
            }
        );
        assert_eq!(shared.webrtc.homekit_transport_state(session_id), None);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_probe_reports_setup_endpoints_write() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-legacy-probe-{}",
            rand::random::<u64>()
        ));
        let (sender, receiver) = sync_channel(1);
        let shared = test_shared_with_probe(&directory.join(STATE_FILE), Some(sender));
        let write = serde_json::json!({
            "aid": 1,
            "iid": 44,
            "value": STANDARD.encode([1, 1, 0]),
        });

        let result = apply_characteristic_write(&shared, &write, 1).unwrap();

        assert_eq!(result.status, 0);
        assert_eq!(
            receiver.try_recv().unwrap(),
            HomeKitProbeRequest {
                kind: HomeKitProbeRequestKind::LegacySetupEndpoints,
                camera_ip: "127.0.0.1".parse().unwrap(),
                name: "Test Camera".to_owned(),
            }
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_setup_endpoints_returns_and_stores_response() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-legacy-setup-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        let mut address = Vec::new();
        push_tlv(&mut address, 1, &[0]);
        push_tlv(&mut address, 2, b"127.0.0.1");
        push_tlv(&mut address, 3, &50_000_u16.to_le_bytes());
        push_tlv(&mut address, 4, &50_001_u16.to_le_bytes());
        let mut video_srtp = Vec::new();
        push_tlv(&mut video_srtp, 1, &[0]);
        push_tlv(&mut video_srtp, 2, &[3; 16]);
        push_tlv(&mut video_srtp, 3, &[4; 14]);
        let mut audio_srtp = Vec::new();
        push_tlv(&mut audio_srtp, 1, &[0]);
        push_tlv(&mut audio_srtp, 2, &[5; 16]);
        push_tlv(&mut audio_srtp, 3, &[6; 14]);
        let mut request = Vec::new();
        push_tlv(&mut request, 1, &[7; 16]);
        push_tlv(&mut request, 3, &address);
        push_tlv(&mut request, 4, &video_srtp);
        push_tlv(&mut request, 5, &audio_srtp);
        let write = serde_json::json!({
            "aid": 1,
            "iid": 44,
            "value": STANDARD.encode(request),
        });

        let result = apply_characteristic_write(&shared, &write, 1).unwrap();

        assert_eq!(result.status, 0);
        let response = result.value.unwrap();
        assert_eq!(first_tlv_value(&response, 1), [7; 16]);
        assert_eq!(first_tlv_value(&response, 2), [0]);
        let response_address = first_tlv_value(&response, 3);
        assert_eq!(first_tlv_value(&response_address, 2), b"127.0.0.1");
        assert_ne!(first_tlv_value(&response_address, 3), [0, 0]);
        let values = shared.characteristic_values.lock().unwrap();
        assert_eq!(
            values[&(1, 44)],
            serde_json::Value::String(STANDARD.encode(&response))
        );
        assert_eq!(
            values[&(1, 38)],
            serde_json::Value::String(STANDARD.encode([1, 1, 1]))
        );
        drop(values);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn webrtc_write_response_requires_controller_request_flag() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-write-response-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        let options = STANDARD.encode([1, 3, 1, 1, 0]);
        let body = serde_json::to_vec(&serde_json::json!({
            "characteristics": [{ "aid": 1, "iid": 10, "value": options }]
        }))
        .unwrap();

        let response = write_characteristics(&shared, &body, 1).unwrap();
        assert_eq!(response.status, Status::NoContent);
        assert!(response.body.is_empty());

        let session_ids = shared.webrtc_devices.lock().unwrap()[&1].session_ids();
        assert_eq!(session_ids.len(), 1);
        assert!(shared.webrtc.close_homekit_session(session_ids[0]));
        shared.webrtc.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn event_subscriptions_are_scoped_to_each_hap_connection() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-subscriptions-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        let (first_sender, first_receiver) = sync_channel(EVENT_QUEUE_CAPACITY);
        let (second_sender, second_receiver) = sync_channel(EVENT_QUEUE_CAPACITY);
        let _first = ConnectionRegistration::new(shared.clone(), 11, first_sender);
        let _second = ConnectionRegistration::new(shared.clone(), 12, second_sender);
        let subscribe =
            br#"{"characteristics":[{"aid":1,"iid":13,"ev":true},{"aid":1,"iid":38,"ev":true}]}"#;

        let response = write_characteristics(&shared, subscribe, 11).unwrap();
        assert_eq!(response.status, Status::NoContent);
        publish_characteristic_event(&shared, 1, 13, serde_json::Value::from(2), None);
        publish_characteristic_event(&shared, 1, 38, serde_json::Value::from("AQEA"), None);

        let event = first_receiver.try_recv().unwrap();
        assert_eq!((event.aid, event.iid, event.value), (1, 13, 2.into()));
        let event = first_receiver.try_recv().unwrap();
        assert_eq!((event.aid, event.iid, event.value), (1, 38, "AQEA".into()));
        assert!(matches!(
            second_receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn setup_artifacts_contain_scannable_payload() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-homekit-setup-{}", rand::random::<u64>()));
        let store = test_store(directory.join(STATE_FILE));
        let setup = store.setup_payload().unwrap();
        write_setup_artifacts(&directory, SETUP_FILE, SETUP_QR_FILE, &setup).unwrap();

        let text = std::fs::read_to_string(directory.join(SETUP_FILE)).unwrap();
        let svg = std::fs::read_to_string(directory.join(SETUP_QR_FILE)).unwrap();
        assert!(text.contains(&setup.code().to_string()));
        assert!(text.contains(&setup.uri()));
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<svg"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn settings_snapshot_hides_setup_material_after_pairing() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-settings-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        let state_directory = directory.join(STATE_DIRECTORY);
        std::fs::create_dir_all(&state_directory).unwrap();
        let mut store = test_store(state_directory.join(STATE_FILE));
        let setup = store.setup_payload().unwrap();
        write_setup_artifacts(&state_directory, SETUP_FILE, SETUP_QR_FILE, &setup).unwrap();
        let index = PersistedAccessoryIndex {
            version: STATE_VERSION,
            accessories: vec![PersistedAccessoryIndexEntry {
                state_file: STATE_FILE.to_owned(),
                setup_qr_file: SETUP_QR_FILE.to_owned(),
            }],
        };
        write_private_file_atomically(
            &state_directory.join(STATE_INDEX_FILE),
            &serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        let config = HomeKitConfig {
            enabled: true,
            bind: IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            port: 32_000,
            name: "KeepPeek".to_owned(),
        };

        let unpaired = settings_snapshot(&config, &config_path, 3).unwrap();
        let expected_setup_code = setup.code().to_string();
        assert_eq!(unpaired.accessories.len(), 1);
        assert!(!unpaired.accessories[0].paired);
        assert_eq!(
            unpaired.accessories[0].setup_code.as_deref(),
            Some(expected_setup_code.as_str())
        );
        assert!(unpaired.accessories[0].setup_qr_svg_base64.is_some());

        store
            .store_pairing(ControllerPairing {
                identifier: b"controller".to_vec(),
                public_key: [7; 32],
                administrator: true,
            })
            .unwrap();
        let paired = settings_snapshot(&config, &config_path, 3).unwrap();
        assert!(paired.accessories[0].paired);
        assert_eq!(paired.accessories[0].pairing_count, 1);
        assert!(paired.accessories[0].setup_code.is_none());
        assert!(paired.accessories[0].setup_qr_svg_base64.is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reset_pairings_clears_only_selected_accessory() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-reset-pairings-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        let state_directory = directory.join(STATE_DIRECTORY);
        std::fs::create_dir_all(&state_directory).unwrap();
        let first_path = state_directory.join("first.json");
        let second_path = state_directory.join("second.json");
        let mut first =
            StateStore::load_or_create(first_path.clone(), "first", "192.0.2.10", "First").unwrap();
        let mut second =
            StateStore::load_or_create(second_path.clone(), "second", "192.0.2.11", "Second")
                .unwrap();
        let setup_code = first.data.setup_code.clone();
        for store in [&mut first, &mut second] {
            store
                .store_pairing(ControllerPairing {
                    identifier: b"controller".to_vec(),
                    public_key: [7; 32],
                    administrator: true,
                })
                .unwrap();
        }
        let index = PersistedAccessoryIndex {
            version: STATE_VERSION,
            accessories: vec![
                PersistedAccessoryIndexEntry {
                    state_file: "first.json".to_owned(),
                    setup_qr_file: "first-setup.svg".to_owned(),
                },
                PersistedAccessoryIndexEntry {
                    state_file: "second.json".to_owned(),
                    setup_qr_file: "second-setup.svg".to_owned(),
                },
            ],
        };
        write_private_file_atomically(
            &state_directory.join(STATE_INDEX_FILE),
            &serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();

        reset_pairings(&config_path, "192.0.2.10").unwrap();

        let first: PersistedState =
            serde_json::from_slice(&std::fs::read(first_path).unwrap()).unwrap();
        let second: PersistedState =
            serde_json::from_slice(&std::fs::read(second_path).unwrap()).unwrap();
        assert!(first.pairings.is_empty());
        assert_eq!(first.setup_code, setup_code);
        assert_eq!(second.pairings.len(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn tcp_runtime_starts_an_independent_session_for_each_connection() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-homekit-tcp-{}", rand::random::<u64>()));
        let shared = test_shared(&directory.join(STATE_FILE));
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = Shutdown::new();
        let runtime_shutdown = shutdown.clone();
        let runtime = std::thread::spawn(move || run(listener, shared, runtime_shutdown));
        let mut streams = [
            TcpStream::connect(address).unwrap(),
            TcpStream::connect(address).unwrap(),
        ];
        for stream in &mut streams {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(b"POST /identify HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            let mut response = [0_u8; 256];
            let read = stream.read(&mut response).unwrap();
            assert!(response[..read].starts_with(b"HTTP/1.1 204 No Content\r\n"));
        }
        shutdown.cancel();
        drop(streams);
        runtime.join().unwrap().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn service_starts_one_listener_and_identity_per_camera() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-service-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        let cameras = test_cameras();
        let shutdown = Shutdown::new();
        let service = HomeKitService::start(
            &HomeKitConfig {
                enabled: true,
                bind: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                port: 0,
                name: format!("KeepPeek Test {}", rand::random::<u32>()),
            },
            &config_path,
            &cameras,
            WebRtc::new(),
            shutdown.clone(),
        )
        .unwrap()
        .unwrap();
        let addresses = service.addresses().collect::<Vec<_>>();
        assert_eq!(addresses.len(), 2);
        assert_ne!(addresses[0], addresses[1]);
        let mut streams = Vec::new();
        for address in addresses {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(b"POST /identify HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            let mut response = [0_u8; 256];
            let read = stream.read(&mut response).unwrap();
            assert!(response[..read].starts_with(b"HTTP/1.1 204 No Content\r\n"));
            streams.push(stream);
        }
        let state_directory = directory.join(STATE_DIRECTORY);
        let index: PersistedAccessoryIndex =
            serde_json::from_slice(&std::fs::read(state_directory.join(STATE_INDEX_FILE)).unwrap())
                .unwrap();
        assert_eq!(index.version, STATE_VERSION);
        assert_eq!(index.accessories.len(), 2);
        let states = index
            .accessories
            .iter()
            .map(|entry| {
                assert!(state_directory.join(&entry.state_file).is_file());
                assert!(state_directory.join(&entry.setup_qr_file).is_file());
                serde_json::from_slice::<PersistedState>(
                    &std::fs::read(state_directory.join(&entry.state_file)).unwrap(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_ne!(states[0].accessory_id, states[1].accessory_id);
        let camera_ids = states
            .iter()
            .map(|state| state.camera_id.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(camera_ids, HashSet::from(["192.0.2.10", "192.0.2.11"]));
        shutdown.cancel();
        drop(streams);
        service.join();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn removing_authenticated_pairing_closes_connection() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-remove-pairing-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        let controller = ControllerPairing {
            identifier: b"controller".to_vec(),
            public_key: [7; 32],
            administrator: true,
        };
        shared
            .store
            .lock()
            .unwrap()
            .store_pairing(controller.clone())
            .unwrap();
        let mut body = vec![6, 1, 1, 0, 1, 4, 1, controller.identifier.len() as u8];
        body.extend_from_slice(&controller.identifier);

        let result = handle_pairings(&shared, &controller, &body).unwrap();
        assert_eq!(result.response.status, Status::Ok);
        assert!(result.close);
        assert!(!shared.store.lock().unwrap().is_paired());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn authenticated_administrator_can_pair_multiple_phones() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-multiple-phones-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        let administrator = ControllerPairing {
            identifier: b"administrator-phone".to_vec(),
            public_key: [1; 32],
            administrator: true,
        };
        shared
            .store
            .lock()
            .unwrap()
            .store_pairing(administrator.clone())
            .unwrap();

        for (identifier, public_key) in [
            (b"phone-two".as_slice(), [2; 32]),
            (b"phone-three".as_slice(), [3; 32]),
        ] {
            let mut body = vec![0, 1, 3, 1, identifier.len() as u8];
            body.extend_from_slice(identifier);
            body.extend_from_slice(&[3, 32]);
            body.extend_from_slice(&public_key);
            body.extend_from_slice(&[6, 1, 1, 11, 1, 0]);

            let result = handle_pairings(&shared, &administrator, &body).unwrap();
            assert_eq!(result.response.status, Status::Ok);
            assert!(!result.close);
        }

        let pairings = shared.store.lock().unwrap().pairings();
        assert_eq!(pairings.len(), 3);
        assert!(
            pairings
                .iter()
                .any(|pairing| pairing.identifier == b"phone-two")
        );
        assert!(
            pairings
                .iter()
                .any(|pairing| pairing.identifier == b"phone-three")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn adding_controller_at_capacity_returns_max_peers() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-homekit-pairing-cap-{}",
            rand::random::<u64>()
        ));
        let shared = test_shared(&directory.join(STATE_FILE));
        let controller = ControllerPairing {
            identifier: b"administrator".to_vec(),
            public_key: [7; 32],
            administrator: true,
        };
        {
            let mut store = shared.store.lock().unwrap();
            store.data.pairings.push(controller.clone().into());
            for index in 1..MAX_PAIRINGS {
                store.data.pairings.push(
                    ControllerPairing {
                        identifier: format!("controller-{index}").into_bytes(),
                        public_key: [index as u8; 32],
                        administrator: false,
                    }
                    .into(),
                );
            }
        }
        let identifier = b"one-too-many";
        let mut body = vec![0, 1, 3, 1, identifier.len() as u8];
        body.extend_from_slice(identifier);
        body.extend_from_slice(&[3, 32]);
        body.extend_from_slice(&[42; 32]);
        body.extend_from_slice(&[6, 1, 1, 11, 1, 0]);

        let result = handle_pairings(&shared, &controller, &body).unwrap();
        assert!(
            result
                .response
                .body
                .windows(3)
                .any(|value| value == [7, 1, 4])
        );
        assert_eq!(
            shared.store.lock().unwrap().data.pairings.len(),
            MAX_PAIRINGS
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
