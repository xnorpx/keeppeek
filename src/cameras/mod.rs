mod hikvision;
mod network;
pub mod reolink;

use crate::camera_catalog::common_onvif_probe_ports;
use ipnet::Ipv4Net;
use onvif::{
    discovery,
    soap::client::{AuthType, ClientBuilder, Credentials},
};
use schema::{devicemgmt, media as onvif_media, onvif as onvif_xsd};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use url::Url;

const DEFAULT_DISCOVERY_DURATION: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_CANDIDATE_STREAM_PROBES: usize = 4;
pub(crate) const BAICHUAN_PORT: u16 = 9000;
const RTSP_PORT: u16 = 554;
const PORT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONCURRENT_PORT_PROBES: usize = 200;

pub(crate) struct SessionTimestampNormalizer {
    session_origin: Option<Duration>,
    session_offset: Duration,
    previous_timestamp: Option<Duration>,
    previous_step: Duration,
}

impl SessionTimestampNormalizer {
    pub(crate) const fn new() -> Self {
        Self {
            session_origin: None,
            session_offset: Duration::ZERO,
            previous_timestamp: None,
            previous_step: Duration::ZERO,
        }
    }

    pub(crate) const fn begin_session(&mut self) {
        self.session_origin = None;
    }

    pub(crate) fn normalize(&mut self, timestamp: Duration) -> Duration {
        let Some(session_origin) = self.session_origin else {
            self.session_origin = Some(timestamp);
            self.session_offset = self.previous_timestamp.map_or(Duration::ZERO, |previous| {
                previous.saturating_add(self.previous_step.max(Duration::from_nanos(1)))
            });
            let normalized = self.session_offset;
            if let Some(previous) = self.previous_timestamp {
                self.previous_step = normalized
                    .saturating_sub(previous)
                    .max(Duration::from_nanos(1));
            }
            self.previous_timestamp = Some(normalized);
            return normalized;
        };

        let normalized = self
            .session_offset
            .saturating_add(timestamp.saturating_sub(session_origin));
        if let Some(previous) = self.previous_timestamp
            && normalized > previous
        {
            self.previous_step = normalized - previous;
        }
        self.previous_timestamp = Some(normalized);
        normalized
    }
}

pub(crate) fn local_broadcasts() -> anyhow::Result<Vec<Ipv4Addr>> {
    Ok(network::local_networks()?
        .into_iter()
        .map(|network| network.broadcast)
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CameraDiscoveryNetwork {
    pub(crate) cidr: String,
    pub(crate) interface_name: String,
    pub(crate) preferred: bool,
}

pub(crate) fn camera_discovery_networks() -> anyhow::Result<Vec<CameraDiscoveryNetwork>> {
    let preferred_ip = network::preferred_local_ipv4();
    let mut networks = BTreeMap::<Ipv4Net, CameraDiscoveryNetwork>::new();
    for local in network::local_networks()? {
        if !local.interface_ip.is_private() {
            continue;
        }
        let network = Ipv4Net::new(local.interface_ip, 24)?.trunc();
        let preferred = preferred_ip.is_some_and(|ip| network.contains(&ip));
        networks
            .entry(network)
            .and_modify(|existing| {
                existing.preferred |= preferred;
                if !existing
                    .interface_name
                    .split(", ")
                    .any(|name| name == local.interface_name)
                {
                    existing.interface_name.push_str(", ");
                    existing.interface_name.push_str(&local.interface_name);
                }
            })
            .or_insert_with(|| CameraDiscoveryNetwork {
                cidr: network.to_string(),
                interface_name: local.interface_name,
                preferred,
            });
    }
    let mut networks = networks.into_values().collect::<Vec<_>>();
    networks.sort_by(|left, right| {
        right
            .preferred
            .cmp(&left.preferred)
            .then_with(|| left.cidr.cmp(&right.cidr))
    });
    Ok(networks)
}

pub(crate) fn parallel_filter_map<T, F>(
    ips: Vec<Ipv4Addr>,
    max_workers: usize,
    cancelled: &AtomicBool,
    probe: F,
) -> Vec<T>
where
    T: Send,
    F: Fn(Ipv4Addr) -> Option<T> + Sync,
{
    if ips.is_empty() || max_workers == 0 {
        return Vec::new();
    }

    let jobs = Mutex::new(ips.into_iter().collect::<VecDeque<_>>());
    let results = Mutex::new(Vec::new());
    let worker_count = max_workers.min(jobs.lock().map_or(0, |jobs| jobs.len()));

    thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let ip = match jobs.lock() {
                        Ok(mut jobs) => jobs.pop_front(),
                        Err(poisoned) => poisoned.into_inner().pop_front(),
                    };
                    let Some(ip) = ip else {
                        break;
                    };

                    if let Some(result) = probe(ip) {
                        match results.lock() {
                            Ok(mut results) => results.push(result),
                            Err(poisoned) => poisoned.into_inner().push(result),
                        }
                    }
                }
            });
        }
    });

    match results.into_inner() {
        Ok(results) => results,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredCamera {
    pub ip: IpAddr,
    pub brand: &'static str,
    pub name: Option<String>,
    pub model: Option<String>,
    pub onvif_urls: Vec<Url>,
    pub sources: Vec<&'static str>,
}

#[derive(Debug, Clone)]
pub struct OnvifDevice {
    pub ip: IpAddr,
    pub name: Option<String>,
    pub hardware: Option<String>,
    pub urls: Vec<Url>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CameraConfig {
    pub ip: IpAddr,
    /// Stable configuration key used for recording storage identity.
    pub name: Option<String>,
    /// Human-readable label shown in the KeepPeek interface.
    #[serde(default)]
    pub display_name: Option<String>,
    /// User-selected manufacturer label that takes precedence over camera discovery.
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// ONVIF service port (defaults to 8000 when `None`).
    pub onvif_port: Option<u16>,
    /// HTTP API port used for direct camera controls (defaults to 80 when `None`).
    #[serde(default)]
    pub http_port: Option<u16>,
    /// Explicit RTSP URL for the main stream, taking precedence over ONVIF when set.
    #[serde(default)]
    pub main_rtsp_url: Option<String>,
    /// Explicit RTSP URL for the sub stream, taking precedence over ONVIF when set.
    #[serde(default)]
    pub sub_rtsp_url: Option<String>,
    /// Reolink P2P UID used for direct BCUDP discovery.
    #[serde(default)]
    pub uid: Option<String>,
    #[serde(default)]
    pub backend: CameraBackend,
    #[serde(default)]
    pub transport: CameraTransport,
    #[serde(default)]
    pub record_generic_motion_events: bool,
    #[serde(default)]
    pub recording_mode: CameraRecordingMode,
    #[serde(default = "default_event_recording_duration_secs")]
    pub event_recording_duration_secs: u64,
}

impl fmt::Debug for CameraConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CameraConfig")
            .field("ip", &self.ip)
            .field("name", &self.name)
            .field("display_name", &self.display_name)
            .field("manufacturer", &self.manufacturer)
            .field("username_configured", &!self.username.is_empty())
            .field("password_configured", &!self.password.is_empty())
            .field("onvif_port", &self.onvif_port)
            .field("http_port", &self.http_port)
            .field("main_rtsp_url_configured", &self.main_rtsp_url.is_some())
            .field("sub_rtsp_url_configured", &self.sub_rtsp_url.is_some())
            .field("uid_configured", &self.uid.is_some())
            .field("backend", &self.backend)
            .field("transport", &self.transport)
            .field(
                "record_generic_motion_events",
                &self.record_generic_motion_events,
            )
            .field("recording_mode", &self.recording_mode)
            .field(
                "event_recording_duration_secs",
                &self.event_recording_duration_secs,
            )
            .finish()
    }
}

impl CameraConfig {
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref().or(self.name.as_deref())
    }

    pub fn manufacturer_override(&self) -> Option<&str> {
        self.manufacturer
            .as_deref()
            .map(str::trim)
            .filter(|manufacturer| !manufacturer.is_empty())
    }

    pub fn has_manual_rtsp_urls(&self) -> bool {
        [self.main_rtsp_url.as_deref(), self.sub_rtsp_url.as_deref()]
            .into_iter()
            .flatten()
            .any(|url| configured_rtsp_url(Some(url)).is_some())
    }
}

const fn default_event_recording_duration_secs() -> u64 {
    60
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CameraRecordingMode {
    Off,
    Sub,
    Main,
    Both,
    #[default]
    EventBoost,
}

fn configured_rtsp_url(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CameraBackend {
    #[default]
    Auto,
    Retina,
    ReoProto,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CameraTransport {
    #[default]
    Tcp,
    Udp,
}

#[derive(Debug, Clone)]
pub struct Camera {
    pub config: CameraConfig,
    pub device: DeviceInfo,
    pub reported_manufacturer: Option<String>,
    pub hostname: Option<String>,
    pub mac_address: Option<String>,
    pub ports: CameraPorts,
    pub capabilities: CameraCapabilities,
    pub profiles: Vec<MediaProfile>,
    pub is_reolink: bool,
    pub ptz: Option<PtzInfo>,
    pub imaging: Option<ImagingSettings>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DeviceInfo {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub firmware_version: Option<String>,
    pub serial_number: Option<String>,
    pub hardware_id: Option<String>,
    pub p2p_uid: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CameraPorts {
    pub http: Option<u16>,
    pub https: Option<u16>,
    pub rtsp: Option<u16>,
    pub onvif: Option<u16>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CameraCapabilities {
    pub ptz: bool,
    pub audio: bool,
    pub events: bool,
    pub recording: bool,
    pub analytics: bool,
    pub imaging: bool,
    pub two_way_audio: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaProfile {
    pub token: String,
    pub name: String,
    pub stream_uri: Option<String>,
    pub snapshot_uri: Option<String>,
    pub video: Option<VideoConfig>,
    pub audio: Option<AudioConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProbedStreamUrls {
    pub(crate) onvif_port: Option<u16>,
    pub(crate) main_rtsp_url: Option<String>,
    pub(crate) sub_rtsp_url: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProbedOnvifCamera {
    pub(crate) onvif_port: u16,
    pub(crate) device: DeviceInfo,
    pub(crate) profiles: Vec<MediaProfile>,
    pub(crate) main_rtsp_url: Option<String>,
    pub(crate) sub_rtsp_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoConfig {
    pub encoding: VideoEncoding,
    pub width: u32,
    pub height: u32,
    pub framerate: f64,
    pub bitrate_kbps: Option<u32>,
    pub quality: Option<f64>,
    pub gov_length: Option<u32>,
    pub h264_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoEncoding {
    H264,
    H265,
    JPEG,
    MPEG4,
    Unknown(String),
}

impl std::fmt::Display for VideoEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::H264 => f.write_str("h264"),
            Self::H265 => f.write_str("h265"),
            Self::JPEG => f.write_str("jpeg"),
            Self::MPEG4 => f.write_str("mpeg4"),
            Self::Unknown(v) => f.write_str(v),
        }
    }
}

impl Serialize for VideoEncoding {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioConfig {
    pub encoding: AudioEncoding,
    pub sample_rate: Option<u32>,
    pub bitrate_kbps: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioEncoding {
    G711,
    G726,
    AAC,
    Unknown(String),
}

impl std::fmt::Display for AudioEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::G711 => f.write_str("g711"),
            Self::G726 => f.write_str("g726"),
            Self::AAC => f.write_str("aac"),
            Self::Unknown(v) => f.write_str(v),
        }
    }
}

impl Serialize for AudioEncoding {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PtzInfo {
    pub continuous_move: bool,
    pub absolute_move: bool,
    pub relative_move: bool,
    pub home_support: bool,
    pub e_flip: bool,
    pub reverse: bool,
    pub presets: Vec<PtzPreset>,
    pub preset_tours: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PtzPreset {
    pub token: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImagingSettings {
    pub brightness: Option<f64>,
    pub contrast: Option<f64>,
    pub saturation: Option<f64>,
    pub sharpness: Option<f64>,
    pub ir_cut_filter: Option<IrCutMode>,
    pub backlight_compensation: Option<bool>,
    pub wide_dynamic_range: Option<bool>,
    pub image_stabilization: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrCutMode {
    On,
    Off,
    Auto,
}

impl Serialize for IrCutMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::On => s.serialize_str("on"),
            Self::Off => s.serialize_str("off"),
            Self::Auto => s.serialize_str("auto"),
        }
    }
}

trait CameraBrand {
    fn name(&self) -> &'static str;
    fn claims_device(&self, name: &str, hardware: &str) -> bool;
    fn discover_extra(
        &self,
        already_claimed: &[IpAddr],
        networks: &[network::LocalNetwork],
        cancelled: &AtomicBool,
    ) -> anyhow::Result<Vec<DiscoveredCamera>>;
}

pub fn discover(
    duration: Option<Duration>,
    extra_subnets: &[u8],
) -> anyhow::Result<Vec<DiscoveredCamera>> {
    let targets = network::scan_networks(extra_subnets)?;
    let listeners = network::local_networks()?;
    let cancelled = AtomicBool::new(false);
    discover_scoped(duration, &targets, &listeners, &cancelled, &mut |_| {})
}

pub(crate) fn discover_on_networks_with_progress(
    duration: Option<Duration>,
    networks: &[Ipv4Net],
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(&[DiscoveredCamera]),
) -> anyhow::Result<Vec<DiscoveredCamera>> {
    let targets = network::requested_networks(networks);
    let listeners = network::local_networks_in(&targets)?;
    discover_scoped(duration, &targets, &listeners, cancelled, &mut on_progress)
}

fn discover_scoped(
    duration: Option<Duration>,
    targets: &[network::LocalNetwork],
    listeners: &[network::LocalNetwork],
    cancelled: &AtomicBool,
    on_progress: &mut impl FnMut(&[DiscoveredCamera]),
) -> anyhow::Result<Vec<DiscoveredCamera>> {
    let (batch_tx, batch_rx) = mpsc::channel::<Vec<DiscoveredCamera>>();
    let all = thread::scope(|scope| {
        let tx = batch_tx.clone();
        scope.spawn(move || {
            let reolink = reolink::Reolink;
            let cameras = run_onvif(duration, listeners, cancelled)
                .map(|devices| {
                    devices
                        .into_iter()
                        .map(|device| {
                            let name = device.name.as_deref().unwrap_or("");
                            let hardware = device.hardware.as_deref().unwrap_or("");
                            DiscoveredCamera {
                                ip: device.ip,
                                brand: if reolink.claims_device(name, hardware) {
                                    reolink.name()
                                } else {
                                    "unknown"
                                },
                                name: device.name,
                                model: device.hardware,
                                onvif_urls: device.urls,
                                sources: vec!["onvif"],
                            }
                        })
                        .collect()
                })
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "ONVIF discovery failed");
                    Vec::new()
                });
            let _ = tx.send(cameras);
        });

        let tx = batch_tx.clone();
        scope.spawn(move || {
            let cameras = reolink::Reolink
                .discover_extra(&[], targets, cancelled)
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "Reolink discovery failed");
                    Vec::new()
                });
            let _ = tx.send(cameras);
        });

        let tx = batch_tx.clone();
        scope.spawn(move || {
            let cameras = hikvision::discover(
                duration.unwrap_or(DEFAULT_DISCOVERY_DURATION),
                listeners,
                cancelled,
            )
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "Hikvision SADP discovery failed");
                Vec::new()
            });
            let _ = tx.send(cameras);
        });

        for (port, label, brand, source) in [
            (BAICHUAN_PORT, "Baichuan", "reolink", "baichuan"),
            (RTSP_PORT, "RTSP", "unknown", "rtsp"),
        ] {
            let tx = batch_tx.clone();
            scope.spawn(move || {
                let cameras = run_port_scan(port, label, targets, cancelled)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|ip| DiscoveredCamera {
                        ip,
                        brand,
                        name: None,
                        model: None,
                        onvif_urls: Vec::new(),
                        sources: vec![source],
                    })
                    .collect();
                let _ = tx.send(cameras);
            });
        }
        drop(batch_tx);

        let mut all = Vec::new();
        for cameras in batch_rx {
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            for camera in cameras {
                match all
                    .iter_mut()
                    .find(|existing: &&mut DiscoveredCamera| existing.ip == camera.ip)
                {
                    Some(existing) => merge_camera(existing, camera),
                    None => all.push(camera),
                }
            }
            all.retain(|camera| {
                let IpAddr::V4(ip) = camera.ip else {
                    return false;
                };
                targets.iter().any(|target| target.network.contains(&ip))
            });
            all.sort_by_key(|camera| match camera.ip {
                IpAddr::V4(ip) => u32::from(ip),
                IpAddr::V6(_) => u32::MAX,
            });
            on_progress(&all);
        }
        all
    });
    Ok(all)
}

fn run_onvif(
    duration: Option<Duration>,
    networks: &[network::LocalNetwork],
    cancelled: &AtomicBool,
) -> anyhow::Result<Vec<OnvifDevice>> {
    let duration = duration.unwrap_or(DEFAULT_DISCOVERY_DURATION);
    let devices = Mutex::new(Vec::new());

    thread::scope(|scope| {
        for local in networks {
            let devices = &devices;
            scope.spawn(move || {
                let deadline = std::time::Instant::now() + duration;
                while !cancelled.load(Ordering::Acquire) {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    let result = discovery::DiscoveryBuilder::default()
                        .duration(remaining.min(Duration::from_millis(500)))
                        .listen_address(IpAddr::V4(local.interface_ip))
                        .discover();
                    match result {
                        Ok(found) => match devices.lock() {
                            Ok(mut devices) => devices.extend(found),
                            Err(poisoned) => poisoned.into_inner().extend(found),
                        },
                        Err(error) => tracing::debug!(
                            interface = local.interface_name,
                            ip = %local.interface_ip,
                            "ONVIF discovery attempt failed: {error}"
                        ),
                    }
                }
            });
        }
    });

    let devices = match devices.into_inner() {
        Ok(devices) => devices,
        Err(poisoned) => poisoned.into_inner(),
    };

    let mut by_ip: HashMap<IpAddr, OnvifDevice> = HashMap::new();

    for dev in devices {
        for url in &dev.urls {
            if let Some(host) = url.host_str()
                && let Ok(ip) = host.parse::<IpAddr>()
            {
                let entry = by_ip.entry(ip).or_insert_with(|| OnvifDevice {
                    ip,
                    name: None,
                    hardware: None,
                    urls: vec![],
                });
                entry.name = entry.name.take().or_else(|| dev.name.clone());
                entry.hardware = entry.hardware.take().or_else(|| dev.hardware.clone());
                if !entry.urls.contains(url) {
                    entry.urls.push(url.clone());
                }
            }
        }
    }

    Ok(by_ip.into_values().collect())
}

fn merge_camera(existing: &mut DiscoveredCamera, new: DiscoveredCamera) {
    if existing.brand == "unknown" && new.brand != "unknown" {
        existing.brand = new.brand;
    }
    if existing.name.is_none() {
        existing.name = new.name;
    }
    if existing.model.is_none() {
        existing.model = new.model;
    }
    for url in new.onvif_urls {
        if !existing.onvif_urls.contains(&url) {
            existing.onvif_urls.push(url);
        }
    }
    for src in new.sources {
        if !existing.sources.contains(&src) {
            existing.sources.push(src);
        }
    }
}

fn run_port_scan(
    port: u16,
    label: &str,
    networks: &[network::LocalNetwork],
    cancelled: &AtomicBool,
) -> anyhow::Result<Vec<IpAddr>> {
    let ips = network::scan_targets(networks);
    tracing::info!(
        "{} port scan: scanning {} IPs on port {}",
        label,
        ips.len(),
        port
    );
    let found = parallel_filter_map(ips, MAX_CONCURRENT_PORT_PROBES, cancelled, |ip| {
        probe_tcp_port(ip, port)
    });

    tracing::info!("{} port scan: found {} open ports", label, found.len());
    Ok(found)
}

fn probe_tcp_port(ip: Ipv4Addr, port: u16) -> Option<IpAddr> {
    let addr = SocketAddr::new(IpAddr::V4(ip), port);
    match TcpStream::connect_timeout(&addr, PORT_PROBE_TIMEOUT) {
        Ok(_) => {
            tracing::debug!("TCP port {} open at {}", port, ip);
            Some(IpAddr::V4(ip))
        }
        _ => None,
    }
}

fn query_onvif(config: &CameraConfig) -> anyhow::Result<Camera> {
    let port = config.onvif_port.unwrap_or(8000);
    let url = Url::parse(&format!(
        "http://{}:{}/onvif/device_service",
        config.ip, port
    ))?;

    let creds = Some(Credentials {
        username: config.username.clone(),
        password: config.password.clone(),
    });
    let client = ClientBuilder::new(&url)
        .credentials(creds.clone())
        .auth_type(AuthType::Any)
        .timeout(Duration::from_secs(10))
        .build();

    let services_resp = devicemgmt::get_services(
        &client,
        &devicemgmt::GetServices {
            include_capability: false,
        },
    )
    .map_err(|e| anyhow::anyhow!("ONVIF get_services: {e}"))?;

    let mut ports = CameraPorts {
        onvif: Some(port),
        ..CameraPorts::default()
    };
    for service in &services_resp.service {
        record_endpoint_port(&mut ports, &service.x_addr);
    }

    let dev_resp =
        devicemgmt::get_device_information(&client, &devicemgmt::GetDeviceInformation {})
            .map_err(|e| anyhow::anyhow!("ONVIF get_device_information: {e}"))?;

    let mac_address =
        devicemgmt::get_network_interfaces(&client, &devicemgmt::GetNetworkInterfaces {})
            .ok()
            .and_then(|response| {
                response
                    .network_interfaces
                    .into_iter()
                    .find_map(|interface| {
                        interface
                            .info
                            .map(|info| info.hw_address.0)
                            .filter(|address| !address.trim().is_empty())
                    })
            });

    let hostname = devicemgmt::get_hostname(&client, &devicemgmt::GetHostname {})
        .ok()
        .and_then(|r| {
            let name = r.hostname_information.name;
            match name {
                Some(n) if !n.is_empty() => Some(n),
                _ => None,
            }
        });

    let scope_name = devicemgmt::get_scopes(&client, &devicemgmt::GetScopes {})
        .ok()
        .and_then(|r| {
            for scope in &r.scopes {
                let uri = &scope.scope_item;
                if let Some(name) = uri.strip_prefix("onvif://www.onvif.org/name/") {
                    let decoded = name.replace("%20", " ");
                    if !decoded.is_empty() {
                        return Some(decoded);
                    }
                }
            }
            None
        });

    let hostname = scope_name.or(hostname);

    let device = DeviceInfo {
        manufacturer: Some(dev_resp.manufacturer),
        model: Some(dev_resp.model),
        firmware_version: Some(dev_resp.firmware_version),
        serial_number: Some(dev_resp.serial_number),
        hardware_id: Some(dev_resp.hardware_id),
        p2p_uid: None,
    };

    let media_url = services_resp
        .service
        .iter()
        .find(|s| s.namespace.contains("media/wsdl"))
        .map(|s| s.x_addr.clone());

    let mut profiles = Vec::new();

    if let Some(ref media_url_str) = media_url {
        let parsed = Url::parse(media_url_str)?;
        let media_client = ClientBuilder::new(&parsed)
            .credentials(creds)
            .auth_type(AuthType::Any)
            .timeout(Duration::from_secs(10))
            .build();

        if let Ok(resp) = onvif_media::get_profiles(&media_client, &onvif_media::GetProfiles {}) {
            for p in &resp.profiles {
                let token = p.token.0.clone();
                let name = p.name.0.clone();

                let video = p.video_encoder_configuration.as_ref().map(|v| {
                    let encoding = match v.encoding {
                        onvif_xsd::VideoEncoding::Jpeg => VideoEncoding::JPEG,
                        onvif_xsd::VideoEncoding::Mpeg4 => VideoEncoding::MPEG4,
                        onvif_xsd::VideoEncoding::H264 => VideoEncoding::H264,
                        onvif_xsd::VideoEncoding::__Unknown__(ref s) => {
                            if s.to_lowercase().contains("265") {
                                VideoEncoding::H265
                            } else {
                                VideoEncoding::Unknown(s.clone())
                            }
                        }
                    };
                    let (fps, bitrate) = v
                        .rate_control
                        .as_ref()
                        .map(|rc| (rc.frame_rate_limit as f64, Some(rc.bitrate_limit as u32)))
                        .unwrap_or((0.0, None));

                    let gov_length = v.h264.as_ref().map(|h| h.gov_length as u32);
                    let h264_profile = v.h264.as_ref().map(|h| format!("{:?}", h.h264_profile));

                    VideoConfig {
                        encoding,
                        width: v.resolution.width as u32,
                        height: v.resolution.height as u32,
                        framerate: fps,
                        bitrate_kbps: bitrate,
                        quality: Some(v.quality),
                        gov_length,
                        h264_profile,
                    }
                });

                let audio = p.audio_encoder_configuration.as_ref().map(|a| {
                    let encoding = match a.encoding {
                        onvif_xsd::AudioEncoding::G711 => AudioEncoding::G711,
                        onvif_xsd::AudioEncoding::G726 => AudioEncoding::G726,
                        onvif_xsd::AudioEncoding::Aac => AudioEncoding::AAC,
                        onvif_xsd::AudioEncoding::__Unknown__(ref s) => {
                            AudioEncoding::Unknown(s.clone())
                        }
                    };
                    AudioConfig {
                        encoding,
                        sample_rate: Some(a.sample_rate as u32),
                        bitrate_kbps: Some(a.bitrate as u32),
                    }
                });

                let stream_uri = onvif_media::get_stream_uri(
                    &media_client,
                    &onvif_media::GetStreamUri {
                        stream_setup: onvif_xsd::StreamSetup {
                            stream: onvif_xsd::StreamType::RtpUnicast,
                            transport: onvif_xsd::Transport {
                                protocol: onvif_xsd::TransportProtocol::Rtsp,
                                tunnel: vec![],
                            },
                        },
                        profile_token: onvif_xsd::ReferenceToken(token.clone()),
                    },
                )
                .ok()
                .map(|r| r.media_uri.uri);

                if let Some(stream_uri) = &stream_uri {
                    record_endpoint_port(&mut ports, stream_uri);
                }

                let snapshot_uri = onvif_media::get_snapshot_uri(
                    &media_client,
                    &onvif_media::GetSnapshotUri {
                        profile_token: onvif_xsd::ReferenceToken(token.clone()),
                    },
                )
                .ok()
                .map(|r| r.media_uri.uri);

                profiles.push(MediaProfile {
                    token,
                    name,
                    stream_uri,
                    snapshot_uri,
                    video,
                    audio,
                });
            }
        }
    }

    Ok(Camera {
        config: config.clone(),
        reported_manufacturer: device.manufacturer.clone(),
        device,
        hostname,
        mac_address,
        ports,
        capabilities: CameraCapabilities::default(),
        profiles,
        is_reolink: false,
        ptz: None,
        imaging: None,
    })
}

fn configured_camera(config: &CameraConfig) -> Camera {
    let manufacturer = config.manufacturer_override().map(str::to_owned);
    let mut ports = CameraPorts {
        http: config.http_port,
        onvif: config.onvif_port,
        ..CameraPorts::default()
    };
    let main_rtsp_url = configured_rtsp_url(config.main_rtsp_url.as_deref());
    let sub_rtsp_url = configured_rtsp_url(config.sub_rtsp_url.as_deref());
    for stream_uri in [&main_rtsp_url, &sub_rtsp_url].into_iter().flatten() {
        record_endpoint_port(&mut ports, stream_uri);
    }
    let profiles = [("mainStream", main_rtsp_url), ("subStream", sub_rtsp_url)]
        .into_iter()
        .map(|(name, stream_uri)| MediaProfile {
            token: name.to_owned(),
            name: name.to_owned(),
            stream_uri,
            snapshot_uri: None,
            video: None,
            audio: None,
        })
        .collect();

    Camera {
        config: config.clone(),
        device: DeviceInfo {
            manufacturer: manufacturer.clone(),
            ..DeviceInfo::default()
        },
        reported_manufacturer: manufacturer,
        hostname: None,
        mac_address: None,
        ports,
        capabilities: CameraCapabilities::default(),
        profiles,
        is_reolink: config.backend == CameraBackend::ReoProto
            || config
                .manufacturer_override()
                .is_some_and(|manufacturer| manufacturer.eq_ignore_ascii_case("reolink")),
        ptz: None,
        imaging: None,
    }
}

/// Builds runtime camera descriptions from persisted configuration without network discovery.
pub fn configured_cameras(configs: &HashMap<String, Vec<CameraConfig>>) -> HashMap<IpAddr, Camera> {
    let mut result = HashMap::new();

    for config in configs.values().flatten() {
        if config.username.is_empty() {
            tracing::warn!(ip = %config.ip, "skipping configured camera without credentials");
            continue;
        }
        result.insert(config.ip, configured_camera(config));
    }

    result
}

fn apply_configured_rtsp_urls(camera: &mut Camera) {
    for (index, (name, stream_uri)) in [
        ("mainStream", camera.config.main_rtsp_url.as_deref()),
        ("subStream", camera.config.sub_rtsp_url.as_deref()),
    ]
    .into_iter()
    .enumerate()
    {
        let Some(stream_uri) = configured_rtsp_url(stream_uri) else {
            continue;
        };
        while camera.profiles.len() <= index {
            let profile_name = if camera.profiles.is_empty() {
                "mainStream"
            } else {
                "subStream"
            };
            camera.profiles.push(MediaProfile {
                token: profile_name.to_owned(),
                name: profile_name.to_owned(),
                stream_uri: None,
                snapshot_uri: None,
                video: None,
                audio: None,
            });
        }
        let profile = &mut camera.profiles[index];
        profile.token = name.to_owned();
        profile.name = name.to_owned();
        profile.stream_uri = Some(stream_uri.clone());
        record_endpoint_port(&mut camera.ports, &stream_uri);
    }
}

fn retain_discovered_rtsp_urls(camera: &mut Camera) {
    let streams = probed_stream_urls(&camera.profiles);
    if camera.config.main_rtsp_url.is_none() {
        camera.config.main_rtsp_url = streams.main_rtsp_url;
    }
    if camera.config.sub_rtsp_url.is_none() {
        camera.config.sub_rtsp_url = streams.sub_rtsp_url;
    }
}

pub(crate) fn probe_onvif_camera(config: &CameraConfig) -> anyhow::Result<ProbedOnvifCamera> {
    let candidate_ports = candidate_stream_probe_ports(config.onvif_port);
    if let [port] = candidate_ports.as_slice() {
        return probe_onvif_streams_on_port(config, *port);
    }

    tracing::debug!(
        ip = %config.ip,
        ports = ?candidate_ports,
        "probing candidate ONVIF service ports"
    );
    let worker_count = candidate_ports
        .len()
        .min(MAX_CONCURRENT_CANDIDATE_STREAM_PROBES);
    let ports = Mutex::new(VecDeque::from(candidate_ports));
    let result = Mutex::new(None);
    let failed_ports = Mutex::new(Vec::new());

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let ports = &ports;
            let result = &result;
            let failed_ports = &failed_ports;
            scope.spawn(|| {
                loop {
                    if result
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_some()
                    {
                        return;
                    }
                    let Some(port) = ports
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .pop_front()
                    else {
                        return;
                    };

                    match probe_onvif_streams_on_port(config, port) {
                        Ok(streams) => {
                            let mut result = result
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if result.is_none() {
                                tracing::debug!(
                                    ip = %config.ip,
                                    onvif_port = port,
                                    "candidate ONVIF service responded"
                                );
                                *result = Some(streams);
                            }
                            return;
                        }
                        Err(error) => {
                            tracing::debug!(
                                ip = %config.ip,
                                onvif_port = port,
                                %error,
                                "candidate ONVIF service probe failed"
                            );
                            failed_ports
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push(port);
                        }
                    }
                }
            });
        }
    });

    let result = result
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    result.ok_or_else(|| {
        let failed_ports = failed_ports
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::anyhow!(
            "ONVIF service did not respond on candidate ports {}",
            failed_ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn candidate_stream_probe_ports(onvif_port: Option<u16>) -> Vec<u16> {
    onvif_port.map_or_else(|| common_onvif_probe_ports().to_vec(), |port| vec![port])
}

fn probe_onvif_streams_on_port(
    config: &CameraConfig,
    port: u16,
) -> anyhow::Result<ProbedOnvifCamera> {
    let mut probe_config = config.clone();
    probe_config.onvif_port = Some(port);
    let camera = query_onvif(&probe_config)?;
    let streams = probed_stream_urls(&camera.profiles);
    Ok(ProbedOnvifCamera {
        onvif_port: port,
        device: camera.device,
        profiles: camera.profiles,
        main_rtsp_url: streams.main_rtsp_url,
        sub_rtsp_url: streams.sub_rtsp_url,
    })
}

fn probed_stream_urls(profiles: &[MediaProfile]) -> ProbedStreamUrls {
    let main_rtsp_url = profiles
        .iter()
        .find(|profile| profile_stream(profile) == Some(ProfileStream::Main))
        .or_else(|| profiles.first())
        .and_then(|profile| credential_free_rtsp_url(profile.stream_uri.as_deref()?));
    let sub_rtsp_url = profiles
        .iter()
        .find(|profile| profile_stream(profile) == Some(ProfileStream::Sub))
        .or_else(|| profiles.get(1))
        .and_then(|profile| credential_free_rtsp_url(profile.stream_uri.as_deref()?));
    ProbedStreamUrls {
        onvif_port: None,
        main_rtsp_url,
        sub_rtsp_url,
    }
}

/// Cameras that fail to connect are logged as warnings and skipped.
pub fn query_cameras(configs: &HashMap<String, Vec<CameraConfig>>) -> HashMap<IpAddr, Camera> {
    let mut result = HashMap::new();

    for (namespace, cameras) in configs {
        for config in cameras {
            tracing::info!(
                "querying {} ({}) in namespace '{}'",
                config.name.as_deref().unwrap_or("?"),
                config.ip,
                namespace,
            );

            if config.username.is_empty() {
                tracing::warn!("skipping {} — no credentials", config.ip);
                continue;
            }

            let mut camera = match query_onvif(config) {
                Ok(cam) => cam,
                Err(error) if config.has_manual_rtsp_urls() => {
                    tracing::warn!(
                        ip = %config.ip,
                        %error,
                        "ONVIF query failed; using configured RTSP URLs",
                    );
                    configured_camera(config)
                }
                Err(e) => {
                    tracing::warn!("ONVIF query failed for {}: {}", config.ip, e);
                    continue;
                }
            };

            tracing::info!(
                "ONVIF: {} — {} {}",
                config.ip,
                camera.device.manufacturer.as_deref().unwrap_or("?"),
                camera.device.model.as_deref().unwrap_or("?"),
            );

            let manufacturer = camera
                .device
                .manufacturer
                .as_deref()
                .unwrap_or("")
                .to_lowercase();

            let model = camera.device.model.as_deref().unwrap_or("").to_lowercase();

            let mut is_reolink = config.backend == CameraBackend::ReoProto
                || manufacturer.contains("reolink")
                || model.contains("reolink")
                || model.starts_with("rlc-")
                || model.starts_with("rlc_")
                || model.starts_with("rln-")
                || model.starts_with("rln_");

            if !is_reolink
                && let Ok(ip4) = config.ip.to_string().parse::<std::net::Ipv4Addr>()
                && reolink::probe_reolink_http(ip4)
            {
                tracing::info!(
                    ip = %config.ip,
                    onvif_manufacturer = manufacturer,
                    "ONVIF reports non-Reolink manufacturer but HTTP API responds \u{2014} treating as Reolink",
                );
                is_reolink = true;
            }

            if is_reolink {
                camera.is_reolink = true;
                normalize_reolink_device(&mut camera.device);
                tracing::info!("detected Reolink — querying HTTP API for extras");

                for (i, profile) in camera.profiles.iter().enumerate() {
                    if let Some(ref uri) = profile.stream_uri {
                        tracing::info!(
                            ip = %config.ip,
                            profile_idx = i,
                            name = %profile.name,
                            onvif_stream_uri = %uri,
                            "ONVIF stream URI (before Reolink merge)",
                        );
                    }
                }

                match reolink::ReolinkClient::connect(config) {
                    Ok((_client, reolink_cam)) => {
                        merge_reolink_device(&mut camera.device, reolink_cam.device);
                        camera.mac_address = reolink_cam.mac_address;
                        camera.ports = reolink_cam.ports;
                        camera.capabilities = reolink_cam.capabilities;
                        camera.ptz = reolink_cam.ptz;
                        camera.imaging = reolink_cam.imaging;
                        merge_reolink_profiles(&mut camera.profiles, reolink_cam.profiles);
                    }
                    Err(e) => {
                        tracing::warn!("Reolink HTTP API failed for {}: {}", config.ip, e);
                    }
                }
            }

            camera
                .reported_manufacturer
                .clone_from(&camera.device.manufacturer);
            if let Some(manufacturer) = config.manufacturer_override() {
                camera.device.manufacturer = Some(manufacturer.to_owned());
            }
            apply_configured_rtsp_urls(&mut camera);
            retain_discovered_rtsp_urls(&mut camera);

            result.insert(config.ip, camera);
        }
    }

    result
}

fn record_endpoint_port(ports: &mut CameraPorts, endpoint: &str) {
    let Ok(url) = Url::parse(endpoint) else {
        return;
    };
    let (destination, default_port) = match url.scheme() {
        "http" => (&mut ports.http, 80),
        "https" => (&mut ports.https, 443),
        "rtsp" => (&mut ports.rtsp, 554),
        "rtsps" => (&mut ports.rtsp, 322),
        _ => return,
    };
    destination.get_or_insert(url.port().unwrap_or(default_port));
}

fn normalize_reolink_device(device: &mut DeviceInfo) {
    device.manufacturer = Some("Reolink".to_owned());
}

fn merge_reolink_device(device: &mut DeviceInfo, discovered: DeviceInfo) {
    normalize_reolink_device(device);
    device.model = discovered.model.or_else(|| device.model.take());
    device.firmware_version = discovered
        .firmware_version
        .or_else(|| device.firmware_version.take());
    device.serial_number = discovered
        .serial_number
        .or_else(|| device.serial_number.take());
    device.hardware_id = discovered.hardware_id.or_else(|| device.hardware_id.take());
    device.p2p_uid = discovered.p2p_uid.or_else(|| device.p2p_uid.take());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileStream {
    Main,
    Sub,
}

fn profile_stream(profile: &MediaProfile) -> Option<ProfileStream> {
    profile_stream_from_name(&profile.name, &profile.token)
}

fn profile_stream_from_name(name: &str, token: &str) -> Option<ProfileStream> {
    let name = name.to_ascii_lowercase();
    let token = token.to_ascii_lowercase();

    if name.contains("main") || token.ends_with("_main") {
        Some(ProfileStream::Main)
    } else if name.contains("sub") || token.ends_with("_sub") {
        Some(ProfileStream::Sub)
    } else {
        None
    }
}

fn credential_free_rtsp_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "rtsp" | "rtsps") || url.host_str().is_none() {
        return None;
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    Some(url.into())
}

fn merge_reolink_profiles(
    onvif_profiles: &mut Vec<MediaProfile>,
    reolink_profiles: Vec<MediaProfile>,
) {
    for mut reolink_profile in reolink_profiles {
        let stream = profile_stream(&reolink_profile);
        let onvif_index = stream.and_then(|stream| {
            onvif_profiles
                .iter()
                .position(|profile| profile_stream(profile) == Some(stream))
        });

        if let Some(onvif_profile) = onvif_index.and_then(|index| onvif_profiles.get(index)) {
            reolink_profile
                .stream_uri
                .clone_from(&onvif_profile.stream_uri);
        }

        if stream == Some(ProfileStream::Main)
            && let Some(video) = &reolink_profile.video
            && let Some(stream_uri) = &reolink_profile.stream_uri
            && let Ok(mut url) = Url::parse(stream_uri)
        {
            match video.encoding {
                VideoEncoding::H264 => url.set_path("/h264Preview_01_main"),
                VideoEncoding::H265 => url.set_path("/h265Preview_01_main"),
                _ => {}
            }
            reolink_profile.stream_uri = Some(url.into());
        }

        if let Some(index) = onvif_index {
            onvif_profiles[index] = reolink_profile;
        } else {
            onvif_profiles.push(reolink_profile);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_stream_probe_ports_prioritize_common_http_and_preserve_overrides() {
        let automatic = candidate_stream_probe_ports(None);
        assert_eq!(automatic.first(), Some(&80));
        assert!(automatic.contains(&8000));
        assert_eq!(candidate_stream_probe_ports(Some(8899)), vec![8899]);
    }

    #[test]
    fn session_timestamps_start_at_zero_and_join_reconnects() {
        let mut timestamps = SessionTimestampNormalizer::new();

        assert_eq!(
            timestamps.normalize(Duration::from_secs(10)),
            Duration::ZERO
        );
        assert_eq!(
            timestamps.normalize(Duration::from_secs(11)),
            Duration::from_secs(1)
        );

        timestamps.begin_session();

        assert_eq!(
            timestamps.normalize(Duration::from_secs(42)),
            Duration::from_secs(2)
        );
        assert_eq!(
            timestamps.normalize(Duration::from_secs(43)),
            Duration::from_secs(3)
        );
    }

    fn profile(name: &str, encoding: VideoEncoding, stream_uri: &str) -> MediaProfile {
        MediaProfile {
            token: name.to_string(),
            name: name.to_string(),
            stream_uri: Some(stream_uri.to_string()),
            snapshot_uri: None,
            video: Some(VideoConfig {
                encoding,
                width: 0,
                height: 0,
                framerate: 0.0,
                bitrate_kbps: None,
                quality: None,
                gov_length: None,
                h264_profile: None,
            }),
            audio: None,
        }
    }

    #[test]
    fn reolink_profiles_enrich_onvif_with_extra_profile() {
        let mut onvif_profiles = vec![
            profile(
                "Profile000_MainStream",
                VideoEncoding::H264,
                "rtsp://camera/",
            ),
            profile(
                "Profile001_SubStream",
                VideoEncoding::H264,
                "rtsp://camera/h264Preview_01_sub",
            ),
            profile(
                "Profile002_Extra",
                VideoEncoding::H264,
                "rtsp://camera/extra",
            ),
        ];
        let reolink_profiles = vec![
            profile("mainStream", VideoEncoding::H265, ""),
            profile("subStream", VideoEncoding::H264, ""),
        ];

        merge_reolink_profiles(&mut onvif_profiles, reolink_profiles);

        assert_eq!(onvif_profiles.len(), 3);
        assert_eq!(
            onvif_profiles[0].video.as_ref().unwrap().encoding,
            VideoEncoding::H265
        );
        assert_eq!(
            onvif_profiles[0].stream_uri.as_deref(),
            Some("rtsp://camera/h265Preview_01_main")
        );
        assert_eq!(
            onvif_profiles[1].stream_uri.as_deref(),
            Some("rtsp://camera/h264Preview_01_sub")
        );
        assert_eq!(onvif_profiles[2].name, "Profile002_Extra");
    }

    #[test]
    fn configured_rtsp_urls_create_direct_main_and_sub_profiles() {
        let config = CameraConfig {
            ip: "192.0.2.77".parse().unwrap(),
            name: Some("manual".to_owned()),
            display_name: None,
            manufacturer: None,
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: Some(8000),
            http_port: None,
            main_rtsp_url: Some("rtsp://192.0.2.77:8554/live/main".to_owned()),
            sub_rtsp_url: Some("rtsp://192.0.2.77:8554/live/sub".to_owned()),
            uid: None,
            backend: CameraBackend::Auto,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: CameraRecordingMode::Sub,
            event_recording_duration_secs: 60,
        };

        let camera = configured_camera(&config);

        assert!(camera.config.has_manual_rtsp_urls());
        assert_eq!(camera.ports.rtsp, Some(8554));
        assert_eq!(camera.profiles[0].name, "mainStream");
        assert_eq!(
            camera.profiles[0].stream_uri.as_deref(),
            Some("rtsp://192.0.2.77:8554/live/main")
        );
        assert_eq!(camera.profiles[1].name, "subStream");
        assert_eq!(
            camera.profiles[1].stream_uri.as_deref(),
            Some("rtsp://192.0.2.77:8554/live/sub")
        );
    }

    #[test]
    fn configured_reo_proto_camera_needs_no_discovered_endpoints() {
        let config = CameraConfig {
            ip: "192.0.2.78".parse().unwrap(),
            name: Some("reolink".to_owned()),
            display_name: None,
            manufacturer: None,
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: Some("test-uid".to_owned()),
            backend: CameraBackend::ReoProto,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: CameraRecordingMode::Sub,
            event_recording_duration_secs: 60,
        };
        let configs = HashMap::from([("cameras".to_owned(), vec![config])]);

        let cameras = configured_cameras(&configs);
        let camera = cameras.get(&"192.0.2.78".parse().unwrap()).unwrap();

        assert!(camera.is_reolink);
        assert_eq!(camera.config.backend, CameraBackend::ReoProto);
        assert_eq!(camera.profiles.len(), 2);
        assert!(
            camera
                .profiles
                .iter()
                .all(|profile| profile.stream_uri.is_none())
        );
    }

    #[test]
    fn configured_retina_camera_uses_persisted_stream_endpoints() {
        let config = CameraConfig {
            ip: "192.0.2.80".parse().unwrap(),
            name: Some("retina".to_owned()),
            display_name: None,
            manufacturer: None,
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: Some("rtsp://192.0.2.80/main".to_owned()),
            sub_rtsp_url: Some("rtsp://192.0.2.80/sub".to_owned()),
            uid: None,
            backend: CameraBackend::Retina,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: CameraRecordingMode::Sub,
            event_recording_duration_secs: 60,
        };
        let configs = HashMap::from([("cameras".to_owned(), vec![config])]);

        let cameras = configured_cameras(&configs);
        let camera = cameras.get(&"192.0.2.80".parse().unwrap()).unwrap();

        assert!(!camera.is_reolink);
        assert_eq!(camera.config.backend, CameraBackend::Retina);
        assert_eq!(
            camera.profiles[0].stream_uri.as_deref(),
            Some("rtsp://192.0.2.80/main")
        );
        assert_eq!(
            camera.profiles[1].stream_uri.as_deref(),
            Some("rtsp://192.0.2.80/sub")
        );
    }

    #[test]
    fn discovered_rtsp_urls_are_retained_without_credentials() {
        let config = CameraConfig {
            ip: "192.0.2.79".parse().unwrap(),
            name: Some("retina".to_owned()),
            display_name: None,
            manufacturer: None,
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: Some(80),
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Retina,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: CameraRecordingMode::Sub,
            event_recording_duration_secs: 60,
        };
        let mut camera = Camera {
            config,
            device: DeviceInfo::default(),
            reported_manufacturer: None,
            hostname: None,
            mac_address: None,
            ports: CameraPorts::default(),
            capabilities: CameraCapabilities::default(),
            profiles: vec![
                profile(
                    "mainStream",
                    VideoEncoding::H265,
                    "rtsp://embedded:credential@192.0.2.79/main",
                ),
                profile(
                    "subStream",
                    VideoEncoding::H264,
                    "rtsp://embedded:credential@192.0.2.79/sub",
                ),
            ],
            is_reolink: false,
            ptz: None,
            imaging: None,
        };

        let streams = probed_stream_urls(&camera.profiles);
        assert_eq!(
            streams.main_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.79/main")
        );
        assert_eq!(
            streams.sub_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.79/sub")
        );

        retain_discovered_rtsp_urls(&mut camera);

        assert_eq!(
            camera.config.main_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.79/main")
        );
        assert_eq!(
            camera.config.sub_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.79/sub")
        );
    }

    #[test]
    fn configured_rtsp_url_overrides_only_its_matching_onvif_profile() {
        let config = CameraConfig {
            ip: "192.0.2.77".parse().unwrap(),
            name: Some("manual".to_owned()),
            display_name: None,
            manufacturer: None,
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: Some(8000),
            http_port: None,
            main_rtsp_url: Some("rtsp://192.0.2.77:8554/manual-main".to_owned()),
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Auto,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: CameraRecordingMode::Sub,
            event_recording_duration_secs: 60,
        };
        let mut camera = Camera {
            config,
            device: DeviceInfo::default(),
            reported_manufacturer: None,
            hostname: None,
            mac_address: None,
            ports: CameraPorts::default(),
            capabilities: CameraCapabilities::default(),
            profiles: vec![
                profile(
                    "mainStream",
                    VideoEncoding::H264,
                    "rtsp://192.0.2.77/onvif-main",
                ),
                profile(
                    "subStream",
                    VideoEncoding::H264,
                    "rtsp://192.0.2.77/onvif-sub",
                ),
            ],
            is_reolink: false,
            ptz: None,
            imaging: None,
        };

        apply_configured_rtsp_urls(&mut camera);

        assert_eq!(
            camera.profiles[0].stream_uri.as_deref(),
            Some("rtsp://192.0.2.77:8554/manual-main")
        );
        assert_eq!(
            camera.profiles[1].stream_uri.as_deref(),
            Some("rtsp://192.0.2.77/onvif-sub")
        );
        assert_eq!(camera.ports.rtsp, Some(8554));
    }

    #[test]
    fn reolink_device_metadata_replaces_generic_onvif_manufacturer() {
        let mut onvif_device = DeviceInfo {
            manufacturer: Some("Manufacturer".to_owned()),
            model: Some("RLC-820A".to_owned()),
            firmware_version: Some("onvif-firmware".to_owned()),
            serial_number: Some("onvif-serial".to_owned()),
            hardware_id: None,
            p2p_uid: None,
        };
        let reolink_device = DeviceInfo {
            manufacturer: Some("Reolink".to_owned()),
            model: None,
            firmware_version: Some("reolink-firmware".to_owned()),
            serial_number: None,
            hardware_id: Some("reolink-hardware".to_owned()),
            p2p_uid: None,
        };

        merge_reolink_device(&mut onvif_device, reolink_device);

        assert_eq!(onvif_device.manufacturer.as_deref(), Some("Reolink"));
        assert_eq!(onvif_device.model.as_deref(), Some("RLC-820A"));
        assert_eq!(
            onvif_device.firmware_version.as_deref(),
            Some("reolink-firmware")
        );
        assert_eq!(onvif_device.serial_number.as_deref(), Some("onvif-serial"));
        assert_eq!(
            onvif_device.hardware_id.as_deref(),
            Some("reolink-hardware")
        );
    }

    #[test]
    fn onvif_endpoints_produce_known_service_ports() {
        let mut ports = CameraPorts {
            onvif: Some(8000),
            ..CameraPorts::default()
        };

        record_endpoint_port(&mut ports, "http://camera.example/onvif/device_service");
        record_endpoint_port(
            &mut ports,
            "https://camera.example:8443/onvif/media_service",
        );
        record_endpoint_port(&mut ports, "rtsp://camera.example/live");
        record_endpoint_port(&mut ports, "not a valid endpoint");

        assert_eq!(ports.http, Some(80));
        assert_eq!(ports.https, Some(8443));
        assert_eq!(ports.rtsp, Some(554));
        assert_eq!(ports.onvif, Some(8000));
    }
}
