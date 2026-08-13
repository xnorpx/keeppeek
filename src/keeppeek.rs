use crate::{
    api::{CameraId, CameraLifecycle, CameraStatus},
    cameras::{AudioEncoding, Camera, CameraBackend, CameraTransport, VideoEncoding},
    reolink::ReolinkLoop,
    rtsp::{RtspLoop, RtspTransport},
    runtime::{FacadeSender, RouterMessage, WorkerEvent},
    shutdown::Shutdown,
    stats::HealthRegistry,
    storage::{EventStore, StorageHandle, metadata::TimelineEvent},
    webrtc::LiveHandle,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread::JoinHandle,
    time::Duration,
};
use url::Url;

const CHANNEL_BUFFER: usize = 256;
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CAMERA_START_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CameraRoute {
    Retina(RtspTransport),
    ReoProto(CameraTransport),
}

fn resolve_camera_route(
    backend: CameraBackend,
    transport: CameraTransport,
    is_reolink: bool,
) -> anyhow::Result<CameraRoute> {
    let backend = match backend {
        CameraBackend::Auto if is_reolink => CameraBackend::ReoProto,
        CameraBackend::Auto => CameraBackend::Retina,
        backend => backend,
    };
    match (backend, transport) {
        (CameraBackend::Retina, CameraTransport::Tcp) => {
            Ok(CameraRoute::Retina(RtspTransport::Tcp))
        }
        (CameraBackend::Retina, CameraTransport::Udp) => {
            Ok(CameraRoute::Retina(RtspTransport::Udp))
        }
        (CameraBackend::ReoProto, _) if !is_reolink => {
            anyhow::bail!("reo-proto can only be used with Reolink cameras")
        }
        (CameraBackend::ReoProto, transport) => Ok(CameraRoute::ReoProto(transport)),
        (CameraBackend::Auto, _) => unreachable!("automatic backend was resolved above"),
    }
}

fn resolve_configured_camera_route(
    config: &crate::cameras::CameraConfig,
    is_reolink: bool,
) -> anyhow::Result<CameraRoute> {
    if config.backend != CameraBackend::ReoProto && config.has_manual_rtsp_urls() {
        return Ok(CameraRoute::Retina(match config.transport {
            CameraTransport::Tcp => RtspTransport::Tcp,
            CameraTransport::Udp => RtspTransport::Udp,
        }));
    }
    resolve_camera_route(config.backend, config.transport, is_reolink)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    Main,
    Sub,
}

impl Serialize for StreamKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl std::fmt::Display for StreamKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Main => write!(f, "main"),
            Self::Sub => write!(f, "sub"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VideoMeta {
    pub encoding: VideoEncoding,
    pub width: u32,
    pub height: u32,
    pub framerate: f64,
}

#[derive(Debug, Clone)]
pub struct AudioMeta {
    pub encoding: AudioEncoding,
    pub sample_rate: Option<u32>,
}

pub enum KeepPeekEvent {
    StreamConnected {
        camera_ip: IpAddr,
        stream: StreamKind,
    },
    StreamError {
        camera_ip: IpAddr,
        stream: StreamKind,
        error: String,
    },
    TimelineEventStarted {
        event: TimelineEvent,
    },
    TimelineEventEnded {
        id: String,
        end_time_ms: i64,
    },
    TimelineEventThumbnail {
        camera_id: String,
        event_id: String,
        jpeg: Vec<u8>,
    },
}

enum KeepPeekCommand {
    StartCamera {
        camera: Camera,
        reply: SyncSender<anyhow::Result<()>>,
    },
}

/// Requests camera starts from the running KeepPeek loop.
#[derive(Clone)]
pub struct KeepPeekControl {
    tx: Sender<KeepPeekCommand>,
}

impl KeepPeekControl {
    pub fn start_camera(&self, camera: Camera) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(KeepPeekCommand::StartCamera {
                camera,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("KeepPeek loop is no longer running"))?;
        reply_rx
            .recv_timeout(CAMERA_START_TIMEOUT)
            .map_err(|error| {
                anyhow::anyhow!("KeepPeek loop did not accept the camera start request: {error}")
            })?
    }
}

struct CameraStreamStatus {
    id: CameraId,
    expected: HashSet<StreamKind>,
    connected: HashSet<StreamKind>,
    errors: HashMap<StreamKind, String>,
}

impl CameraStreamStatus {
    fn new(id: CameraId) -> Self {
        Self {
            id,
            expected: HashSet::new(),
            connected: HashSet::new(),
            errors: HashMap::new(),
        }
    }

    fn expect(&mut self, stream: StreamKind) {
        self.expected.insert(stream);
    }

    fn connected(&mut self, stream: StreamKind) -> CameraStatus {
        self.connected.insert(stream);
        self.errors.remove(&stream);
        self.snapshot()
    }

    fn error(&mut self, stream: StreamKind, error: String) -> CameraStatus {
        self.connected.remove(&stream);
        self.errors.insert(stream, error);
        self.snapshot()
    }

    fn snapshot(&self) -> CameraStatus {
        let lifecycle =
            if self.expected.is_empty() || (self.connected.is_empty() && self.errors.is_empty()) {
                CameraLifecycle::Starting
            } else if self.connected.len() == self.expected.len() {
                CameraLifecycle::Connected
            } else if self.connected.is_empty() {
                CameraLifecycle::Reconnecting
            } else {
                CameraLifecycle::Degraded
            };
        let last_error = [StreamKind::Main, StreamKind::Sub]
            .into_iter()
            .find_map(|stream| self.errors.get(&stream).cloned());
        CameraStatus {
            id: self.id.clone(),
            lifecycle,
            last_error,
        }
    }
}

pub struct KeepPeekLoop {
    rx: Receiver<KeepPeekEvent>,
    tx: SyncSender<KeepPeekEvent>,
    command_rx: Receiver<KeepPeekCommand>,
    command_tx: Sender<KeepPeekCommand>,
    shutdown: Shutdown,
    handles: Vec<JoinHandle<()>>,
    storage: Option<StorageHandle>,
    events: Option<EventStore>,
    live: Option<LiveHandle>,
    health: HealthRegistry,
    status_tx: Option<FacadeSender<RouterMessage>>,
    stream_statuses: HashMap<IpAddr, CameraStreamStatus>,
}

impl KeepPeekLoop {
    pub fn new(shutdown: Shutdown, storage: Option<StorageHandle>) -> Self {
        let (tx, rx) = mpsc::sync_channel(CHANNEL_BUFFER);
        let (command_tx, command_rx) = mpsc::channel();
        Self {
            rx,
            tx,
            command_rx,
            command_tx,
            shutdown,
            handles: Vec::new(),
            storage,
            events: None,
            live: None,
            health: HealthRegistry::new(),
            status_tx: None,
            stream_statuses: HashMap::new(),
        }
    }

    pub fn control(&self) -> KeepPeekControl {
        KeepPeekControl {
            tx: self.command_tx.clone(),
        }
    }

    pub fn set_live(&mut self, live: LiveHandle) {
        self.live = Some(live);
    }

    pub fn set_event_store(&mut self, events: EventStore) {
        self.events = Some(events);
    }

    pub fn set_health_registry(&mut self, health: HealthRegistry) {
        self.health = health;
    }

    pub fn set_status_sender(&mut self, status_tx: FacadeSender<RouterMessage>) {
        self.status_tx = Some(status_tx);
    }

    fn expect_stream(&mut self, camera_ip: IpAddr, camera_name: Option<&str>, stream: StreamKind) {
        self.stream_statuses
            .entry(camera_ip)
            .or_insert_with(|| {
                CameraStreamStatus::new(CameraId::new(
                    camera_name
                        .map(str::to_owned)
                        .unwrap_or_else(|| camera_ip.to_string()),
                ))
            })
            .expect(stream);
        let status = self
            .stream_statuses
            .get(&camera_ip)
            .expect("camera stream status was just registered")
            .snapshot();
        self.publish_status(status);
    }

    fn publish_status(&self, status: CameraStatus) {
        let Some(status_tx) = &self.status_tx else {
            return;
        };
        if let Err(error) = status_tx.send(RouterMessage::WorkerEvent(WorkerEvent::StatusChanged(
            status,
        ))) {
            tracing::warn!(?error, "unable to publish camera lifecycle");
        }
    }

    /// Profile 0 is treated as Main, all others as Sub.
    pub fn add_cameras(&mut self, cameras: &HashMap<IpAddr, Camera>) -> anyhow::Result<()> {
        for camera in cameras.values() {
            self.add_camera(camera, true, true)?;
        }
        Ok(())
    }

    pub fn add_camera(
        &mut self,
        camera: &Camera,
        enable_main: bool,
        enable_sub: bool,
    ) -> anyhow::Result<()> {
        let route_result = resolve_configured_camera_route(&camera.config, camera.is_reolink);
        let route = route_result.map_err(|error| {
            anyhow::anyhow!(
                "invalid stream configuration for camera '{}': {error}",
                camera.config.name.as_deref().unwrap_or("unnamed"),
            )
        })?;
        match route {
            CameraRoute::Retina(transport) => {
                self.add_rtsp_camera(camera, enable_main, enable_sub, transport);
                if camera.is_reolink {
                    self.add_reolink_event_camera(camera);
                }
            }
            CameraRoute::ReoProto(transport) => {
                let main_video = camera
                    .profiles
                    .first()
                    .and_then(|profile| profile.video.as_ref());
                let sub_video = camera
                    .profiles
                    .get(1)
                    .and_then(|profile| profile.video.as_ref());
                self.add_reolink_camera(
                    camera.config.ip,
                    camera.config.name.clone(),
                    camera.device.manufacturer.clone(),
                    camera
                        .config
                        .uid
                        .clone()
                        .or_else(|| camera.device.p2p_uid.clone()),
                    camera.config.username.clone(),
                    camera.config.password.clone(),
                    transport,
                    0,
                    enable_main,
                    enable_sub,
                    main_video.map_or(0, |video| video.width),
                    main_video.map_or(0, |video| video.height),
                    main_video.map_or(0.0, |video| video.framerate),
                    sub_video.map_or(0, |video| video.width),
                    sub_video.map_or(0, |video| video.height),
                    sub_video.map_or(0.0, |video| video.framerate),
                );
            }
        }
        Ok(())
    }

    fn add_reolink_event_camera(&mut self, camera: &Camera) {
        self.add_reolink_camera(
            camera.config.ip,
            camera.config.name.clone(),
            camera.device.manufacturer.clone(),
            camera
                .config
                .uid
                .clone()
                .or_else(|| camera.device.p2p_uid.clone()),
            camera.config.username.clone(),
            camera.config.password.clone(),
            CameraTransport::Tcp,
            0,
            false,
            false,
            0,
            0,
            0.0,
            0,
            0,
            0.0,
        );
    }

    /// Profile 0 is treated as Main, all others as Sub.
    pub fn add_camera_streams(
        &mut self,
        cameras: &HashMap<IpAddr, Camera>,
        enable_main: bool,
        enable_sub: bool,
    ) {
        self.add_camera_streams_with_transport(
            cameras,
            enable_main,
            enable_sub,
            RtspTransport::Tcp,
        );
    }

    /// Profile 0 is treated as Main, all others as Sub.
    pub fn add_camera_streams_with_transport(
        &mut self,
        cameras: &HashMap<IpAddr, Camera>,
        enable_main: bool,
        enable_sub: bool,
        transport: RtspTransport,
    ) {
        for camera in cameras.values() {
            self.add_rtsp_camera(camera, enable_main, enable_sub, transport);
        }
    }

    fn add_rtsp_camera(
        &mut self,
        camera: &Camera,
        enable_main: bool,
        enable_sub: bool,
        transport: RtspTransport,
    ) {
        for (i, profile) in camera.profiles.iter().enumerate() {
            let stream_kind = if i == 0 {
                StreamKind::Main
            } else {
                StreamKind::Sub
            };
            let manual_stream_uri = match i {
                0 => camera.config.main_rtsp_url.as_ref(),
                1 => camera.config.sub_rtsp_url.as_ref(),
                _ => None,
            };
            let Some(stream_uri) = manual_stream_uri
                .filter(|stream_uri| !stream_uri.trim().is_empty())
                .cloned()
                .or_else(|| profile.stream_uri.clone())
            else {
                continue;
            };
            let video_meta = profile.video.as_ref().map_or_else(
                || VideoMeta {
                    encoding: VideoEncoding::Unknown("rtsp".to_owned()),
                    width: 0,
                    height: 0,
                    framerate: 0.0,
                },
                |video| VideoMeta {
                    encoding: video.encoding.clone(),
                    width: video.width,
                    height: video.height,
                    framerate: video.framerate,
                },
            );
            if (stream_kind == StreamKind::Main && !enable_main)
                || (stream_kind == StreamKind::Sub && !enable_sub)
            {
                continue;
            }
            self.expect_stream(camera.config.ip, camera.config.name.as_deref(), stream_kind);

            let audio_meta = profile.audio.as_ref().map(|a| AudioMeta {
                encoding: a.encoding.clone(),
                sample_rate: a.sample_rate,
            });

            let mut log_url = Url::parse(&stream_uri).ok();
            if let Some(ref mut u) = log_url {
                u.set_username("").ok();
                u.set_password(None).ok();
            }
            tracing::info!(
                ip = %camera.config.ip,
                stream = %stream_kind,
                profile = %profile.name,
                url = %log_url.as_ref().map(|u| u.as_str()).unwrap_or(&stream_uri),
                "spawning rtsp camera loop",
            );

            let camera_loop = RtspLoop {
                camera_ip: camera.config.ip,
                camera_name: camera.config.name.clone(),
                camera_brand: camera.device.manufacturer.clone(),
                camera_port: camera.ports.rtsp.unwrap_or(554),
                stream: stream_kind,
                rtsp_url: stream_uri,
                username: camera.config.username.clone(),
                password: camera.config.password.clone(),
                transport,
                video_meta,
                audio_meta,
                storage: self.storage.clone(),
                live: self.live.clone(),
                health: self.health.clone(),
                tx: self.tx.clone(),
                shutdown: self.shutdown.clone(),
            };

            self.handles.push(
                std::thread::Builder::new()
                    .name(format!("rtsp-{}-{stream_kind}", camera.config.ip))
                    .spawn(move || camera_loop.run())
                    .expect("failed to spawn RTSP camera worker"),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_reolink_camera(
        &mut self,
        camera_ip: IpAddr,
        camera_name: Option<String>,
        camera_brand: Option<String>,
        camera_uid: Option<String>,
        username: String,
        password: String,
        transport: CameraTransport,
        channel: u8,
        enable_main: bool,
        enable_sub: bool,
        main_expected_width: u32,
        main_expected_height: u32,
        main_expected_fps: f64,
        sub_expected_width: u32,
        sub_expected_height: u32,
        sub_expected_fps: f64,
    ) {
        if enable_main {
            self.expect_stream(camera_ip, camera_name.as_deref(), StreamKind::Main);
        }
        if enable_sub {
            self.expect_stream(camera_ip, camera_name.as_deref(), StreamKind::Sub);
        }
        tracing::info!(
            ip = %camera_ip,
            sub_enabled = enable_sub,
            "spawning baichuan single-session main/sub loop",
        );

        let reolink = ReolinkLoop {
            camera_ip,
            camera_name,
            camera_brand,
            camera_uid,
            username,
            password,
            transport,
            channel,
            enable_main,
            enable_sub,
            main_expected_width,
            main_expected_height,
            main_expected_fps,
            sub_expected_width,
            sub_expected_height,
            sub_expected_fps,
            storage: self.storage.clone(),
            live: self.live.clone(),
            health: self.health.clone(),
            tx: self.tx.clone(),
            shutdown: self.shutdown.clone(),
        };
        self.handles.push(
            std::thread::Builder::new()
                .name(format!("baichuan-{camera_ip}"))
                .spawn(move || reolink.run())
                .expect("failed to spawn Baichuan camera worker"),
        );
    }

    pub fn run(mut self) {
        tracing::info!("KeepPeek loop started");

        while !self.shutdown.is_cancelled() {
            self.handle_commands();
            match self.rx.recv_timeout(EVENT_POLL_INTERVAL) {
                Ok(KeepPeekEvent::StreamConnected { camera_ip, stream }) => {
                    tracing::info!(%camera_ip, %stream, "stream connected");
                    let status = self
                        .stream_statuses
                        .get_mut(&camera_ip)
                        .map(|status| status.connected(stream));
                    if let Some(status) = status {
                        self.publish_status(status);
                    }
                }
                Ok(KeepPeekEvent::StreamError {
                    camera_ip,
                    stream,
                    error,
                }) => {
                    tracing::warn!(%camera_ip, %stream, %error, "stream error");
                    let status = self
                        .stream_statuses
                        .get_mut(&camera_ip)
                        .map(|status| status.error(stream, error));
                    if let Some(status) = status {
                        self.publish_status(status);
                    }
                }
                Ok(KeepPeekEvent::TimelineEventStarted { event }) => {
                    if let Some(events) = &self.events {
                        let event_id = event.id.clone();
                        let camera_id = event.camera_id.clone();
                        let kind = event.kind.clone();
                        if let Err(error) = events.insert(event) {
                            tracing::warn!(%event_id, %camera_id, %kind, %error, "unable to store camera event");
                        }
                    }
                }
                Ok(KeepPeekEvent::TimelineEventEnded { id, end_time_ms }) => {
                    if let Some(events) = &self.events
                        && let Err(error) = events.close(&id, end_time_ms)
                    {
                        tracing::warn!(event_id = %id, %error, "unable to close camera event");
                    }
                }
                Ok(KeepPeekEvent::TimelineEventThumbnail {
                    camera_id,
                    event_id,
                    jpeg,
                }) => {
                    if let Some(events) = &self.events
                        && let Err(error) = events.save_thumbnail(&camera_id, &event_id, &jpeg)
                    {
                        tracing::warn!(%camera_id, %event_id, %error, "unable to store event thumbnail");
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        tracing::info!("KeepPeek loop shutting down");
        drop(self.tx);

        for handle in self.handles {
            if handle.join().is_err() {
                tracing::warn!("camera worker panicked");
            }
        }

        tracing::info!("KeepPeek loop stopped");
    }

    fn handle_commands(&mut self) {
        loop {
            let command = match self.command_rx.try_recv() {
                Ok(command) => command,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            match command {
                KeepPeekCommand::StartCamera { camera, reply } => {
                    let result = self.add_camera(&camera, true, true);
                    let _ = reply.send(result);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cameras::{CameraCapabilities, CameraConfig, CameraPorts, DeviceInfo};

    #[test]
    fn automatic_backend_uses_reo_proto_for_reolink() {
        assert_eq!(
            resolve_camera_route(CameraBackend::Auto, CameraTransport::Tcp, true).unwrap(),
            CameraRoute::ReoProto(CameraTransport::Tcp),
        );
        assert_eq!(
            resolve_camera_route(CameraBackend::Auto, CameraTransport::Udp, false).unwrap(),
            CameraRoute::Retina(RtspTransport::Udp),
        );
    }

    #[test]
    fn reo_proto_preserves_configured_transport_for_reolink() {
        assert_eq!(
            resolve_camera_route(CameraBackend::ReoProto, CameraTransport::Udp, true).unwrap(),
            CameraRoute::ReoProto(CameraTransport::Udp),
        );
        assert!(
            resolve_camera_route(CameraBackend::ReoProto, CameraTransport::Tcp, false).is_err()
        );
    }

    #[test]
    fn explicit_reo_proto_takes_precedence_over_saved_rtsp_urls() {
        let config = CameraConfig {
            ip: "192.0.2.89".parse().unwrap(),
            name: Some("reolink".to_owned()),
            display_name: None,
            manufacturer: None,
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: Some("rtsp://192.0.2.89/main".to_owned()),
            sub_rtsp_url: Some("rtsp://192.0.2.89/sub".to_owned()),
            uid: None,
            backend: CameraBackend::ReoProto,
            transport: CameraTransport::Tcp,
        };

        assert_eq!(
            resolve_configured_camera_route(&config, true).unwrap(),
            CameraRoute::ReoProto(CameraTransport::Tcp)
        );
    }

    #[test]
    fn dual_profile_lifecycle_tracks_partial_failure_and_recovery() {
        let mut status = CameraStreamStatus::new(CameraId::new("front_gate"));
        status.expect(StreamKind::Main);
        status.expect(StreamKind::Sub);

        assert_eq!(status.snapshot().lifecycle, CameraLifecycle::Starting);
        assert_eq!(
            status.connected(StreamKind::Main).lifecycle,
            CameraLifecycle::Degraded
        );
        assert_eq!(
            status.connected(StreamKind::Sub).lifecycle,
            CameraLifecycle::Connected
        );

        let degraded = status.error(StreamKind::Main, "main stalled".to_owned());
        assert_eq!(degraded.lifecycle, CameraLifecycle::Degraded);
        assert_eq!(degraded.last_error.as_deref(), Some("main stalled"));

        let reconnecting = status.error(StreamKind::Sub, "sub stalled".to_owned());
        assert_eq!(reconnecting.lifecycle, CameraLifecycle::Reconnecting);
        assert_eq!(
            status.connected(StreamKind::Main).lifecycle,
            CameraLifecycle::Degraded
        );

        let connected = status.connected(StreamKind::Sub);
        assert_eq!(connected.lifecycle, CameraLifecycle::Connected);
        assert_eq!(connected.last_error, None);
    }

    #[test]
    fn control_dispatches_a_new_camera_to_the_running_loop() {
        let shutdown = Shutdown::new();
        let loop_ = KeepPeekLoop::new(shutdown.clone(), None);
        let control = loop_.control();
        let handle = std::thread::spawn(move || loop_.run());

        control
            .start_camera(Camera {
                config: CameraConfig {
                    ip: "192.0.2.88".parse().unwrap(),
                    name: Some("runtime-camera".to_owned()),
                    display_name: None,
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
                device: DeviceInfo::default(),
                reported_manufacturer: None,
                hostname: None,
                mac_address: None,
                ports: CameraPorts::default(),
                capabilities: CameraCapabilities::default(),
                profiles: Vec::new(),
                is_reolink: false,
                ptz: None,
                imaging: None,
            })
            .unwrap();

        shutdown.cancel();
        handle.join().unwrap();
    }
}
