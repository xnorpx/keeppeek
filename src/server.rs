use crate::{
    api::{
        ApiError, AudioProfileSummary, CameraId, CameraInfo, Health, MotionDetection,
        ProfileSummary, Ready, RecordingCapacityEstimate, RecordingEvent, RecordingEventsResponse,
        RecordingSegment, RecordingsResponse, SanitizedConfig, SanitizedStorage,
    },
    cameras::{
        Camera, CameraBackend, CameraConfig, CameraPorts, CameraTransport, reolink::ReolinkClient,
    },
    config::{self, Config, StorageMigration, StorageMigrationPaths, StorageToml},
    health::{
        CameraHealth, HealthIssue, HealthTotals, ServerHealthResponse, StorageHealth, SystemMonitor,
    },
    keeppeek::KeepPeekControl,
    logging::{LogStreamError, LoggingService},
    runtime::{
        FacadeSendError, FacadeSender, RouterError, RouterMessage, RouterQuery, RouterResponse,
    },
    shutdown::{Restart, Shutdown},
    stats::{HealthRegistry, REPORT_INTERVAL},
    storage::{EventStore, RecordingCatalogHandle, RecordingDemand, StorageConfig},
    webrtc::{
        BrowserSessionStatus, BrowserTrackPlan, LiveQuality, LiveSessionId, LiveSessionStatus,
        LiveTrackId, Source, WebRtc,
    },
};
use rouille::{Request, Response, ResponseBody, Server, router};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::File,
    net::{IpAddr, TcpListener, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock, mpsc},
    time::{Duration, Instant},
};
use url::Url;

const SERVER_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SERVER_SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(3);
const ROUTER_REPLY_TIMEOUT: Duration = Duration::from_secs(2);
const RECORDING_ACTIVITY_LEASE: Duration = Duration::from_secs(30);
const MAX_WRITE_BUFFER_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_LOG_SNAPSHOT_LIMIT: usize = 1_000;
const MAX_LOG_SNAPSHOT_LIMIT: usize = 10_000;
const DEFAULT_LOG_STREAM_TAIL: usize = 200;
const MAX_LOG_STREAM_TAIL: usize = 1_000;
const MEBIBYTE_BYTES: u64 = 1_048_576;
const GIBIBYTE_BYTES: u64 = 1_073_741_824;

#[derive(Deserialize)]
struct AdaptiveLiveOffer {
    offer: str0m::change::SdpOffer,
    #[serde(default)]
    quality: LiveQuality,
}

#[derive(Deserialize)]
struct BrowserLiveOffer {
    offer: str0m::change::SdpOffer,
    tracks: Vec<BrowserLiveTrackOffer>,
}

#[derive(Deserialize)]
struct BrowserLiveTrackOffer {
    track_id: String,
    camera_id: String,
    mid: String,
    #[serde(default)]
    quality: LiveQuality,
}

#[derive(Deserialize)]
struct LiveQualityUpdate {
    quality: LiveQuality,
}

#[derive(Deserialize)]
struct MotionDetectionUpdate {
    enabled: bool,
}

#[derive(Deserialize)]
struct LogFilterUpdate {
    filter: String,
}

#[derive(Deserialize)]
struct ManufacturerOverrideUpdate {
    manufacturer: Option<String>,
}

#[derive(Default, Deserialize)]
struct CameraDiscoveryRequest {
    #[serde(default)]
    subnets: Vec<u8>,
}

#[derive(Deserialize)]
struct CameraSettingsUpdate {
    display_name: Option<Option<String>>,
    manufacturer: Option<Option<String>>,
    username: Option<String>,
    password: Option<String>,
    onvif_port: Option<Option<u16>>,
    http_port: Option<Option<u16>>,
    #[serde(default, deserialize_with = "deserialize_optional_string_setting")]
    main_rtsp_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_string_setting")]
    sub_rtsp_url: Option<Option<String>>,
    uid: Option<Option<String>>,
    backend: Option<CameraBackend>,
    transport: Option<CameraTransport>,
}

#[derive(Deserialize)]
struct RuntimeSettingsUpdate {
    host: String,
    port: u16,
    storage: RuntimeStorageSettingsUpdate,
    #[serde(default)]
    move_existing_recordings: bool,
}

#[derive(Deserialize)]
struct RuntimeStorageSettingsUpdate {
    medium_term_path: String,
    long_term_path: String,
    recording_catalog_path: String,
    event_thumbnail_path: String,
    event_thumbnail_max_mb: u64,
    short_term_secs: u64,
    medium_term_secs: u64,
    flush_interval_secs: u64,
    write_buffer_bytes: usize,
    long_term_max_gb: u64,
}

#[derive(Serialize)]
struct CameraSettings {
    id: String,
    ip: String,
    display_name: Option<String>,
    manufacturer_override: Option<String>,
    username_configured: bool,
    password_configured: bool,
    onvif_port: Option<u16>,
    http_port: Option<u16>,
    main_rtsp_url: Option<String>,
    sub_rtsp_url: Option<String>,
    uid_configured: bool,
    backend: String,
    transport: String,
    health: Option<String>,
    model: Option<String>,
}

#[derive(Serialize)]
struct DiscoveredCameraSettings {
    ip: String,
    brand: String,
    name: Option<String>,
    model: Option<String>,
    onvif_port: Option<u16>,
    sources: Vec<String>,
    configured: bool,
    health: Option<String>,
}

#[derive(Serialize)]
struct CameraSettingsUpdateResponse {
    camera: CameraSettings,
    restart_required: bool,
}

#[derive(Serialize)]
struct RuntimeSettingsUpdateResponse {
    config: SanitizedConfig,
    restart_required: bool,
}

#[derive(Serialize)]
struct RestartResponse {
    restarting: bool,
}

#[derive(Serialize)]
struct AdaptiveLiveAnswer {
    session_id: LiveSessionId,
    answer: str0m::change::SdpAnswer,
    #[serde(flatten)]
    status: LiveSessionStatus,
}

#[derive(Serialize)]
struct BrowserLiveAnswer {
    session_id: LiveSessionId,
    answer: str0m::change::SdpAnswer,
    #[serde(flatten)]
    status: BrowserSessionStatus,
}

#[derive(Clone)]
struct CameraEntry {
    info: CameraInfo,
    reported_manufacturer: Option<String>,
    configuration: CameraConfig,
    recording_label: String,
    control: Option<CameraControl>,
}

#[derive(Clone)]
struct CameraControl {
    ip: IpAddr,
    username: String,
    password: String,
    http_port: Option<u16>,
}

#[derive(Clone)]
struct RestartControl {
    shutdown: Shutdown,
    restart: Restart,
}

fn camera_entry(camera_config: &CameraConfig, camera: Option<&Camera>) -> CameraEntry {
    let id = camera_config.ip.to_string();
    let reported_manufacturer = camera.and_then(|camera| camera.reported_manufacturer.clone());
    let mut ports = camera.map_or_else(
        || CameraPorts {
            onvif: camera_config.onvif_port,
            ..CameraPorts::default()
        },
        |camera| camera.ports.clone(),
    );
    ports.http = camera_config.http_port.or(ports.http);
    let control = (camera.is_some_and(|camera| camera.is_reolink)
        || camera_config.backend == CameraBackend::ReoProto)
        .then(|| CameraControl {
            ip: camera_config.ip,
            username: camera_config.username.clone(),
            password: camera_config.password.clone(),
            http_port: camera_config.http_port.or(ports.http),
        });
    let profiles = camera.map_or_else(
        || {
            ["main", "sub"]
                .into_iter()
                .map(|stream| ProfileSummary {
                    name: format!("{stream}Stream"),
                    stream: stream.to_owned(),
                    encoding: None,
                    resolution: None,
                    framerate: None,
                    bitrate_kbps: None,
                    gop: None,
                    h264_profile: None,
                    audio: None,
                })
                .collect()
        },
        |camera| {
            camera
                .profiles
                .iter()
                .enumerate()
                .map(|(index, profile)| ProfileSummary {
                    name: profile.name.clone(),
                    stream: if index == 0 { "main" } else { "sub" }.to_owned(),
                    encoding: profile
                        .video
                        .as_ref()
                        .map(|video| video.encoding.to_string()),
                    resolution: profile
                        .video
                        .as_ref()
                        .map(|video| format!("{}x{}", video.width, video.height)),
                    framerate: profile.video.as_ref().map(|video| video.framerate),
                    bitrate_kbps: profile.video.as_ref().and_then(|video| video.bitrate_kbps),
                    gop: profile.video.as_ref().and_then(|video| video.gov_length),
                    h264_profile: profile
                        .video
                        .as_ref()
                        .and_then(|video| video.h264_profile.clone()),
                    audio: profile.audio.as_ref().map(|audio| AudioProfileSummary {
                        encoding: audio.encoding.to_string(),
                        sample_rate: audio.sample_rate,
                        bitrate_kbps: audio.bitrate_kbps,
                    }),
                })
                .collect()
        },
    );

    CameraEntry {
        info: CameraInfo {
            id: id.clone(),
            ip: id,
            name: camera_config.display_name().map(str::to_owned),
            manufacturer: reported_manufacturer.clone(),
            model: camera.and_then(|camera| camera.device.model.clone()),
            firmware_version: camera.and_then(|camera| camera.device.firmware_version.clone()),
            serial_number: camera.and_then(|camera| camera.device.serial_number.clone()),
            hardware_id: camera.and_then(|camera| camera.device.hardware_id.clone()),
            hostname: camera.and_then(|camera| camera.hostname.clone()),
            mac_address: camera.and_then(|camera| camera.mac_address.clone()),
            is_reolink: camera.is_some_and(|camera| camera.is_reolink)
                || camera_config.backend == CameraBackend::ReoProto,
            backend: camera_backend_name(camera_config.backend).to_owned(),
            transport: camera_transport_name(camera_config.transport).to_owned(),
            web_url: camera_web_url(camera_config.ip, &ports),
            ports,
            capabilities: camera
                .map(|camera| camera.capabilities.clone())
                .unwrap_or_default(),
            profiles,
        },
        reported_manufacturer,
        configuration: camera_config.clone(),
        recording_label: camera_config
            .name
            .clone()
            .unwrap_or_else(|| camera_config.ip.to_string()),
        control,
    }
}

#[derive(Serialize)]
struct CameraDetails {
    camera: CameraInfo,
    health: Option<CameraHealth>,
    motion_detection: MotionDetection,
}

#[derive(Clone)]
pub struct ServerState {
    host: String,
    port: u16,
    cameras: Arc<RwLock<Vec<CameraEntry>>>,
    recordings: PathBuf,
    events: Option<EventStore>,
    recording_demand: RecordingDemand,
    config: SanitizedConfig,
    manufacturer_overrides: Arc<Mutex<HashMap<String, String>>>,
    camera_config_path: Option<PathBuf>,
    config_update: Arc<Mutex<()>>,
    restart_control: Option<RestartControl>,
    camera_runtime: Option<KeepPeekControl>,
    webrtc: WebRtc,
    health: HealthRegistry,
    system: Arc<Mutex<SystemMonitor>>,
    storage_config: StorageConfig,
    catalog: Option<RecordingCatalogHandle>,
    logging: Option<LoggingService>,
    started_at: Instant,
}

impl ServerState {
    pub fn new(
        config: &Config,
        camera_configs: &HashMap<String, Vec<CameraConfig>>,
        cameras: &HashMap<IpAddr, Camera>,
        storage: &StorageConfig,
        recording_demand: RecordingDemand,
        webrtc: WebRtc,
    ) -> Self {
        let mut configured = camera_configs.values().flatten().collect::<Vec<_>>();
        configured.sort_unstable_by_key(|camera| camera.ip);
        configured.dedup_by_key(|camera| camera.ip);
        let mut manufacturer_overrides = HashMap::new();
        for camera_config in &configured {
            if let Some(manufacturer) = camera_config.manufacturer_override() {
                manufacturer_overrides
                    .insert(camera_config.ip.to_string(), manufacturer.to_owned());
            }
        }
        let mut entries = configured
            .into_iter()
            .map(|camera_config| camera_entry(camera_config, cameras.get(&camera_config.ip)))
            .collect::<Vec<_>>();
        entries.sort_unstable_by(|left, right| left.info.id.cmp(&right.info.id));
        let camera_count = entries.len();
        let sanitized_config = sanitized_config(config, storage, camera_count, &entries);

        Self {
            host: config.host.clone(),
            port: config.port,
            cameras: Arc::new(RwLock::new(entries)),
            recordings: storage.long_term_path.clone(),
            events: None,
            recording_demand,
            config: sanitized_config,
            manufacturer_overrides: Arc::new(Mutex::new(manufacturer_overrides)),
            camera_config_path: None,
            config_update: Arc::new(Mutex::new(())),
            restart_control: None,
            camera_runtime: None,
            webrtc,
            health: HealthRegistry::new(),
            system: Arc::new(Mutex::new(SystemMonitor::new())),
            storage_config: storage.clone(),
            catalog: None,
            logging: None,
            started_at: Instant::now(),
        }
    }

    fn empty() -> Self {
        let config = Config::default();
        let storage = StorageConfig::default();
        Self::new(
            &config,
            &HashMap::new(),
            &HashMap::new(),
            &storage,
            RecordingDemand::new(RECORDING_ACTIVITY_LEASE),
            WebRtc::new(),
        )
    }

    fn camera_entries(&self) -> Vec<CameraEntry> {
        self.cameras
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn camera(&self, id: &str) -> Option<CameraEntry> {
        self.cameras
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|camera| camera.info.id == id)
            .cloned()
    }

    fn upsert_camera(&self, entry: CameraEntry) {
        let mut cameras = self
            .cameras
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = cameras
            .iter_mut()
            .find(|camera| camera.info.id == entry.info.id)
        {
            *existing = entry;
        } else {
            cameras.push(entry);
        }
        cameras.sort_unstable_by(|left, right| left.info.id.cmp(&right.info.id));
    }

    fn camera_info(&self, camera: &CameraEntry) -> CameraInfo {
        let mut info = camera.info.clone();
        let manufacturer_override = self
            .manufacturer_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&camera.info.id)
            .cloned();
        info.manufacturer = manufacturer_override.or_else(|| camera.reported_manufacturer.clone());
        info
    }

    pub fn with_camera_config_path(mut self, config_path: PathBuf) -> Self {
        self.camera_config_path = Some(config_path);
        self
    }

    pub fn with_restart_control(mut self, shutdown: Shutdown, restart: Restart) -> Self {
        self.restart_control = Some(RestartControl { shutdown, restart });
        self
    }

    pub fn with_camera_runtime(mut self, runtime: KeepPeekControl) -> Self {
        self.camera_runtime = Some(runtime);
        self
    }

    pub fn with_event_store(mut self, events: EventStore) -> Self {
        self.events = Some(events);
        self
    }

    pub fn with_health_registry(mut self, health: HealthRegistry) -> Self {
        self.health = health;
        self
    }

    pub fn with_recording_catalog(mut self, catalog: RecordingCatalogHandle) -> Self {
        self.catalog = Some(catalog);
        self
    }

    pub fn with_logging(mut self, logging: LoggingService) -> Self {
        self.logging = Some(logging);
        self
    }
}

fn sanitized_config(
    config: &Config,
    storage_config: &StorageConfig,
    camera_count: usize,
    cameras: &[CameraEntry],
) -> SanitizedConfig {
    SanitizedConfig {
        host: config.host.clone(),
        port: config.port,
        storage: SanitizedStorage {
            medium_term_path: storage_config
                .medium_term_path
                .to_string_lossy()
                .into_owned(),
            long_term_path: storage_config.long_term_path.to_string_lossy().into_owned(),
            recording_catalog_path: storage_config
                .recording_catalog_path
                .to_string_lossy()
                .into_owned(),
            event_thumbnail_path: storage_config
                .event_thumbnail_path
                .to_string_lossy()
                .into_owned(),
            event_thumbnail_max_mb: config.storage.event_thumbnail_max_mb,
            short_term_secs: config.storage.short_term_secs,
            medium_term_secs: config.storage.medium_term_secs,
            flush_interval_secs: config.storage.flush_interval_secs,
            write_buffer_bytes: config.storage.write_buffer_bytes,
            long_term_max_gb: config.storage.long_term_max_gb,
        },
        camera_count,
        recording_estimate: recording_capacity_estimate(
            cameras
                .iter()
                .flat_map(|camera| camera.info.profiles.iter()),
            storage_config.long_term_max_bytes,
        ),
    }
}

fn recording_capacity_estimate<'a>(
    profiles: impl IntoIterator<Item = &'a ProfileSummary>,
    long_term_max_bytes: u64,
) -> RecordingCapacityEstimate {
    const SECONDS_PER_DAY: u64 = 86_400;
    const BITS_PER_BYTE: u64 = 8;

    let mut estimated_bitrate_bps = 0_u64;
    let mut known_streams = 0;
    let mut unknown_streams = 0;
    for profile in profiles {
        match profile.bitrate_kbps {
            Some(bitrate_kbps) => {
                known_streams += 1;
                estimated_bitrate_bps = estimated_bitrate_bps
                    .saturating_add(u64::from(bitrate_kbps).saturating_mul(1_000));
            }
            None => unknown_streams += 1,
        }
        if let Some(audio_bitrate_kbps) =
            profile.audio.as_ref().and_then(|audio| audio.bitrate_kbps)
        {
            estimated_bitrate_bps = estimated_bitrate_bps
                .saturating_add(u64::from(audio_bitrate_kbps).saturating_mul(1_000));
        }
    }
    let bytes_per_day = estimated_bitrate_bps
        .saturating_mul(SECONDS_PER_DAY)
        .saturating_div(BITS_PER_BYTE);

    RecordingCapacityEstimate {
        estimated_bitrate_bps,
        bytes_per_day,
        known_streams,
        unknown_streams,
        estimated_retention_days: (long_term_max_bytes != 0 && bytes_per_day != 0)
            .then(|| long_term_max_bytes as f64 / bytes_per_day as f64),
    }
}

fn current_config(state: &ServerState) -> SanitizedConfig {
    let Some(path) = &state.camera_config_path else {
        return state.config.clone();
    };
    let loaded = (|| -> anyhow::Result<SanitizedConfig> {
        let config: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
        let camera_count = config::load_cameras(path)?.values().map(Vec::len).sum();
        let storage = StorageConfig::from_toml(&config.storage);
        Ok(sanitized_config(
            &config,
            &storage,
            camera_count,
            &state.camera_entries(),
        ))
    })();
    match loaded {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "unable to read persisted settings");
            state.config.clone()
        }
    }
}

pub fn serve_on_listener(
    listener: TcpListener,
    shutdown: Shutdown,
    router_tx: FacadeSender<RouterMessage>,
) -> anyhow::Result<std::net::SocketAddr> {
    serve_with_state_on_listener(listener, shutdown, router_tx, ServerState::empty())
}

pub fn serve_with_state_on_listener(
    listener: TcpListener,
    shutdown: Shutdown,
    router_tx: FacadeSender<RouterMessage>,
    state: ServerState,
) -> anyhow::Result<std::net::SocketAddr> {
    let logging = state.logging.clone();
    let server = Server::from_tcp_listener(listener, move |request| {
        handle_request(request, &router_tx, &state)
    })
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let addr = server.server_addr();
    tracing::info!(%addr, port = addr.port(), "KeepPeek is up and listening on http://{addr}");

    while !shutdown.is_cancelled() {
        server.poll_timeout(SERVER_POLL_INTERVAL);
    }
    server.poll_timeout(Duration::ZERO);
    if let Some(logging) = logging {
        logging.close_streams();
    }
    if !server.join_timeout(SERVER_SHUTDOWN_GRACE_PERIOD) {
        tracing::warn!(
            timeout_seconds = SERVER_SHUTDOWN_GRACE_PERIOD.as_secs(),
            "HTTP requests still active after shutdown grace period"
        );
    }

    Ok(addr)
}

pub fn run_server(
    state: ServerState,
    shutdown: Shutdown,
    router_tx: FacadeSender<RouterMessage>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(server_bind_address(&state.host, state.port))?;
    let _ = serve_with_state_on_listener(listener, shutdown, router_tx, state)?;
    Ok(())
}

fn handle_request(
    request: &Request,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> Response {
    router!(request,
        (GET) (/health) => {
            Response::json(&Health { status: "ok".to_owned() })
                .with_additional_header("Access-Control-Allow-Origin", "*")
        },
        (GET) (/ready) => {
            match query_router(router_tx, RouterQuery::ListCameras) {
                Ok(_) => Response::json(&Ready { ready: true }),
                Err(response) => response,
            }
        },
        (GET) (/api/v1/cameras) => {
            match query_router(router_tx, RouterQuery::ListCameras) {
                Ok(RouterResponse::Cameras(cameras)) => Response::json(&cameras),
                Ok(RouterResponse::Camera(_)) => internal_error("unexpected router response"),
                Err(response) => response,
            }
        },
        (GET) (/api/v1/cameras/{camera_id: String}) => {
            match query_router(router_tx, RouterQuery::GetCamera(CameraId::new(camera_id))) {
                Ok(RouterResponse::Camera(camera)) => Response::json(&camera),
                Ok(RouterResponse::Cameras(_)) => internal_error("unexpected router response"),
                Err(response) => response,
            }
        },
        (GET) (/api/cameras) => {
            Response::json(
                &state
                    .camera_entries()
                    .iter()
                    .map(|camera| state.camera_info(camera))
                    .collect::<Vec<_>>(),
            )
        },
        (GET) (/api/cameras/{camera_id: String}/details) => {
            camera_details(router_tx, state, &camera_id).map_or_else(
                || Response::text(format!("camera {camera_id} was not found")).with_status_code(404),
                |details| Response::json(&details),
            )
        },
        (GET) (/api/settings/cameras) => {
            Response::json(&camera_settings(router_tx, state))
        },
        (POST) (/api/settings/cameras/discover) => {
            discover_settings_cameras(request, router_tx, state)
        },
        (PUT) (/api/settings/cameras/{camera_id: String}) => {
            update_camera_settings(request, router_tx, state, &camera_id)
        },
        (DELETE) (/api/settings/cameras/{camera_id: String}) => {
            remove_camera_settings(state, &camera_id)
        },
        (PUT) (/api/settings/config) => {
            update_runtime_settings(request, state)
        },
        (GET) (/api/settings/logging) => {
            logging_settings(state)
        },
        (PUT) (/api/settings/logging) => {
            update_logging_settings(request, state)
        },
        (POST) (/api/settings/restart) => {
            restart_settings_server(state)
        },
        (PUT) (/api/cameras/{camera_id: String}/manufacturer) => {
            update_camera_manufacturer(request, state, &camera_id)
        },
        (POST) (/api/cameras/{camera_id: String}/motion) => {
            update_camera_motion(request, state, &camera_id)
        },
        (GET) (/api/config) => {
            Response::json(&current_config(state))
        },
        (GET) (/api/health) => {
            Response::json(&server_health(router_tx, state))
        },
        (GET) (/api/logs) => {
            log_snapshot(request, state)
        },
        (GET) (/api/logs/stream) => {
            log_stream(request, state)
        },
        (GET) (/api/recordings/{camera_id: String}) => {
            match list_recordings(state, &camera_id, request.get_param("date").as_deref()) {
                Ok(recordings) => Response::json(&recordings),
                Err(response) => response,
            }
        },
        (GET) (/api/events/{camera_id: String}) => {
            let Some(date) = request.get_param("date") else {
                return service_error(400, "event date is required");
            };
            match list_events(state, &camera_id, &date) {
                Ok(events) => Response::json(&events),
                Err(response) => response,
            }
        },
        (GET) (/api/events/{camera_id: String}/{event_id: String}/thumbnail) => {
            event_thumbnail(state, &camera_id, &event_id)
        },
        (POST) (/api/recordings/{camera_id: String}/{stream: String}/activity) => {
            recording_activity(state, &camera_id, &stream)
        },
        (GET) (/api/recordings/{camera_id: String}/{stream: String}/{date: String}/{hour: String}/{filename: String}) => {
            recording_file(request, state, &camera_id, &stream, &date, &hour, &filename)
        },
        (HEAD) (/api/recordings/{camera_id: String}/{stream: String}/{date: String}/{hour: String}/{filename: String}) => {
            recording_file(request, state, &camera_id, &stream, &date, &hour, &filename)
        },
        (POST) (/api/live/browser/offer) => {
            browser_webrtc_offer(request, state)
        },
        (GET) (/api/live/browser/{session_id: u64}) => {
            browser_live_session_status(state, LiveSessionId::from_u64(session_id))
        },
        (POST) (/api/live/browser/{session_id: u64}/tracks/{track_id: String}/quality) => {
            update_browser_track_quality(request, state, LiveSessionId::from_u64(session_id), &track_id)
        },
        (POST) (/api/live/browser/{session_id: u64}/close) => {
            close_browser_live_session(state, LiveSessionId::from_u64(session_id))
        },
        (POST) (/api/cameras/{camera_id: String}/live/offer) => {
            adaptive_webrtc_offer(request, state, &camera_id)
        },
        (GET) (/api/live/{session_id: u64}) => {
            live_session_status(state, LiveSessionId::from_u64(session_id))
        },
        (POST) (/api/live/{session_id: u64}/quality) => {
            update_live_quality(request, state, LiveSessionId::from_u64(session_id))
        },
        (POST) (/api/cameras/{camera_id: String}/live/{stream: String}/offer) => {
            webrtc_offer(request, state, &camera_id, &stream)
        },
        _ => serve_ui(request)
    )
}

fn logging_settings(state: &ServerState) -> Response {
    state.logging.as_ref().map_or_else(
        || service_error(503, "logging service is unavailable"),
        |logging| Response::json(&logging.settings()),
    )
}

fn update_logging_settings(request: &Request, state: &ServerState) -> Response {
    let Some(logging) = &state.logging else {
        return service_error(503, "logging service is unavailable");
    };
    let Some(body) = request.data() else {
        return service_error(400, "missing logging settings");
    };
    let update: LogFilterUpdate = match serde_json::from_reader(body) {
        Ok(update) => update,
        Err(error) => return service_error(400, &format!("invalid logging settings: {error}")),
    };
    if update.filter.trim().is_empty() {
        return service_error(400, "log filter must not be empty");
    }
    if let Err(error) = tracing_subscriber::EnvFilter::try_new(update.filter.trim()) {
        return service_error(400, &format!("invalid log filter: {error}"));
    }
    match logging.update_filter(&update.filter) {
        Ok(()) => Response::json(&logging.settings()),
        Err(error) => service_error(500, &format!("unable to update log filter: {error}")),
    }
}

fn log_snapshot(request: &Request, state: &ServerState) -> Response {
    let Some(logging) = &state.logging else {
        return service_error(503, "logging service is unavailable");
    };
    let after = match optional_query_u64(request, "after") {
        Ok(after) => after,
        Err(response) => return response,
    };
    let limit = match query_usize(
        request,
        "limit",
        DEFAULT_LOG_SNAPSHOT_LIMIT,
        MAX_LOG_SNAPSHOT_LIMIT,
    ) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    Response::json(&logging.snapshot(after, limit))
}

fn log_stream(request: &Request, state: &ServerState) -> Response {
    let Some(logging) = &state.logging else {
        return service_error(503, "logging service is unavailable");
    };
    let after = match optional_query_u64(request, "after") {
        Ok(Some(after)) => Some(after),
        Ok(None) => match request.header("Last-Event-ID") {
            Some(last_event_id) => match last_event_id.parse::<u64>() {
                Ok(last_event_id) => Some(last_event_id),
                Err(_) => return service_error(400, "Last-Event-ID must be an unsigned integer"),
            },
            None => None,
        },
        Err(response) => return response,
    };
    let tail = match query_usize(
        request,
        "tail",
        DEFAULT_LOG_STREAM_TAIL,
        MAX_LOG_STREAM_TAIL,
    ) {
        Ok(tail) => tail,
        Err(response) => return response,
    };
    let stream = match logging.stream(after, tail) {
        Ok(stream) => stream,
        Err(LogStreamError::LimitReached) => {
            return service_error(429, "too many active log streams");
        }
        Err(LogStreamError::Closed) => {
            return service_error(503, "log streaming is shutting down");
        }
    };
    Response {
        status_code: 200,
        headers: vec![
            (
                "Content-Type".into(),
                "text/event-stream; charset=utf-8".into(),
            ),
            ("Cache-Control".into(), "no-cache".into()),
            ("X-Accel-Buffering".into(), "no".into()),
        ],
        data: ResponseBody::from_reader(stream),
        upgrade: None,
    }
}

fn optional_query_u64(request: &Request, name: &str) -> Result<Option<u64>, Response> {
    request.get_param(name).map_or(Ok(None), |value| {
        value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| service_error(400, &format!("{name} must be an unsigned integer")))
    })
}

fn query_usize(
    request: &Request,
    name: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, Response> {
    let Some(value) = request.get_param(name) else {
        return Ok(default);
    };
    let value = value
        .parse::<usize>()
        .map_err(|_| service_error(400, &format!("{name} must be a positive integer")))?;
    if value == 0 || value > maximum {
        return Err(service_error(
            400,
            &format!("{name} must be between 1 and {maximum}"),
        ));
    }
    Ok(value)
}

fn server_health(
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> ServerHealthResponse {
    let uptime_seconds = state.started_at.elapsed().as_secs();
    let system = state
        .system
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .snapshot(&state.storage_config.long_term_path);
    let mut issues = Vec::new();
    let lifecycle = match query_router(router_tx, RouterQuery::ListCameras) {
        Ok(RouterResponse::Cameras(statuses)) => statuses
            .into_iter()
            .map(|status| (status.id.to_string(), status))
            .collect::<HashMap<_, _>>(),
        Ok(RouterResponse::Camera(_)) => {
            issues.push(HealthIssue {
                severity: "warning".to_owned(),
                scope: "runtime".to_owned(),
                message: "Router returned an unexpected camera response".to_owned(),
            });
            HashMap::new()
        }
        Err(_) => {
            issues.push(HealthIssue {
                severity: "warning".to_owned(),
                scope: "runtime".to_owned(),
                message: "Camera lifecycle router did not answer the health query".to_owned(),
            });
            HashMap::new()
        }
    };
    let mut ingress = state
        .health
        .snapshot()
        .into_iter()
        .map(|report| (report.ip, report))
        .collect::<HashMap<_, _>>();
    let configured_cameras = state.camera_entries();
    let mut totals = HealthTotals {
        configured_cameras: configured_cameras.len(),
        configured_video_streams: configured_cameras
            .iter()
            .map(|camera| camera.info.profiles.len())
            .sum(),
        ..HealthTotals::default()
    };
    let mut cameras = Vec::with_capacity(configured_cameras.len());

    for camera in &configured_cameras {
        let info = state.camera_info(camera);
        let ip = camera.info.ip.parse::<IpAddr>().ok();
        let report = ip.and_then(|ip| ingress.remove(&ip));
        let streams = report.map_or_else(Vec::new, |report| report.streams);
        let video_streams = streams
            .iter()
            .filter(|stream| stream.report.kind.starts_with("video_"))
            .collect::<Vec<_>>();
        if !video_streams.is_empty() {
            totals.reporting_cameras += 1;
        }
        totals.reporting_video_streams += video_streams.len();
        for stream in &video_streams {
            totals.ingress_fps += stream.report.fps;
            totals.ingress_bitrate_bps = totals
                .ingress_bitrate_bps
                .saturating_add((stream.report.kbps * 1_000.0).max(0.0) as u64);
            totals.frames = totals
                .frames
                .saturating_add(stream.report.frames.unwrap_or(0));
            totals.keyframes = totals
                .keyframes
                .saturating_add(stream.report.keyframes.unwrap_or(0));
            totals.drops = totals
                .drops
                .saturating_add(stream.report.drops.unwrap_or(0));
            totals.errors = totals
                .errors
                .saturating_add(stream.report.errors.unwrap_or(0));
            totals.reconnects = totals
                .reconnects
                .saturating_add(stream.report.reconnects.unwrap_or(0));
        }

        let state_name = if video_streams.is_empty() {
            if uptime_seconds < REPORT_INTERVAL.as_secs() * 2 {
                "starting"
            } else {
                issues.push(HealthIssue {
                    severity: "warning".to_owned(),
                    scope: camera
                        .info
                        .name
                        .clone()
                        .unwrap_or_else(|| camera.info.ip.clone()),
                    message: "No stream health report has been received".to_owned(),
                });
                "offline"
            }
        } else if video_streams
            .iter()
            .any(|stream| stream.report_age_ms > 30_000)
        {
            issues.push(HealthIssue {
                severity: "warning".to_owned(),
                scope: camera
                    .info
                    .name
                    .clone()
                    .unwrap_or_else(|| camera.info.ip.clone()),
                message: "One or more stream reports are stale".to_owned(),
            });
            "stale"
        } else if video_streams.iter().any(|stream| {
            stream.report.expected_fps > 0.0 && stream.report.fps < stream.report.expected_fps * 0.7
        }) {
            issues.push(HealthIssue {
                severity: "warning".to_owned(),
                scope: camera
                    .info
                    .name
                    .clone()
                    .unwrap_or_else(|| camera.info.ip.clone()),
                message: "Measured stream FPS is below 70% of the configured rate".to_owned(),
            });
            "degraded"
        } else {
            "online"
        };
        for stream in &video_streams {
            let expected_gap_ms =
                (stream.report.expected_fps > 0.0).then(|| 1_000.0 / stream.report.expected_fps);
            if stream.report.jitter_samples > 0
                && expected_gap_ms.is_some_and(|expected| stream.report.jitter_p99_ms > expected)
            {
                issues.push(HealthIssue {
                    severity: "info".to_owned(),
                    scope: camera
                        .info
                        .name
                        .clone()
                        .unwrap_or_else(|| camera.info.ip.clone()),
                    message: format!(
                        "{} frame-arrival jitter P99 is {:.1} ms",
                        stream.report.kind, stream.report.jitter_p99_ms
                    ),
                });
            }
            if stream.report.gap_max_ms > 2_000.0 {
                issues.push(HealthIssue {
                    severity: "warning".to_owned(),
                    scope: camera
                        .info
                        .name
                        .clone()
                        .unwrap_or_else(|| camera.info.ip.clone()),
                    message: format!(
                        "{} maximum frame gap is {:.0} ms",
                        stream.report.kind, stream.report.gap_max_ms
                    ),
                });
            }
            if stream.report.drops.unwrap_or(0) > 0 || stream.report.errors.unwrap_or(0) > 0 {
                issues.push(HealthIssue {
                    severity: "info".to_owned(),
                    scope: camera
                        .info
                        .name
                        .clone()
                        .unwrap_or_else(|| camera.info.ip.clone()),
                    message: format!(
                        "{} cumulative drops {}, errors {}",
                        stream.report.kind,
                        stream.report.drops.unwrap_or(0),
                        stream.report.errors.unwrap_or(0)
                    ),
                });
            }
        }

        let router_status = lifecycle.get(&camera.recording_label);
        cameras.push(CameraHealth {
            id: camera.info.id.clone(),
            ip: camera.info.ip.clone(),
            name: camera
                .info
                .name
                .clone()
                .unwrap_or_else(|| camera.info.ip.clone()),
            manufacturer: info.manufacturer,
            model: camera.info.model.clone(),
            firmware_version: camera.info.firmware_version.clone(),
            backend: camera.info.backend.clone(),
            transport: camera.info.transport.clone(),
            state: state_name.to_owned(),
            lifecycle: router_status.map(|status| format!("{:?}", status.lifecycle).to_lowercase()),
            last_error: router_status.and_then(|status| status.last_error.clone()),
            configured_profiles: camera.info.profiles.clone(),
            streams,
        });
    }

    let catalog = state
        .catalog
        .as_ref()
        .and_then(|catalog| match catalog.stats() {
            Ok(stats) => Some(stats),
            Err(error) => {
                issues.push(HealthIssue {
                    severity: "warning".to_owned(),
                    scope: "storage".to_owned(),
                    message: format!("Recording catalog statistics are unavailable: {error}"),
                });
                None
            }
        });
    let catalog_bytes = std::fs::metadata(&state.storage_config.recording_catalog_path)
        .ok()
        .map(|metadata| metadata.len());
    let storage = StorageHealth {
        medium_term_path: state
            .storage_config
            .medium_term_path
            .to_string_lossy()
            .into_owned(),
        long_term_path: state
            .storage_config
            .long_term_path
            .to_string_lossy()
            .into_owned(),
        paths_are_same: state.storage_config.medium_term_path
            == state.storage_config.long_term_path,
        short_term_seconds: state.storage_config.short_term_duration.as_secs(),
        medium_term_seconds: state.storage_config.medium_term_duration.as_secs(),
        flush_interval_seconds: state.storage_config.flush_interval.as_secs(),
        write_buffer_bytes: state.storage_config.write_buffer_bytes,
        long_term_max_bytes: state.storage_config.long_term_max_bytes,
        catalog_bytes,
        catalog,
        demand: state.recording_demand.health_snapshot(),
    };
    for disk in system.disks.iter().filter(|disk| disk.stores_recordings) {
        if disk.total_bytes > 0 && disk.available_bytes.saturating_mul(100) / disk.total_bytes < 10
        {
            issues.push(HealthIssue {
                severity: "critical".to_owned(),
                scope: "storage".to_owned(),
                message: format!(
                    "Recording disk {} has less than 10% free space",
                    disk.mount_point
                ),
            });
        }
    }
    if system.memory.total_bytes > 0
        && system.memory.available_bytes.saturating_mul(100) / system.memory.total_bytes < 5
    {
        issues.push(HealthIssue {
            severity: "critical".to_owned(),
            scope: "system".to_owned(),
            message: "System memory has less than 5% available".to_owned(),
        });
    }
    let webrtc = state.webrtc.health_snapshot();
    if webrtc.queue_drops > 0 {
        issues.push(HealthIssue {
            severity: "info".to_owned(),
            scope: "webrtc".to_owned(),
            message: format!(
                "{} WebRTC frames were dropped by full session queues; {} queued frames were discarded, including {} during keyframe recovery",
                webrtc.queue_drops,
                webrtc.queue_discarded_frames,
                webrtc.queue_recovery_drops
            ),
        });
    }
    let status = if issues
        .iter()
        .any(|issue| matches!(issue.severity.as_str(), "critical" | "warning"))
    {
        "degraded"
    } else {
        "healthy"
    };

    ServerHealthResponse {
        status: status.to_owned(),
        generated_at_ms: unix_time_ms(),
        uptime_seconds,
        version: env!("CARGO_PKG_VERSION"),
        totals,
        system,
        storage,
        webrtc,
        cameras,
        issues,
    }
}

fn camera_details(
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
    camera_id: &str,
) -> Option<CameraDetails> {
    let camera = state.camera(camera_id)?;
    let health = server_health(router_tx, state)
        .cameras
        .into_iter()
        .find(|health| health.id == camera_id);
    Some(CameraDetails {
        camera: state.camera_info(&camera),
        health,
        motion_detection: motion_detection_status(&camera),
    })
}

fn camera_settings(
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> Vec<CameraSettings> {
    let entries = state.camera_entries();
    let health = server_health(router_tx, state)
        .cameras
        .into_iter()
        .map(|camera| (camera.id, camera.state))
        .collect::<HashMap<_, _>>();
    let mut configured = state.camera_config_path.as_ref().map_or_else(
        || {
            entries
                .iter()
                .map(|camera| camera.configuration.clone())
                .collect::<Vec<_>>()
        },
        |config_path| match config::load_cameras(config_path) {
            Ok(configs) => configs.into_values().flatten().collect::<Vec<_>>(),
            Err(error) => {
                tracing::warn!(%error, path = %config_path.display(), "unable to read persisted camera settings");
                entries
                    .iter()
                    .map(|camera| camera.configuration.clone())
                    .collect()
            }
        },
    );
    configured.sort_unstable_by_key(|camera| camera.ip);
    configured.dedup_by_key(|camera| camera.ip);
    configured
        .iter()
        .map(|config| {
            entries
                .iter()
                .find(|camera| camera.info.ip == config.ip.to_string())
                .map_or_else(
                    || {
                        camera_settings_entry(
                            config,
                            None,
                            health.get(&config.ip.to_string()).cloned(),
                        )
                    },
                    |camera| {
                        camera_settings_entry(
                            config,
                            Some(camera),
                            health.get(&camera.info.id).cloned(),
                        )
                    },
                )
        })
        .collect()
}

fn camera_settings_entry(
    configuration: &CameraConfig,
    camera: Option<&CameraEntry>,
    health: Option<String>,
) -> CameraSettings {
    CameraSettings {
        id: configuration.ip.to_string(),
        ip: configuration.ip.to_string(),
        display_name: configuration.display_name.clone(),
        manufacturer_override: configuration.manufacturer_override().map(str::to_owned),
        username_configured: !configuration.username.is_empty(),
        password_configured: !configuration.password.is_empty(),
        onvif_port: configuration.onvif_port,
        http_port: configuration.http_port,
        main_rtsp_url: configuration.main_rtsp_url.clone(),
        sub_rtsp_url: configuration.sub_rtsp_url.clone(),
        uid_configured: configuration.uid.is_some(),
        backend: camera_backend_name(configuration.backend).to_owned(),
        transport: camera_transport_name(configuration.transport).to_owned(),
        health,
        model: camera.and_then(|camera| camera.info.model.clone()),
    }
}

fn discover_settings_cameras(
    request: &Request,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> Response {
    let request = request.data().map_or_else(
        || Ok(CameraDiscoveryRequest::default()),
        serde_json::from_reader,
    );
    let request: CameraDiscoveryRequest = match request {
        Ok(request) => request,
        Err(error) => return service_error(400, &format!("invalid discovery request: {error}")),
    };
    let mut subnets = request.subnets;
    subnets.sort_unstable();
    subnets.dedup();
    if subnets.len() > 32 {
        return service_error(400, "at most 32 additional subnets may be scanned at once");
    }
    let health = server_health(router_tx, state)
        .cameras
        .into_iter()
        .map(|camera| (camera.ip, camera.state))
        .collect::<HashMap<_, _>>();
    let configured = state
        .camera_entries()
        .into_iter()
        .map(|camera| camera.info.ip)
        .collect::<HashSet<_>>();
    let discovered = match crate::cameras::discover(Some(Duration::from_secs(3)), &subnets) {
        Ok(discovered) => discovered,
        Err(error) => return service_error(502, &format!("camera discovery failed: {error}")),
    };
    let cameras = discovered
        .into_iter()
        .map(|camera| {
            let ip = camera.ip.to_string();
            DiscoveredCameraSettings {
                onvif_port: camera.onvif_urls.iter().find_map(|url| {
                    matches!(url.scheme(), "http" | "https")
                        .then(|| url.port_or_known_default())
                        .flatten()
                }),
                brand: camera.brand.to_owned(),
                name: camera.name,
                model: camera.model,
                sources: camera.sources.into_iter().map(str::to_owned).collect(),
                configured: configured.contains(&ip),
                health: health.get(&ip).cloned(),
                ip,
            }
        })
        .collect::<Vec<_>>();
    Response::json(&cameras)
}

fn update_runtime_settings(request: &Request, state: &ServerState) -> Response {
    let Some(body) = request.data() else {
        return service_error(400, "missing runtime settings");
    };
    let update: RuntimeSettingsUpdate = match serde_json::from_reader(body) {
        Ok(update) => update,
        Err(error) => return service_error(400, &format!("invalid runtime settings: {error}")),
    };
    let Some(host) = normalize_server_host(&update.host) else {
        return service_error(400, "host must be a nonempty address or hostname");
    };
    if update.port == 0 {
        return service_error(400, "server port must be between 1 and 65535");
    }
    if server_bind_address(&host, update.port)
        .to_socket_addrs()
        .is_err()
    {
        return service_error(400, "host must resolve to an address");
    }
    if update.storage.write_buffer_bytes == 0
        || update.storage.write_buffer_bytes > MAX_WRITE_BUFFER_BYTES
    {
        return service_error(
            400,
            &format!("write buffer size must be between 1 and {MAX_WRITE_BUFFER_BYTES} bytes"),
        );
    }
    if update.storage.long_term_max_gb > u64::MAX / GIBIBYTE_BYTES {
        return service_error(400, "long-term storage limit is too large");
    }
    if update.storage.event_thumbnail_max_mb > u64::MAX / MEBIBYTE_BYTES {
        return service_error(400, "event thumbnail storage limit is too large");
    }
    let Some(medium_term_path) = normalize_storage_path(&update.storage.medium_term_path) else {
        return service_error(
            400,
            "medium-term storage path must be nonempty and cannot contain NUL",
        );
    };
    let Some(long_term_path) = normalize_storage_path(&update.storage.long_term_path) else {
        return service_error(
            400,
            "long-term storage path must be nonempty and cannot contain NUL",
        );
    };
    let Some(recording_catalog_path) =
        normalize_storage_path(&update.storage.recording_catalog_path)
    else {
        return service_error(
            400,
            "recording metadata database path must be nonempty and cannot contain NUL",
        );
    };
    let Some(event_thumbnail_path) = normalize_storage_path(&update.storage.event_thumbnail_path)
    else {
        return service_error(
            400,
            "event thumbnail storage path must be nonempty and cannot contain NUL",
        );
    };
    let recording_catalog_is_default = state.storage_config.recording_catalog_path
        == state.storage_config.long_term_path.join("recordings.db");
    let event_thumbnail_is_default = state.storage_config.event_thumbnail_path
        == state
            .storage_config
            .long_term_path
            .join(".event-thumbnails");
    let recording_catalog_path = (!recording_catalog_is_default
        || Path::new(&recording_catalog_path) != state.storage_config.recording_catalog_path)
        .then_some(recording_catalog_path);
    let event_thumbnail_path = (!event_thumbnail_is_default
        || Path::new(&event_thumbnail_path) != state.storage_config.event_thumbnail_path)
        .then_some(event_thumbnail_path);
    let Some(config_path) = &state.camera_config_path else {
        return service_error(409, "settings persistence is unavailable");
    };
    let settings = Config {
        host,
        port: update.port,
        storage: StorageToml {
            medium_term_path: Some(medium_term_path),
            long_term_path: Some(long_term_path),
            recording_catalog_path,
            event_thumbnail_path,
            event_thumbnail_max_mb: update.storage.event_thumbnail_max_mb,
            short_term_secs: update.storage.short_term_secs,
            medium_term_secs: update.storage.medium_term_secs,
            flush_interval_secs: update.storage.flush_interval_secs,
            write_buffer_bytes: update.storage.write_buffer_bytes,
            long_term_max_gb: update.storage.long_term_max_gb,
        },
        battery_wake: Default::default(),
    };
    let next_storage_config = StorageConfig::from_toml(&settings.storage);
    let migration = if update.move_existing_recordings {
        match StorageMigration::between_with_metadata(
            StorageMigrationPaths::new(
                &state.storage_config.medium_term_path,
                &state.storage_config.long_term_path,
                &state.storage_config.recording_catalog_path,
                &state.storage_config.event_thumbnail_path,
            ),
            StorageMigrationPaths::new(
                &next_storage_config.medium_term_path,
                &next_storage_config.long_term_path,
                &next_storage_config.recording_catalog_path,
                &next_storage_config.event_thumbnail_path,
            ),
        ) {
            Ok(migration) => migration,
            Err(error) => {
                return service_error(400, &format!("invalid storage migration: {error}"));
            }
        }
    } else {
        None
    };
    let _config_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let saved =
        match config::update_settings_with_migration(config_path, &settings, migration.as_ref()) {
            Ok(saved) => saved,
            Err(error) => return service_error(500, &format!("unable to save settings: {error}")),
        };
    let camera_count = config::load_cameras(config_path)
        .map(|cameras| cameras.values().map(Vec::len).sum())
        .unwrap_or(state.config.camera_count);
    let storage = StorageConfig::from_toml(&saved.storage);
    Response::json(&RuntimeSettingsUpdateResponse {
        config: sanitized_config(&saved, &storage, camera_count, &state.camera_entries()),
        restart_required: true,
    })
}

fn update_camera_settings(
    request: &Request,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
    camera_id: &str,
) -> Response {
    let Ok(ip) = camera_id.parse::<IpAddr>() else {
        return service_error(400, "camera ID must be an IP address");
    };
    let Some(body) = request.data() else {
        return service_error(400, "missing camera configuration");
    };
    let update: CameraSettingsUpdate = match serde_json::from_reader(body) {
        Ok(update) => update,
        Err(error) => return service_error(400, &format!("invalid camera configuration: {error}")),
    };
    if update.onvif_port == Some(Some(0)) || update.http_port == Some(Some(0)) {
        return service_error(400, "camera ports must be between 1 and 65535");
    }
    let Some(config_path) = &state.camera_config_path else {
        return service_error(409, "camera configuration persistence is unavailable");
    };
    let _config_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let persisted = match config::load_cameras(config_path) {
        Ok(cameras) => cameras
            .into_values()
            .flatten()
            .find(|camera| camera.ip == ip),
        Err(error) => {
            return service_error(
                500,
                &format!("unable to load camera configuration: {error}"),
            );
        }
    };
    let existing = state.camera(camera_id);
    let existing_config = existing
        .as_ref()
        .map(|camera| camera.configuration.clone())
        .or(persisted);
    let is_new_camera = existing_config.is_none();
    let username = nonempty_setting(update.username).or_else(|| {
        existing_config
            .as_ref()
            .map(|camera| camera.username.clone())
    });
    let password = nonempty_setting(update.password).or_else(|| {
        existing_config
            .as_ref()
            .map(|camera| camera.password.clone())
    });
    let (Some(username), Some(password)) = (username, password) else {
        return service_error(400, "username and password are required for a new camera");
    };
    let display_name = update.display_name.map_or_else(
        || {
            existing_config
                .as_ref()
                .and_then(|camera| camera.display_name.clone())
        },
        |display_name| display_name.and_then(|display_name| normalize_display_name(&display_name)),
    );
    let manufacturer = match update.manufacturer {
        Some(Some(manufacturer)) => {
            let normalized = normalize_manufacturer(&manufacturer);
            if normalized.is_none() && !manufacturer.trim().is_empty() {
                return service_error(400, "manufacturer must be at most 120 printable characters");
            }
            normalized
        }
        Some(None) => None,
        None => existing_config
            .as_ref()
            .and_then(|camera| camera.manufacturer.clone()),
    };
    let main_rtsp_url = match update.main_rtsp_url {
        Some(url) => match normalize_rtsp_url(url) {
            Ok(url) => url,
            Err(error) => return service_error(400, &format!("main RTSP URL {error}")),
        },
        None => existing_config
            .as_ref()
            .and_then(|camera| camera.main_rtsp_url.clone()),
    };
    let sub_rtsp_url = match update.sub_rtsp_url {
        Some(url) => match normalize_rtsp_url(url) {
            Ok(url) => url,
            Err(error) => return service_error(400, &format!("sub RTSP URL {error}")),
        },
        None => existing_config
            .as_ref()
            .and_then(|camera| camera.sub_rtsp_url.clone()),
    };
    let mut config = CameraConfig {
        ip,
        name: existing_config
            .as_ref()
            .and_then(|camera| camera.name.clone()),
        display_name,
        manufacturer,
        username,
        password,
        onvif_port: update.onvif_port.unwrap_or_else(|| {
            existing_config
                .as_ref()
                .and_then(|camera| camera.onvif_port)
        }),
        http_port: update
            .http_port
            .unwrap_or_else(|| existing_config.as_ref().and_then(|camera| camera.http_port)),
        main_rtsp_url,
        sub_rtsp_url,
        uid: update.uid.map_or_else(
            || {
                existing_config
                    .as_ref()
                    .and_then(|camera| camera.uid.clone())
            },
            |uid| uid.and_then(|uid| nonempty_setting(Some(uid))),
        ),
        backend: update
            .backend
            .or_else(|| existing_config.as_ref().map(|camera| camera.backend))
            .unwrap_or_default(),
        transport: update
            .transport
            .or_else(|| existing_config.as_ref().map(|camera| camera.transport))
            .unwrap_or_default(),
    };
    if let Err(error) = config::upsert_camera(config_path, &config) {
        return service_error(
            500,
            &format!("unable to save camera configuration: {error}"),
        );
    }
    let started_config = is_new_camera
        .then(|| start_runtime_camera(state, &config))
        .flatten();
    let dynamically_started = started_config.is_some();
    if let Some(started_config) = started_config {
        config = started_config;
    }
    let health = server_health(router_tx, state)
        .cameras
        .into_iter()
        .find(|camera| camera.ip == config.ip.to_string())
        .map(|camera| camera.state);
    let camera = CameraSettings {
        id: config.ip.to_string(),
        ip: config.ip.to_string(),
        display_name: config.display_name.clone(),
        manufacturer_override: config.manufacturer_override().map(str::to_owned),
        username_configured: true,
        password_configured: true,
        onvif_port: config.onvif_port,
        http_port: config.http_port,
        main_rtsp_url: config.main_rtsp_url.clone(),
        sub_rtsp_url: config.sub_rtsp_url.clone(),
        uid_configured: config.uid.is_some(),
        backend: camera_backend_name(config.backend).to_owned(),
        transport: camera_transport_name(config.transport).to_owned(),
        health,
        model: existing
            .as_ref()
            .and_then(|camera| camera.info.model.clone()),
    };
    Response::json(&CameraSettingsUpdateResponse {
        camera,
        restart_required: !dynamically_started,
    })
}

fn start_runtime_camera(state: &ServerState, config: &CameraConfig) -> Option<CameraConfig> {
    let Some(runtime) = &state.camera_runtime else {
        return None;
    };
    let configured = HashMap::from([("runtime".to_owned(), vec![config.clone()])]);
    let mut cameras = if config.backend == CameraBackend::ReoProto || config.has_manual_rtsp_urls()
    {
        crate::cameras::configured_cameras(&configured)
    } else {
        crate::cameras::query_cameras(&configured)
    };
    let Some(camera) = cameras.remove(&config.ip) else {
        tracing::warn!(ip = %config.ip, "new camera could not be prepared for a live start");
        return None;
    };
    let Some(config_path) = &state.camera_config_path else {
        tracing::warn!(ip = %config.ip, "new camera configuration cannot be persisted");
        return None;
    };
    if let Err(error) = config::upsert_camera(config_path, &camera.config) {
        tracing::warn!(ip = %config.ip, %error, "discovered camera endpoints could not be persisted");
        return None;
    }
    if let Err(error) = runtime.start_camera(camera.clone()) {
        tracing::warn!(ip = %config.ip, %error, "new camera could not be started live");
        return None;
    }
    state.upsert_camera(camera_entry(&camera.config, Some(&camera)));
    Some(camera.config)
}

fn remove_camera_settings(state: &ServerState, camera_id: &str) -> Response {
    let Ok(ip) = camera_id.parse::<IpAddr>() else {
        return service_error(400, "camera ID must be an IP address");
    };
    let Some(config_path) = &state.camera_config_path else {
        return service_error(409, "camera configuration persistence is unavailable");
    };
    let _config_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match config::remove_camera(config_path, ip) {
        Ok(()) => Response::empty_204(),
        Err(error) => service_error(
            500,
            &format!("unable to remove camera configuration: {error}"),
        ),
    }
}

fn restart_settings_server(state: &ServerState) -> Response {
    let Some(control) = &state.restart_control else {
        return service_error(409, "server restart is unavailable");
    };
    control.restart.request();
    control.shutdown.cancel();
    Response::json(&RestartResponse { restarting: true })
}

fn nonempty_setting(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn deserialize_optional_string_setting<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

fn normalize_display_name(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.chars().any(char::is_control)).then(|| value.to_owned())
}

fn normalize_server_host(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 255
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace()))
    .then(|| value.to_owned())
}

fn normalize_storage_path(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.contains('\0')).then(|| value.to_owned())
}

fn normalize_rtsp_url(value: Option<String>) -> Result<Option<String>, &'static str> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let url = Url::parse(value).map_err(|_| "must be a valid RTSP URL")?;
    if url.scheme() != "rtsp" || url.host_str().is_none() {
        return Err("must use the rtsp scheme and include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must use the configured camera username and password");
    }
    Ok(Some(value.to_owned()))
}

fn server_bind_address(host: &str, port: u16) -> String {
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    format!("{host}:{port}")
}

fn normalize_manufacturer(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= 120 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn update_camera_manufacturer(request: &Request, state: &ServerState, camera_id: &str) -> Response {
    let Some(camera) = state.camera(camera_id) else {
        return service_error(404, "camera not found");
    };
    let Some(body) = request.data() else {
        return service_error(400, "missing manufacturer update");
    };
    let update: ManufacturerOverrideUpdate = match serde_json::from_reader(body) {
        Ok(update) => update,
        Err(error) => return service_error(400, &format!("invalid manufacturer update: {error}")),
    };
    let manufacturer = match update.manufacturer {
        Some(manufacturer) => {
            let normalized = normalize_manufacturer(&manufacturer);
            if normalized.is_none() && !manufacturer.trim().is_empty() {
                return service_error(400, "manufacturer must be at most 120 printable characters");
            }
            normalized
        }
        None => None,
    };
    let Some(config_path) = &state.camera_config_path else {
        return service_error(409, "camera configuration persistence is unavailable");
    };
    let Ok(camera_ip) = camera.info.ip.parse::<IpAddr>() else {
        return internal_error("camera has an invalid IP address");
    };

    let _config_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(error) =
        config::set_camera_manufacturer(config_path, camera_ip, manufacturer.as_deref())
    {
        return service_error(
            500,
            &format!("unable to save manufacturer override: {error}"),
        );
    }
    {
        let mut overrides = state
            .manufacturer_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match manufacturer {
            Some(manufacturer) => {
                overrides.insert(camera.info.id.clone(), manufacturer);
            }
            None => {
                overrides.remove(&camera.info.id);
            }
        }
    }

    Response::json(&state.camera_info(&camera))
}

fn motion_detection_status(camera: &CameraEntry) -> MotionDetection {
    let Some(control) = &camera.control else {
        return MotionDetection {
            supported: camera.info.capabilities.events,
            controllable: false,
            enabled: None,
            error: None,
        };
    };
    match reolink_motion_state(control) {
        Ok(enabled) => MotionDetection {
            supported: true,
            controllable: true,
            enabled: Some(enabled),
            error: None,
        },
        Err(error) => MotionDetection {
            supported: camera.info.capabilities.events,
            controllable: true,
            enabled: None,
            error: Some(error.to_string()),
        },
    }
}

fn reolink_motion_state(control: &CameraControl) -> anyhow::Result<bool> {
    let mut client = ReolinkClient::new_with_http_port(control.ip, control.http_port);
    client.login(&control.username, &control.password)?;
    client.get_md_state(0)
}

fn update_camera_motion(request: &Request, state: &ServerState, camera_id: &str) -> Response {
    let Some(camera) = state.camera(camera_id) else {
        return service_error(404, "camera not found");
    };
    let Some(control) = &camera.control else {
        return service_error(
            409,
            "motion detection control is unavailable for this camera",
        );
    };
    let Some(body) = request.data() else {
        return service_error(400, "missing motion detection update");
    };
    let update: MotionDetectionUpdate = match serde_json::from_reader(body) {
        Ok(update) => update,
        Err(error) => {
            return service_error(400, &format!("invalid motion detection update: {error}"));
        }
    };
    let mut client = ReolinkClient::new_with_http_port(control.ip, control.http_port);
    if let Err(error) = client.login(&control.username, &control.password) {
        return service_error(502, &format!("camera motion login failed: {error}"));
    }
    if let Err(error) = client.set_alarm(0, update.enabled) {
        return service_error(502, &format!("camera motion update failed: {error}"));
    }
    match client.get_md_state(0) {
        Ok(enabled) if enabled == update.enabled => Response::json(&MotionDetection {
            supported: true,
            controllable: true,
            enabled: Some(enabled),
            error: None,
        }),
        Ok(enabled) => service_error(
            502,
            &format!(
                "camera motion state was {enabled} after requesting {}",
                update.enabled
            ),
        ),
        Err(error) => service_error(502, &format!("camera motion verification failed: {error}")),
    }
}

const fn camera_backend_name(backend: CameraBackend) -> &'static str {
    match backend {
        CameraBackend::Auto => "auto",
        CameraBackend::Retina => "retina",
        CameraBackend::ReoProto => "reo-proto",
    }
}

const fn camera_transport_name(transport: CameraTransport) -> &'static str {
    match transport {
        CameraTransport::Tcp => "tcp",
        CameraTransport::Udp => "udp",
    }
}

fn camera_web_url(ip: IpAddr, ports: &CameraPorts) -> String {
    let host = match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    if let Some(port) = ports.https {
        return if port == 443 {
            format!("https://{host}")
        } else {
            format!("https://{host}:{port}")
        };
    }
    if let Some(port) = ports.http {
        return if port == 80 {
            format!("http://{host}")
        } else {
            format!("http://{host}:{port}")
        };
    }
    format!("http://{host}")
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn recording_activity(state: &ServerState, camera_id: &str, stream: &str) -> Response {
    let Some(camera) = state.camera(camera_id) else {
        return service_error(404, "camera not found");
    };
    if !matches!(stream, "main" | "sub") {
        return service_error(400, "stream must be 'main' or 'sub'");
    }

    state.recording_demand.renew(
        &format!("{}/{stream}", camera.recording_label),
        RECORDING_ACTIVITY_LEASE,
    );
    Response::empty_204()
}

fn browser_webrtc_offer(request: &Request, state: &ServerState) -> Response {
    let Some(body) = request.data() else {
        return service_error(400, "missing browser SDP offer");
    };
    let request: BrowserLiveOffer = match serde_json::from_reader(body) {
        Ok(request) => request,
        Err(error) => return service_error(400, &format!("invalid browser SDP offer: {error}")),
    };
    let mut camera_ids = HashSet::with_capacity(request.tracks.len());
    let mut plans = Vec::with_capacity(request.tracks.len());
    for track in request.tracks {
        if !camera_ids.insert(track.camera_id.clone()) {
            return service_error(400, "browser offer contains a camera more than once");
        }
        let Some(camera) = state.camera(&track.camera_id) else {
            return service_error(404, "camera not found");
        };
        let track_id = match LiveTrackId::parse(track.track_id) {
            Ok(track_id) => track_id,
            Err(error) => return service_error(400, &error.to_string()),
        };
        let camera_ip = match camera.info.ip.parse() {
            Ok(ip) => ip,
            Err(error) => return internal_error(&format!("invalid camera IP: {error}")),
        };
        let has_sub_stream = camera
            .info
            .profiles
            .iter()
            .any(|profile| profile.stream == "sub");
        plans.push(BrowserTrackPlan {
            track_id,
            mid: track.mid,
            camera_ip,
            has_sub_stream,
            recording_label: camera.recording_label.clone(),
            quality: track.quality,
        });
    }

    let session = match state.webrtc.accept_browser_offer(plans, request.offer) {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(%error, "unable to create shared browser WebRTC session");
            return service_error(400, &format!("unable to accept SDP offer: {error}"));
        }
    };
    let Some(status) = state.webrtc.browser_session_status(session.id) else {
        return service_error(503, "shared WebRTC session ended during setup");
    };
    Response::json(&BrowserLiveAnswer {
        session_id: session.id,
        answer: session.answer,
        status,
    })
}

fn browser_live_session_status(state: &ServerState, session_id: LiveSessionId) -> Response {
    state.webrtc.browser_session_status(session_id).map_or_else(
        || service_error(404, "shared WebRTC session not found"),
        |status| Response::json(&status),
    )
}

fn update_browser_track_quality(
    request: &Request,
    state: &ServerState,
    session_id: LiveSessionId,
    track_id: &str,
) -> Response {
    let Some(body) = request.data() else {
        return service_error(400, "missing shared track quality update");
    };
    let update: LiveQualityUpdate = match serde_json::from_reader(body) {
        Ok(update) => update,
        Err(error) => {
            return service_error(
                400,
                &format!("invalid shared track quality update: {error}"),
            );
        }
    };
    let track_id = match LiveTrackId::parse(track_id.to_owned()) {
        Ok(track_id) => track_id,
        Err(error) => return service_error(400, &error.to_string()),
    };
    state
        .webrtc
        .set_browser_track_quality(session_id, &track_id, update.quality)
        .map_or_else(
            || service_error(404, "shared WebRTC track not found"),
            |status| Response::json(&status),
        )
}

fn close_browser_live_session(state: &ServerState, session_id: LiveSessionId) -> Response {
    state.webrtc.close_browser_session(session_id);
    Response::empty_204()
}

fn adaptive_webrtc_offer(request: &Request, state: &ServerState, camera_id: &str) -> Response {
    let Some(camera) = state.camera(camera_id) else {
        return service_error(404, "camera not found");
    };
    let Some(body) = request.data() else {
        return service_error(400, "missing adaptive SDP offer");
    };
    let request: AdaptiveLiveOffer = match serde_json::from_reader(body) {
        Ok(request) => request,
        Err(error) => return service_error(400, &format!("invalid adaptive SDP offer: {error}")),
    };
    let camera_ip = match camera.info.ip.parse() {
        Ok(ip) => ip,
        Err(error) => return internal_error(&format!("invalid camera IP: {error}")),
    };
    let has_sub_stream = camera
        .info
        .profiles
        .iter()
        .any(|profile| profile.stream == "sub");

    let session = match state.webrtc.accept_adaptive_offer(
        camera_ip,
        has_sub_stream,
        &camera.recording_label,
        request.quality,
        request.offer,
    ) {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(%camera_id, %error, "unable to create adaptive WebRTC session");
            return service_error(400, &format!("unable to accept SDP offer: {error}"));
        }
    };
    let Some(status) = state.webrtc.session_status(session.id) else {
        return service_error(503, "WebRTC session ended during setup");
    };
    Response::json(&AdaptiveLiveAnswer {
        session_id: session.id,
        answer: session.answer,
        status,
    })
}

fn live_session_status(state: &ServerState, session_id: LiveSessionId) -> Response {
    state.webrtc.session_status(session_id).map_or_else(
        || service_error(404, "live session not found"),
        |status| Response::json(&status),
    )
}

fn update_live_quality(
    request: &Request,
    state: &ServerState,
    session_id: LiveSessionId,
) -> Response {
    let Some(body) = request.data() else {
        return service_error(400, "missing quality update");
    };
    let update: LiveQualityUpdate = match serde_json::from_reader(body) {
        Ok(update) => update,
        Err(error) => return service_error(400, &format!("invalid quality update: {error}")),
    };
    state
        .webrtc
        .set_quality(session_id, update.quality)
        .map_or_else(
            || service_error(404, "live session not found"),
            |status| Response::json(&status),
        )
}

fn webrtc_offer(request: &Request, state: &ServerState, camera_id: &str, stream: &str) -> Response {
    let Some(camera) = state.camera(camera_id) else {
        return service_error(404, "camera not found");
    };
    let stream = match stream {
        "main" => crate::keeppeek::StreamKind::Main,
        "sub" => crate::keeppeek::StreamKind::Sub,
        _ => return service_error(400, "stream must be 'main' or 'sub'"),
    };
    let Some(body) = request.data() else {
        return service_error(400, "missing SDP offer");
    };
    let offer = match serde_json::from_reader(body) {
        Ok(offer) => offer,
        Err(error) => return service_error(400, &format!("invalid SDP offer: {error}")),
    };
    let camera_ip = match camera.info.ip.parse() {
        Ok(ip) => ip,
        Err(error) => return internal_error(&format!("invalid camera IP: {error}")),
    };

    let recording_stream_id = format!("{}/{stream}", camera.recording_label);
    match state.webrtc.accept_offer_for_recording(
        Source { camera_ip, stream },
        &recording_stream_id,
        offer,
    ) {
        Ok(answer) => Response::json(&answer),
        Err(error) => {
            tracing::warn!(%camera_id, %stream, %error, "unable to create WebRTC session");
            service_error(400, &format!("unable to accept SDP offer: {error}"))
        }
    }
}

fn list_recordings(
    state: &ServerState,
    camera_id: &str,
    date_filter: Option<&str>,
) -> Result<RecordingsResponse, Response> {
    let Some(camera) = state.camera(camera_id) else {
        return Err(service_error(404, "camera not found"));
    };
    if date_filter.is_some_and(|date| !safe_component(date)) {
        return Err(service_error(400, "invalid date"));
    }

    let mut dates = BTreeSet::new();
    for stream in ["main", "sub"] {
        let stream_root = state.recordings.join(&camera.recording_label).join(stream);
        for date_path in child_directories(&stream_root) {
            if let Some(date) = file_name(&date_path)
                && recording_date(&date).is_some()
            {
                dates.insert(date);
            }
        }
    }
    let selected_date = date_filter
        .map(str::to_owned)
        .or_else(|| dates.last().cloned());
    let dates = dates.into_iter().rev().collect::<Vec<_>>();
    let mut segments = Vec::new();
    for stream in ["main", "sub"] {
        let stream_root = state.recordings.join(&camera.recording_label).join(stream);
        let date_dirs = selected_date
            .as_ref()
            .map_or_else(Vec::new, |date| vec![stream_root.join(date)]);
        for date_path in date_dirs {
            let Some(date) = file_name(&date_path) else {
                continue;
            };
            for hour_path in child_directories(&date_path) {
                let Some(hour) = file_name(&hour_path) else {
                    continue;
                };
                for file_path in child_files(&hour_path) {
                    let Some(filename) = file_name(&file_path) else {
                        continue;
                    };
                    if !filename.ends_with(".mp4") || filename.ends_with(".active") {
                        continue;
                    }
                    let Some(start_time_ms) = recording_start_time_ms(&date, &hour, &filename)
                    else {
                        tracing::warn!(path = %file_path.display(), "unable to parse recording timestamp");
                        continue;
                    };
                    let duration_ms = match recording_duration_ms(&file_path) {
                        Ok(duration) => duration,
                        Err(error) => {
                            tracing::warn!(path = %file_path.display(), %error, "unable to read recording metadata");
                            continue;
                        }
                    };
                    let Ok(duration_ms_i64) = i64::try_from(duration_ms) else {
                        tracing::warn!(path = %file_path.display(), "recording duration exceeds API timestamp range");
                        continue;
                    };
                    segments.push(RecordingSegment {
                        stream: stream.to_owned(),
                        date: date.clone(),
                        hour: hour.clone(),
                        url: format!(
                            "/api/recordings/{camera_id}/{stream}/{date}/{hour}/{filename}"
                        ),
                        filename,
                        start_time_ms,
                        end_time_ms: start_time_ms.saturating_add(duration_ms_i64),
                        duration_ms,
                    });
                }
            }
        }
    }
    segments.sort_unstable_by(|left, right| {
        (&right.date, &right.hour, &right.filename, &right.stream).cmp(&(
            &left.date,
            &left.hour,
            &left.filename,
            &left.stream,
        ))
    });
    Ok(RecordingsResponse {
        camera_id: camera_id.to_owned(),
        date: selected_date,
        dates,
        segments,
    })
}

fn list_events(
    state: &ServerState,
    camera_id: &str,
    date: &str,
) -> Result<RecordingEventsResponse, Response> {
    if state.camera(camera_id).is_none() {
        return Err(service_error(404, "camera not found"));
    }
    let Some((start_ms, end_ms)) = event_day_range(date) else {
        return Err(service_error(400, "invalid event date"));
    };
    let Some(store) = &state.events else {
        return Ok(RecordingEventsResponse {
            camera_id: camera_id.to_owned(),
            date: date.to_owned(),
            events: Vec::new(),
        });
    };
    let events = store
        .events_in_range(camera_id, start_ms, end_ms)
        .map_err(|error| internal_error(&format!("unable to query events: {error}")))?
        .into_iter()
        .map(|event| RecordingEvent {
            thumbnail_url: event
                .thumbnail_filename
                .as_ref()
                .map(|_| format!("/api/events/{camera_id}/{}/thumbnail", event.id)),
            id: event.id,
            source: event.source.as_str().to_owned(),
            kind: event.kind,
            start_time_ms: event.start_time_ms,
            end_time_ms: event.end_time_ms,
            confidence: event.confidence,
            bbox: event.bbox,
            zone: event.zone,
        })
        .collect();
    Ok(RecordingEventsResponse {
        camera_id: camera_id.to_owned(),
        date: date.to_owned(),
        events,
    })
}

fn event_thumbnail(state: &ServerState, camera_id: &str, event_id: &str) -> Response {
    if state.camera(camera_id).is_none() {
        return service_error(404, "camera not found");
    }
    if !safe_component(event_id) {
        return service_error(400, "invalid event identifier");
    }
    let Some(store) = &state.events else {
        return service_error(404, "event thumbnail not found");
    };
    let path = match store.thumbnail_path(camera_id, event_id) {
        Ok(Some(path)) => path,
        Ok(None) => return service_error(404, "event thumbnail not found"),
        Err(error) => {
            return internal_error(&format!("unable to resolve event thumbnail: {error}"));
        }
    };
    match File::open(path) {
        Ok(file) => Response::from_file("image/jpeg", file).with_public_cache(31_536_000),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            service_error(404, "event thumbnail not found")
        }
        Err(error) => internal_error(&format!("unable to open event thumbnail: {error}")),
    }
}

fn recording_file(
    request: &Request,
    state: &ServerState,
    camera_id: &str,
    stream: &str,
    date: &str,
    hour: &str,
    filename: &str,
) -> Response {
    let Some(camera) = state.camera(camera_id) else {
        return service_error(404, "camera not found");
    };
    if !matches!(stream, "main" | "sub")
        || ![date, hour, filename].into_iter().all(safe_component)
        || !filename.ends_with(".mp4")
    {
        return service_error(400, "invalid recording path");
    }
    let path = state
        .recordings
        .join(&camera.recording_label)
        .join(stream)
        .join(date)
        .join(hour)
        .join(filename);
    let path = if needs_browser_compatibility(filename) {
        match crate::storage::playback::browser_compatible_recording(&path) {
            Ok(compatible) => compatible,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "unable to prepare legacy recording for browser playback");
                path
            }
        }
    } else {
        path
    };
    match File::open(path) {
        Ok(file) => match Response::from_file_with_range(request, "video/mp4", file) {
            Ok(response) if response.is_success() => response.with_public_cache(31_536_000),
            Ok(response) => response,
            Err(error) => internal_error(&format!("unable to serve recording: {error}")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            service_error(404, "recording not found")
        }
        Err(error) => internal_error(&format!("unable to open recording: {error}")),
    }
}

fn needs_browser_compatibility(filename: &str) -> bool {
    filename
        .strip_suffix(".mp4")
        .is_some_and(|stem| stem.len() == 4 && stem.bytes().all(|byte| byte.is_ascii_digit()))
}

fn safe_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\'])
}

fn child_directories(path: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect()
}

fn child_files(path: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.path())
        .collect()
}

fn file_name(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(str::to_owned)
}

fn recording_start_time_ms(date: &str, hour: &str, filename: &str) -> Option<i64> {
    let stem = filename.strip_suffix(".mp4")?;
    if date.len() != 10 || hour.len() != 2 || !matches!(stem.len(), 4 | 7) {
        return None;
    }
    let date = recording_date(date)?;
    let hour = hour.parse().ok()?;
    let minute = stem[0..2].parse().ok()?;
    let second = stem[2..4].parse().ok()?;
    let millisecond = if stem.len() == 7 {
        stem[4..7].parse().ok()?
    } else {
        0
    };
    let time = time::Time::from_hms_milli(hour, minute, second, millisecond).ok()?;
    time::PrimitiveDateTime::new(date, time)
        .assume_utc()
        .unix_timestamp()
        .checked_mul(1_000)
        .and_then(|timestamp| timestamp.checked_add(i64::from(millisecond)))
}

fn recording_date(value: &str) -> Option<time::Date> {
    if value.len() != 10 || &value[4..5] != "-" || &value[7..8] != "-" {
        return None;
    }
    let year = value[0..4].parse().ok()?;
    let month = time::Month::try_from(value[5..7].parse::<u8>().ok()?).ok()?;
    let day = value[8..10].parse().ok()?;
    time::Date::from_calendar_date(year, month, day).ok()
}

fn event_day_range(value: &str) -> Option<(i64, i64)> {
    let date = recording_date(value)?;
    let start_ms = date
        .midnight()
        .assume_utc()
        .unix_timestamp()
        .checked_mul(1_000)?;
    Some((start_ms, start_ms.checked_add(86_400_000)?))
}

fn recording_duration_ms(path: &Path) -> anyhow::Result<u64> {
    let reader = mp4::read_mp4(File::open(path)?)?;
    let duration = reader
        .tracks()
        .values()
        .filter(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
        .map(mp4::Mp4Track::duration)
        .max()
        .unwrap_or_else(|| reader.duration());
    Ok(u64::try_from(duration.as_millis())?)
}

fn serve_ui(request: &Request) -> Response {
    if request.method() != "GET" {
        return service_error(404, "not found");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/build");
    let response = rouille::match_assets(request, &root);
    if response.is_success() {
        return response;
    }
    File::open(root.join("index.html")).map_or_else(
        |_| service_error(404, "not found"),
        |file| Response::from_file("text/html; charset=utf-8", file).with_no_cache(),
    )
}

fn query_router(
    router_tx: &FacadeSender<RouterMessage>,
    query: RouterQuery,
) -> Result<RouterResponse, Response> {
    let (reply, rx) = mpsc::sync_channel(1);
    router_tx
        .send(RouterMessage::Query { query, reply })
        .map_err(|error| match error {
            FacadeSendError::Disconnected(_) => service_error(503, "router unavailable"),
            FacadeSendError::Notify(error) => {
                tracing::error!(%error, "unable to wake router");
                service_error(503, "router unavailable")
            }
        })?;

    match rx.recv_timeout(ROUTER_REPLY_TIMEOUT) {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(RouterError::CameraNotFound(id))) => {
            Err(service_error(404, &format!("camera '{id}' not found")))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => Err(service_error(504, "router timed out")),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(service_error(503, "router unavailable")),
    }
}

fn internal_error(message: &str) -> Response {
    tracing::error!(%message, "invalid router response");
    service_error(500, message)
}

fn service_error(status: u16, message: &str) -> Response {
    Response::json(&ApiError {
        error: message.to_owned(),
    })
    .with_status_code(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::LogFilterFile;
    use crate::storage::{
        RecordingCatalog,
        metadata::{EventSource, TimelineEvent},
    };
    use image::{DynamicImage, codecs::jpeg::JpegEncoder};
    use std::io::{self, Read, Write};
    use time::macros::datetime;

    fn response_data(response: Response) -> Vec<u8> {
        let (mut reader, _) = response.data.into_reader_and_size();
        let mut data = Vec::new();
        reader.read_to_end(&mut data).unwrap();
        data
    }

    fn logging_test_state(
        initial_filter: &str,
    ) -> (
        ServerState,
        LoggingService,
        tracing::Dispatch,
        LogFilterFile,
    ) {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-server-logging-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&directory).unwrap();
        let filter_file = LogFilterFile::new(directory.join("log-filter"));
        let (logging, dispatch) = LoggingService::for_test(filter_file.clone(), initial_filter);
        (
            ServerState::empty().with_logging(logging.clone()),
            logging,
            dispatch,
            filter_file,
        )
    }

    fn read_http_until(
        address: std::net::SocketAddr,
        path: &str,
        marker: &str,
    ) -> io::Result<String> {
        let mut stream = std::net::TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        )?;
        stream.flush()?;

        let mut response = Vec::new();
        let mut buffer = [0_u8; 4096];
        while !response
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
        {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => response.extend_from_slice(&buffer[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(String::from_utf8_lossy(&response).into_owned())
    }

    #[test]
    fn live_server_serves_captured_logs_over_http_and_sse() {
        let (state, _logging, dispatch, filter_file) = logging_test_state("trace");
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(
                target: "keeppeek::integration",
                camera_id = "test-camera",
                "low-level server log"
            );
        });

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = Shutdown::new();
        let server_shutdown = shutdown.clone();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let server = std::thread::spawn(move || {
            serve_with_state_on_listener(listener, server_shutdown, router_tx, state)
        });

        let responses = (|| -> anyhow::Result<(String, String)> {
            let snapshot = ureq::get(format!("http://{address}/api/logs?limit=10"))
                .call()?
                .into_body()
                .read_to_string()?;
            let stream =
                read_http_until(address, "/api/logs/stream?tail=10", "low-level server log")?;
            Ok((snapshot, stream))
        })();

        shutdown.cancel();
        server.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(filter_file.path().parent().unwrap());

        let (snapshot, stream) = responses.unwrap();
        assert!(snapshot.contains("\"target\":\"keeppeek::integration\""));
        assert!(snapshot.contains("\"message\":\"low-level server log\""));
        assert!(snapshot.contains("\"camera_id\":\"test-camera\""));

        assert!(
            stream.starts_with("HTTP/1.1 200 OK"),
            "unexpected SSE response: {stream:?}"
        );
        assert!(stream.contains("Content-Type: text/event-stream"));
        assert!(stream.contains("Transfer-Encoding: chunked"));
        assert!(stream.contains("event: log"));
        assert!(stream.contains("\"message\":\"low-level server log\""));
    }

    #[test]
    fn live_server_shutdown_closes_active_log_stream() {
        let (state, _logging, _dispatch, filter_file) = logging_test_state("trace");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = Shutdown::new();
        let server_shutdown = shutdown.clone();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let (stopped_tx, stopped_rx) = std::sync::mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let result = serve_with_state_on_listener(listener, server_shutdown, router_tx, state);
            let _ = stopped_tx.send(());
            result
        });

        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            stream,
            "GET /api/logs/stream HTTP/1.1\r\nHost: {address}\r\n\r\n"
        )
        .unwrap();
        stream.flush().unwrap();
        let mut response = Vec::new();
        let mut buffer = [0_u8; 512];
        while !response
            .windows(b": connected\n\n".len())
            .any(|window| window == b": connected\n\n")
        {
            let read = stream.read(&mut buffer).unwrap();
            assert_ne!(read, 0, "SSE response ended before its initial frame");
            response.extend_from_slice(&buffer[..read]);
        }

        shutdown.cancel();
        let stopped = stopped_rx.recv_timeout(Duration::from_secs(1));
        drop(stream);
        server.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(filter_file.path().parent().unwrap());

        assert!(
            stopped.is_ok(),
            "server did not stop while a log stream remained connected"
        );
    }

    #[test]
    fn logging_filter_route_persists_and_reloads_valid_directive() {
        let (state, logging, dispatch, filter_file) = logging_test_state("error");
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let request = Request::fake_http(
            "PUT",
            "/api/settings/logging",
            vec![("Content-Type".to_owned(), "application/json".to_owned())],
            serde_json::to_vec(&serde_json::json!({ "filter": "info,str0m=warn" })).unwrap(),
        );

        let response = tracing::dispatcher::with_default(&dispatch, || {
            let response = handle_request(&request, &router_tx, &state);
            tracing::info!(target: "keeppeek::test", "included after update");
            tracing::info!(target: "str0m", "filtered after update");
            tracing::warn!(target: "str0m", "included after update");
            response
        });

        assert_eq!(response.status_code, 200);
        let body: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(body["active_filter"], "info,str0m=warn");
        assert_eq!(
            std::fs::read_to_string(filter_file.path()).unwrap(),
            "info,str0m=warn"
        );
        let snapshot = logging.snapshot(None, 100);
        assert!(snapshot.entries.iter().any(|entry| {
            entry.target == "keeppeek::test" && entry.message == "included after update"
        }));
        assert!(
            snapshot.entries.iter().any(|entry| {
                entry.target == "str0m" && entry.message == "included after update"
            })
        );
        assert!(
            !snapshot
                .entries
                .iter()
                .any(|entry| entry.message == "filtered after update")
        );
        std::fs::remove_dir_all(filter_file.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn logging_filter_route_rejects_invalid_directive_without_mutation() {
        let (state, logging, dispatch, filter_file) = logging_test_state("warn");
        filter_file.write_log_filter("warn").unwrap();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let request = Request::fake_http(
            "PUT",
            "/api/settings/logging",
            vec![("Content-Type".to_owned(), "application/json".to_owned())],
            serde_json::to_vec(&serde_json::json!({ "filter": "keeppeek=verbose" })).unwrap(),
        );

        let response = tracing::dispatcher::with_default(&dispatch, || {
            let response = handle_request(&request, &router_tx, &state);
            tracing::info!(target: "keeppeek::test", "still filtered");
            tracing::warn!(target: "keeppeek::test", "still included");
            response
        });

        assert_eq!(response.status_code, 400);
        assert_eq!(logging.active_filter(), "warn");
        assert_eq!(std::fs::read_to_string(filter_file.path()).unwrap(), "warn");
        let snapshot = logging.snapshot(None, 100);
        assert!(
            !snapshot
                .entries
                .iter()
                .any(|entry| entry.message == "still filtered")
        );
        assert!(
            snapshot
                .entries
                .iter()
                .any(|entry| entry.message == "still included")
        );
        std::fs::remove_dir_all(filter_file.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn log_snapshot_route_applies_cursor_and_limit() {
        let (state, _logging, dispatch, filter_file) = logging_test_state("trace");
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(target: "keeppeek::test", "one");
            tracing::info!(target: "keeppeek::test", "two");
            tracing::info!(target: "keeppeek::test", "three");
        });
        let request =
            Request::fake_http("GET", "/api/logs?after=1&limit=1", Vec::new(), Vec::new());

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        let body: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(body["entries"].as_array().unwrap().len(), 1);
        assert_eq!(body["entries"][0]["sequence"], 2);
        assert_eq!(body["entries"][0]["message"], "two");
        assert_eq!(body["truncated"], true);
        std::fs::remove_dir_all(filter_file.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn log_stream_route_replays_last_event_id_as_sse() {
        let (state, logging, dispatch, filter_file) = logging_test_state("trace");
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(target: "keeppeek::test", "streamed");
        });
        let request = Request::fake_http(
            "GET",
            "/api/logs/stream",
            vec![("Last-Event-ID".to_owned(), "0".to_owned())],
            Vec::new(),
        );

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        assert!(response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Content-Type") && value.starts_with("text/event-stream")
        }));
        assert!(response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("X-Accel-Buffering") && value == "no"
        }));
        logging.close_streams();
        let output = String::from_utf8(response_data(response)).unwrap();
        assert!(output.starts_with(": connected\n\n"));
        assert!(output.contains("id: 1\nevent: log\ndata: "));
        assert!(output.contains("\"message\":\"streamed\""));
        std::fs::remove_dir_all(filter_file.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn recording_layout_maps_to_utc_timestamp() {
        let expected = datetime!(2026-08-10 14:35:09 UTC).unix_timestamp() * 1_000;
        assert_eq!(
            recording_start_time_ms("2026-08-10", "14", "3509.mp4"),
            Some(expected)
        );
    }

    #[test]
    fn recording_layout_preserves_server_timestamp_milliseconds() {
        let expected = datetime!(2026-08-10 14:35:09.123 UTC).unix_timestamp_nanos() / 1_000_000;
        assert_eq!(
            recording_start_time_ms("2026-08-10", "14", "3509123.mp4"),
            i64::try_from(expected).ok()
        );
    }

    #[test]
    fn recording_layout_rejects_invalid_dates_and_times() {
        assert_eq!(
            recording_start_time_ms("2026-02-30", "14", "3509.mp4"),
            None
        );
        assert_eq!(
            recording_start_time_ms("2026-08-10", "24", "3509.mp4"),
            None
        );
        assert_eq!(
            recording_start_time_ms("2026-08-10", "14", "6060.mp4"),
            None
        );
        assert_eq!(
            recording_start_time_ms("2026-08-10", "14", "3509.mp4.active"),
            None
        );
    }

    #[test]
    fn only_legacy_recording_names_require_browser_compatibility() {
        assert!(needs_browser_compatibility("3509.mp4"));
        assert!(!needs_browser_compatibility("3509123.mp4"));
        assert!(!needs_browser_compatibility("3509.mp4.active"));
    }

    #[test]
    fn recording_duration_uses_video_when_legacy_audio_timescale_is_invalid() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-recording-duration-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("recording.mp4");
        let config = mp4::Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
            timescale: 1_000,
        };
        let tracks = [
            mp4::TrackConfig {
                track_type: mp4::TrackType::Video,
                timescale: 90_000,
                language: "und".to_owned(),
                media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                    width: 320,
                    height: 240,
                    seq_param_set: Vec::new(),
                    pic_param_set: Vec::new(),
                }),
            },
            mp4::TrackConfig {
                track_type: mp4::TrackType::Audio,
                timescale: 16,
                language: "und".to_owned(),
                media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
                    bitrate: 64_000,
                    profile: mp4::AudioObjectType::AacLowComplexity,
                    freq_index: mp4::SampleFreqIndex::Freq16000,
                    chan_conf: mp4::ChannelConfig::Mono,
                }),
            },
        ];
        let mut writer =
            mp4::FragmentedMp4Writer::write_start(File::create(&path).unwrap(), &config, &tracks)
                .unwrap();
        writer
            .write_sample(
                1,
                mp4::Mp4Sample {
                    start_time: 0,
                    duration: 90_000,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: vec![1].into(),
                },
            )
            .unwrap();
        writer
            .write_sample(
                2,
                mp4::Mp4Sample {
                    start_time: 0,
                    duration: 1_024,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: vec![2].into(),
                },
            )
            .unwrap();
        writer.write_end().unwrap();
        drop(writer.into_writer());

        assert_eq!(recording_duration_ms(&path).unwrap(), 1_000);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn event_day_range_uses_utc_day_boundaries() {
        let start = datetime!(2026-08-10 00:00 UTC).unix_timestamp() * 1_000;
        let end = datetime!(2026-08-11 00:00 UTC).unix_timestamp() * 1_000;
        assert_eq!(event_day_range("2026-08-10"), Some((start, end)));
        assert_eq!(event_day_range("2026-02-30"), None);
    }

    #[test]
    fn health_route_returns_runtime_and_resource_sections() {
        let state = ServerState::empty();
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let readiness_request = Request::fake_http("GET", "/health", Vec::new(), Vec::new());
        let readiness_response = handle_request(&readiness_request, &router_tx, &state);
        assert_eq!(readiness_response.status_code, 200);
        assert!(readiness_response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Access-Control-Allow-Origin") && value == "*"
        }));
        let request = Request::fake_http("GET", "/api/health", Vec::new(), Vec::new());

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        let body: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        assert!(matches!(
            body["status"].as_str(),
            Some("healthy" | "degraded")
        ));
        assert!(body["system"]["process"]["pid"].as_u64().is_some());
        assert!(
            body["system"]["process"]["cpu_capacity_percent"]
                .as_f64()
                .is_some()
        );
        assert!(
            body["system"]["process"]["cpu_core_equivalents"]
                .as_f64()
                .is_some()
        );
        assert!(
            body["system"]["process"]["memory_capacity_percent"]
                .as_f64()
                .is_some()
        );
        assert!(body["system"]["memory"]["total_bytes"].as_u64().is_some());
        assert!(body["system"]["network_egress_bps"].as_u64().is_some());
        assert!(
            body["storage"]["demand"]["active_streams"]
                .as_u64()
                .is_some()
        );
        assert!(body["webrtc"]["active_sessions"].as_u64().is_some());
        assert!(body["cameras"].as_array().is_some());
        assert_eq!(router_thread.join().unwrap(), 1);
    }

    #[test]
    fn health_route_reports_bytes_for_a_custom_recording_catalog_path() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-custom-catalog-health-{}",
            rand::random::<u64>()
        ));
        let catalog_path = directory.join("metadata/recordings.db");
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        std::fs::write(&catalog_path, b"catalog").unwrap();
        let config = Config {
            host: "0.0.0.0".to_owned(),
            port: 3000,
            storage: StorageToml {
                recording_catalog_path: Some(catalog_path.to_string_lossy().into_owned()),
                ..StorageToml::default()
            },
            battery_wake: Default::default(),
        };
        let storage = StorageConfig::from_toml(&config.storage);
        let state = ServerState::new(
            &config,
            &HashMap::new(),
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        );
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let request = Request::fake_http("GET", "/api/health", Vec::new(), Vec::new());

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        let body: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(body["storage"]["catalog_bytes"], 7);
        assert_eq!(router_thread.join().unwrap(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn configured_camera_remains_visible_when_discovery_fails() {
        let config = Config::default();
        let storage = StorageConfig::default();
        let camera = CameraConfig {
            ip: "192.0.2.10".parse().unwrap(),
            name: Some("north".to_owned()),
            display_name: Some("North Courtyard".to_owned()),
            manufacturer: None,
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Auto,
            transport: CameraTransport::Tcp,
        };
        let camera_configs = HashMap::from([("cameras".to_owned(), vec![camera])]);

        let state = ServerState::new(
            &config,
            &camera_configs,
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        );

        let cameras = state.camera_entries();
        assert_eq!(cameras.len(), 1);
        assert_eq!(cameras[0].info.name.as_deref(), Some("North Courtyard"));
        assert_eq!(cameras[0].info.profiles.len(), 2);
        assert_eq!(state.config.camera_count, 1);
    }

    #[test]
    fn recording_capacity_estimate_includes_video_and_audio_bitrates() {
        let profiles = [
            ProfileSummary {
                name: "mainStream".to_owned(),
                stream: "main".to_owned(),
                encoding: None,
                resolution: None,
                framerate: None,
                bitrate_kbps: Some(8_000),
                gop: None,
                h264_profile: None,
                audio: Some(AudioProfileSummary {
                    encoding: "aac".to_owned(),
                    sample_rate: Some(48_000),
                    bitrate_kbps: Some(64),
                }),
            },
            ProfileSummary {
                name: "subStream".to_owned(),
                stream: "sub".to_owned(),
                encoding: None,
                resolution: None,
                framerate: None,
                bitrate_kbps: Some(512),
                gop: None,
                h264_profile: None,
                audio: None,
            },
            ProfileSummary {
                name: "unknown".to_owned(),
                stream: "sub".to_owned(),
                encoding: None,
                resolution: None,
                framerate: None,
                bitrate_kbps: None,
                gop: None,
                h264_profile: None,
                audio: None,
            },
        ];

        let estimate = recording_capacity_estimate(profiles.iter(), 185_241_600_000);

        assert_eq!(estimate.estimated_bitrate_bps, 8_576_000);
        assert_eq!(estimate.bytes_per_day, 92_620_800_000);
        assert_eq!(estimate.known_streams, 2);
        assert_eq!(estimate.unknown_streams, 1);
        assert_eq!(estimate.estimated_retention_days, Some(2.0));
    }

    #[test]
    fn camera_details_include_configured_transport_and_profiles() {
        let config = Config::default();
        let storage = StorageConfig::default();
        let camera = CameraConfig {
            ip: "192.0.2.41".parse().unwrap(),
            name: Some("fake-retina".to_owned()),
            display_name: Some("Fake Retina".to_owned()),
            manufacturer: None,
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: Some(8000),
            http_port: Some(8080),
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Retina,
            transport: CameraTransport::Udp,
        };
        let camera_configs = HashMap::from([("cameras".to_owned(), vec![camera])]);
        let state = ServerState::new(
            &config,
            &camera_configs,
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        );
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });

        let request = Request::fake_http(
            "GET",
            "/api/cameras/192.0.2.41/details",
            Vec::new(),
            Vec::new(),
        );
        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        let body: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(body["camera"]["backend"], "retina");
        assert_eq!(body["camera"]["transport"], "udp");
        assert_eq!(body["camera"]["web_url"], "http://192.0.2.41:8080");
        assert_eq!(body["camera"]["profiles"].as_array().map(Vec::len), Some(2));
        assert_eq!(body["motion_detection"]["controllable"], false);
        assert_eq!(router_thread.join().unwrap(), 1);
    }

    #[test]
    fn manufacturer_override_updates_live_info_and_persists_to_config() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-server-manufacturer-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("cameras.toml");
        crate::config::write_private_file(
            &config_path,
            br#"
                [cameras.back_yard]
                ip = "192.0.2.55"
                username = "operator"
                password = "secret"
            "#,
        )
        .unwrap();

        let mut state = ServerState::empty().with_camera_config_path(config_path.clone());
        state.cameras = Arc::new(RwLock::new(vec![CameraEntry {
            info: CameraInfo {
                id: "192.0.2.55".to_owned(),
                ip: "192.0.2.55".to_owned(),
                name: Some("Back Yard".to_owned()),
                manufacturer: Some("ONVIF".to_owned()),
                model: None,
                firmware_version: None,
                serial_number: None,
                hardware_id: None,
                hostname: None,
                mac_address: None,
                is_reolink: false,
                backend: "retina".to_owned(),
                transport: "tcp".to_owned(),
                web_url: "http://192.0.2.55".to_owned(),
                ports: CameraPorts::default(),
                capabilities: Default::default(),
                profiles: Vec::new(),
            },
            reported_manufacturer: Some("ONVIF".to_owned()),
            configuration: CameraConfig {
                ip: "192.0.2.55".parse().unwrap(),
                name: Some("back_yard".to_owned()),
                display_name: Some("Back Yard".to_owned()),
                manufacturer: None,
                username: "operator".to_owned(),
                password: "secret".to_owned(),
                onvif_port: None,
                http_port: None,
                main_rtsp_url: None,
                sub_rtsp_url: None,
                uid: None,
                backend: CameraBackend::Retina,
                transport: CameraTransport::Tcp,
            },
            recording_label: "back-yard".to_owned(),
            control: None,
        }]));
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        let request = Request::fake_http(
            "PUT",
            "/api/cameras/192.0.2.55/manufacturer",
            Vec::new(),
            br#"{"manufacturer":"Hikvision"}"#.to_vec(),
        );
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        let updated: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(updated["manufacturer"], "Hikvision");
        let saved: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            saved["cameras"]["back_yard"]["manufacturer"].as_str(),
            Some("Hikvision")
        );

        let request = Request::fake_http(
            "PUT",
            "/api/cameras/192.0.2.55/manufacturer",
            Vec::new(),
            br#"{"manufacturer":null}"#.to_vec(),
        );
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        let restored: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(restored["manufacturer"], "ONVIF");
        let saved: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            saved["cameras"]["back_yard"]
                .as_table()
                .is_some_and(|camera| !camera.contains_key("manufacturer"))
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn settings_camera_update_persists_manual_camera_without_returning_credentials() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-settings-camera-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        crate::config::write_private_file(&config_path, b"[cameras]\n").unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });

        let request = Request::fake_http(
            "PUT",
            "/api/settings/cameras/192.0.2.77",
            Vec::new(),
            br#"{
                "display_name":"Manual Gate",
                "manufacturer":"Reolink",
                "username":"operator",
                "password":"not-in-the-response",
                "onvif_port":8080,
                "main_rtsp_url":"rtsp://192.0.2.77:8554/live/main",
                "sub_rtsp_url":"rtsp://192.0.2.77:8554/live/sub",
                "backend":"reo-proto",
                "transport":"udp"
            }"#
            .to_vec(),
        );
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        let body = response_data(response);
        assert!(!String::from_utf8_lossy(&body).contains("not-in-the-response"));
        let saved_response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(saved_response["camera"]["ip"], "192.0.2.77");
        assert_eq!(saved_response["camera"]["password_configured"], true);
        assert_eq!(saved_response["restart_required"], true);
        assert_eq!(router_thread.join().unwrap(), 1);

        let cameras = crate::config::load_cameras(&config_path).unwrap();
        let config = &cameras["cameras"][0];
        assert_eq!(config.ip, "192.0.2.77".parse::<IpAddr>().unwrap());
        assert_eq!(config.password, "not-in-the-response");
        assert_eq!(config.display_name.as_deref(), Some("Manual Gate"));
        assert_eq!(config.backend, CameraBackend::ReoProto);
        assert_eq!(config.transport, CameraTransport::Udp);
        assert_eq!(
            config.main_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.77:8554/live/main")
        );
        assert_eq!(
            config.sub_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.77:8554/live/sub")
        );

        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let request = Request::fake_http("GET", "/api/settings/cameras", Vec::new(), Vec::new());
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        let body = response_data(response);
        assert!(!String::from_utf8_lossy(&body).contains("not-in-the-response"));
        let settings: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(settings.as_array().map(Vec::len), Some(1));
        assert_eq!(settings[0]["ip"], "192.0.2.77");
        assert_eq!(settings[0]["password_configured"], true);
        assert_eq!(
            settings[0]["main_rtsp_url"],
            "rtsp://192.0.2.77:8554/live/main"
        );
        assert_eq!(
            settings[0]["sub_rtsp_url"],
            "rtsp://192.0.2.77:8554/live/sub"
        );
        assert_eq!(router_thread.join().unwrap(), 1);

        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let request = Request::fake_http(
            "PUT",
            "/api/settings/cameras/192.0.2.77",
            Vec::new(),
            br#"{"display_name":"Updated Manual Gate"}"#.to_vec(),
        );
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        assert!(!String::from_utf8_lossy(&response_data(response)).contains("not-in-the-response"));
        assert_eq!(router_thread.join().unwrap(), 1);
        let cameras = crate::config::load_cameras(&config_path).unwrap();
        let config = &cameras["cameras"][0];
        assert_eq!(config.display_name.as_deref(), Some("Updated Manual Gate"));
        assert_eq!(config.password, "not-in-the-response");
        assert_eq!(
            config.main_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.77:8554/live/main")
        );

        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let request = Request::fake_http(
            "PUT",
            "/api/settings/cameras/192.0.2.77",
            Vec::new(),
            br#"{"main_rtsp_url":null,"sub_rtsp_url":null}"#.to_vec(),
        );
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        assert_eq!(router_thread.join().unwrap(), 1);
        let cameras = crate::config::load_cameras(&config_path).unwrap();
        let config = &cameras["cameras"][0];
        assert_eq!(config.main_rtsp_url, None);
        assert_eq!(config.sub_rtsp_url, None);

        let request = Request::fake_http(
            "DELETE",
            "/api/settings/cameras/192.0.2.77",
            Vec::new(),
            Vec::new(),
        );
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 204);
        assert!(
            crate::config::load_cameras(&config_path)
                .unwrap()
                .get("cameras")
                .is_none_or(Vec::is_empty)
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rtsp_url_settings_require_credential_free_rtsp_endpoints() {
        assert_eq!(
            normalize_rtsp_url(Some(" rtsp://192.0.2.77:8554/live ".to_owned())).unwrap(),
            Some("rtsp://192.0.2.77:8554/live".to_owned())
        );
        assert_eq!(normalize_rtsp_url(Some(String::new())).unwrap(), None);
        assert!(normalize_rtsp_url(Some("https://192.0.2.77/live".to_owned())).is_err());
        assert!(normalize_rtsp_url(Some("rtsp://user:pass@192.0.2.77/live".to_owned())).is_err());
    }

    #[test]
    fn server_bind_address_brackets_raw_ipv6_hosts() {
        assert_eq!(server_bind_address("::", 3000), "[::]:3000");
        assert_eq!(server_bind_address("::1", 3200), "[::1]:3200");
        assert_eq!(server_bind_address("[::1]", 3200), "[::1]:3200");
    }

    #[test]
    fn runtime_settings_update_persists_and_reflects_pending_configuration() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-runtime-{}", rand::random::<u64>()));
        let config_path = directory.join("config.toml");
        crate::config::write_private_file(
            &config_path,
            br#"
                host = "0.0.0.0"
                port = 3000

                [storage]
                medium_term_path = "/media/keeppeek"
                long_term_path = "/archive/keeppeek"

                [cameras.front]
                ip = "192.0.2.44"
                username = "operator"
                password = "not-in-the-response"
            "#,
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let request = Request::fake_http(
            "PUT",
            "/api/settings/config",
            Vec::new(),
            br#"{
                "host":"127.0.0.1",
                "port":3200,
                "storage":{
                    "medium_term_path":"/media/new-keeppeek",
                    "long_term_path":"/archive/new-keeppeek",
                    "recording_catalog_path":"/metadata/new-recordings.db",
                    "event_thumbnail_path":"/metadata/new-thumbnails",
                    "event_thumbnail_max_mb":512,
                    "short_term_secs":30,
                    "medium_term_secs":120,
                    "flush_interval_secs":15,
                    "write_buffer_bytes":16384,
                    "long_term_max_gb":24
                }
            }"#
            .to_vec(),
        );

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        let body = response_data(response);
        assert!(!String::from_utf8_lossy(&body).contains("not-in-the-response"));
        let saved: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(saved["config"]["host"], "127.0.0.1");
        assert_eq!(saved["config"]["port"], 3200);
        assert_eq!(
            saved["config"]["storage"]["medium_term_path"],
            "/media/new-keeppeek"
        );
        assert_eq!(
            saved["config"]["storage"]["long_term_path"],
            "/archive/new-keeppeek"
        );
        assert_eq!(
            saved["config"]["storage"]["recording_catalog_path"],
            "/metadata/new-recordings.db"
        );
        assert_eq!(
            saved["config"]["storage"]["event_thumbnail_path"],
            "/metadata/new-thumbnails"
        );
        assert_eq!(saved["config"]["storage"]["event_thumbnail_max_mb"], 512);
        assert_eq!(saved["config"]["storage"]["medium_term_secs"], 120);
        assert_eq!(saved["restart_required"], true);

        let request = Request::fake_http("GET", "/api/config", Vec::new(), Vec::new());
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        let persisted: serde_json::Value =
            serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(persisted["host"], "127.0.0.1");
        assert_eq!(persisted["port"], 3200);
        assert_eq!(
            persisted["storage"]["medium_term_path"],
            "/media/new-keeppeek"
        );
        assert_eq!(
            persisted["storage"]["long_term_path"],
            "/archive/new-keeppeek"
        );
        assert_eq!(
            persisted["storage"]["recording_catalog_path"],
            "/metadata/new-recordings.db"
        );
        assert_eq!(
            persisted["storage"]["event_thumbnail_path"],
            "/metadata/new-thumbnails"
        );
        assert_eq!(persisted["storage"]["event_thumbnail_max_mb"], 512);
        assert_eq!(persisted["camera_count"], 1);

        let request = Request::fake_http(
            "PUT",
            "/api/settings/config",
            Vec::new(),
            br#"{
                "host":"127.0.0.1",
                "port":0,
                "storage":{
                    "medium_term_path":"/media/invalid",
                    "long_term_path":"/archive/invalid",
                    "recording_catalog_path":"/metadata/invalid-recordings.db",
                    "event_thumbnail_path":"/metadata/invalid-thumbnails",
                    "event_thumbnail_max_mb":512,
                    "short_term_secs":30,
                    "medium_term_secs":120,
                    "flush_interval_secs":15,
                    "write_buffer_bytes":16384,
                    "long_term_max_gb":24
                }
            }"#
            .to_vec(),
        );
        assert_eq!(
            handle_request(&request, &router_tx, &state).status_code,
            400
        );
        let persisted: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(persisted["port"].as_integer(), Some(3200));
        assert_eq!(
            persisted["cameras"]["front"]["password"].as_str(),
            Some("not-in-the-response")
        );
        assert!(!persisted.contains_key("storage_migration"));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn runtime_settings_can_schedule_a_storage_migration() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-runtime-storage-migration-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        let current = directory.join("current-recordings");
        let next = directory.join("next-recordings");
        let config = Config {
            host: "0.0.0.0".to_owned(),
            port: 3000,
            storage: StorageToml {
                medium_term_path: Some(current.to_string_lossy().into_owned()),
                long_term_path: Some(current.to_string_lossy().into_owned()),
                ..StorageToml::default()
            },
            battery_wake: Default::default(),
        };
        crate::config::write_private_file(
            &config_path,
            toml::to_string_pretty(&config).unwrap().as_bytes(),
        )
        .unwrap();
        let storage = StorageConfig::from_toml(&config.storage);
        let state = ServerState::new(
            &config,
            &HashMap::new(),
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        )
        .with_camera_config_path(config_path.clone());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let request = Request::fake_http(
            "PUT",
            "/api/settings/config",
            Vec::new(),
            serde_json::to_vec(&serde_json::json!({
                "host": "0.0.0.0",
                "port": 3000,
                "move_existing_recordings": true,
                "storage": {
                    "medium_term_path": next,
                    "long_term_path": next,
                    "recording_catalog_path": current.join("recordings.db"),
                    "event_thumbnail_path": current.join(".event-thumbnails"),
                    "event_thumbnail_max_mb": 1024,
                    "short_term_secs": 120,
                    "medium_term_secs": 1800,
                    "flush_interval_secs": 60,
                    "write_buffer_bytes": 8192,
                    "long_term_max_gb": 0
                }
            }))
            .unwrap(),
        );

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        let response: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        let expected_catalog_path = next.join("recordings.db").to_string_lossy().into_owned();
        let expected_thumbnail_path = next
            .join(".event-thumbnails")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            response["config"]["storage"]["recording_catalog_path"].as_str(),
            Some(expected_catalog_path.as_str())
        );
        assert_eq!(
            response["config"]["storage"]["event_thumbnail_path"].as_str(),
            Some(expected_thumbnail_path.as_str())
        );
        let saved: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            saved["storage"]
                .as_table()
                .is_some_and(|storage| !storage.contains_key("recording_catalog_path"))
        );
        assert!(
            saved["storage"]
                .as_table()
                .is_some_and(|storage| !storage.contains_key("event_thumbnail_path"))
        );
        assert_eq!(
            saved["storage_migration"]["medium_term"]["from"].as_str(),
            current.to_str()
        );
        assert_eq!(
            saved["storage_migration"]["medium_term"]["to"].as_str(),
            next.to_str()
        );
        assert_eq!(
            saved["storage_migration"]["long_term"]["from"].as_str(),
            current.to_str()
        );
        assert_eq!(
            saved["storage_migration"]["long_term"]["to"].as_str(),
            next.to_str()
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn runtime_settings_can_schedule_custom_metadata_migrations() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-runtime-custom-storage-migration-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        let current_recordings = directory.join("current-recordings");
        let next_recordings = directory.join("next-recordings");
        let current_catalog = directory.join("current-metadata/recordings.db");
        let next_catalog = directory.join("next-metadata/recordings.db");
        let current_thumbnails = directory.join("current-thumbnails");
        let next_thumbnails = directory.join("next-thumbnails");
        let config = Config {
            host: "0.0.0.0".to_owned(),
            port: 3000,
            storage: StorageToml {
                medium_term_path: Some(current_recordings.to_string_lossy().into_owned()),
                long_term_path: Some(current_recordings.to_string_lossy().into_owned()),
                recording_catalog_path: Some(current_catalog.to_string_lossy().into_owned()),
                event_thumbnail_path: Some(current_thumbnails.to_string_lossy().into_owned()),
                ..StorageToml::default()
            },
            battery_wake: Default::default(),
        };
        crate::config::write_private_file(
            &config_path,
            toml::to_string_pretty(&config).unwrap().as_bytes(),
        )
        .unwrap();
        let storage = StorageConfig::from_toml(&config.storage);
        let state = ServerState::new(
            &config,
            &HashMap::new(),
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        )
        .with_camera_config_path(config_path.clone());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let request = Request::fake_http(
            "PUT",
            "/api/settings/config",
            Vec::new(),
            serde_json::to_vec(&serde_json::json!({
                "host": "0.0.0.0",
                "port": 3000,
                "move_existing_recordings": true,
                "storage": {
                    "medium_term_path": next_recordings,
                    "long_term_path": next_recordings,
                    "recording_catalog_path": next_catalog,
                    "event_thumbnail_path": next_thumbnails,
                    "event_thumbnail_max_mb": 512,
                    "short_term_secs": 120,
                    "medium_term_secs": 1800,
                    "flush_interval_secs": 60,
                    "write_buffer_bytes": 8192,
                    "long_term_max_gb": 0
                }
            }))
            .unwrap(),
        );

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        let saved: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            saved["storage_migration"]["recording_catalog"]["from"].as_str(),
            current_catalog.to_str()
        );
        assert_eq!(
            saved["storage_migration"]["recording_catalog"]["to"].as_str(),
            next_catalog.to_str()
        );
        assert_eq!(
            saved["storage_migration"]["event_thumbnails"]["from"].as_str(),
            current_thumbnails.to_str()
        );
        assert_eq!(
            saved["storage_migration"]["event_thumbnails"]["to"].as_str(),
            next_thumbnails.to_str()
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn settings_restart_requests_graceful_lifecycle_restart() {
        let shutdown = Shutdown::new();
        let restart = Restart::default();
        let state = ServerState::empty().with_restart_control(shutdown.clone(), restart.clone());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let request = Request::fake_http("POST", "/api/settings/restart", Vec::new(), Vec::new());

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&response_data(response)).unwrap()["restarting"],
            true
        );
        assert!(shutdown.is_cancelled());
        assert!(restart.is_requested());
    }

    #[test]
    fn settings_discovery_rejects_excessive_subnets_before_network_probing() {
        let subnets = (0_u8..33).collect::<Vec<_>>();
        let request = Request::fake_http(
            "POST",
            "/api/settings/cameras/discover",
            Vec::new(),
            serde_json::to_vec(&serde_json::json!({ "subnets": subnets })).unwrap(),
        );
        let state = ServerState::empty();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 400);
    }

    #[test]
    fn event_routes_list_and_serve_catalog_owned_thumbnail() {
        let root =
            std::env::temp_dir().join(format!("keeppeek-server-events-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&root).unwrap();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let event_store = EventStore::new(catalog.handle(), &root.join("thumbnails"), 0).unwrap();
        let start_time_ms = datetime!(2026-08-10 12:30 UTC).unix_timestamp() * 1_000;
        event_store
            .insert(TimelineEvent {
                id: "event-1".to_owned(),
                camera_id: "camera-1".to_owned(),
                stream: None,
                source: EventSource::Camera,
                kind: "motion".to_owned(),
                start_time_ms,
                end_time_ms: Some(start_time_ms + 10_000),
                confidence: None,
                bbox: None,
                zone: None,
                thumbnail_filename: None,
            })
            .unwrap();
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 80)
            .encode_image(&DynamicImage::new_rgb8(640, 360))
            .unwrap();
        event_store
            .save_thumbnail("camera-1", "event-1", &jpeg)
            .unwrap();

        let mut state = ServerState::empty().with_event_store(event_store.clone());
        state.recordings = root.clone();
        state.cameras = Arc::new(RwLock::new(vec![CameraEntry {
            info: CameraInfo {
                id: "camera-1".to_owned(),
                ip: "192.0.2.1".to_owned(),
                name: Some("Front Door".to_owned()),
                manufacturer: None,
                model: None,
                firmware_version: None,
                serial_number: None,
                hardware_id: None,
                hostname: None,
                mac_address: None,
                is_reolink: true,
                backend: "reo-proto".to_owned(),
                transport: "tcp".to_owned(),
                web_url: "http://192.0.2.1".to_owned(),
                ports: CameraPorts::default(),
                capabilities: Default::default(),
                profiles: Vec::new(),
            },
            reported_manufacturer: None,
            configuration: CameraConfig {
                ip: "192.0.2.1".parse().unwrap(),
                name: Some("camera-1".to_owned()),
                display_name: Some("Front Door".to_owned()),
                manufacturer: None,
                username: "operator".to_owned(),
                password: "secret".to_owned(),
                onvif_port: None,
                http_port: None,
                main_rtsp_url: None,
                sub_rtsp_url: None,
                uid: None,
                backend: CameraBackend::ReoProto,
                transport: CameraTransport::Tcp,
            },
            recording_label: "front-door".to_owned(),
            control: None,
        }]));
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        let request = Request::fake_http(
            "GET",
            "/api/events/camera-1?date=2026-08-10",
            Vec::new(),
            Vec::new(),
        );
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        let events: RecordingEventsResponse =
            serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(events.events.len(), 1);
        assert_eq!(events.events[0].kind, "motion");
        assert_eq!(
            events.events[0].thumbnail_url.as_deref(),
            Some("/api/events/camera-1/event-1/thumbnail")
        );

        let request = Request::fake_http(
            "GET",
            "/api/events/camera-1/event-1/thumbnail",
            Vec::new(),
            Vec::new(),
        );
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        assert!(response_data(response).starts_with(&[0xff, 0xd8, 0xff]));
        assert_eq!(
            event_thumbnail(&state, "camera-1", "../event-1").status_code,
            400
        );

        drop(state);
        drop(event_store);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
}
