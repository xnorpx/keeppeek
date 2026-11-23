use crate::{
    api::{CameraId, CameraLifecycle, CameraStatus},
    battery_wake::BatteryWakeHandle,
    cameras::{AudioEncoding, Camera, CameraBackend, CameraTransport, VideoEncoding},
    event_forwarder::{Handle as EventForwarderHandle, model::EventTransition},
    notifications::{
        Handle as NotificationHandle, Lifecycle as NotificationLifecycle,
        Stage as NotificationStage,
        model::{Candidate as NotificationCandidate, Severity, Trigger},
    },
    privacy::PrivacyRegistry,
    reolink::ReolinkLoop,
    rtsp::{RtspLoop, RtspTransport},
    runtime::{FacadeSender, RouterMessage, WorkerEvent},
    shutdown::Shutdown,
    stats::HealthRegistry,
    storage::{EventStore, StorageHandle, metadata::TimelineEvent},
    webrtc::Publisher,
};
use recording::{PendingCameraRecording, recording_request_state};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::IpAddr,
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

mod profile_identity;
mod recording;
pub(crate) use profile_identity::CameraProfileIds;

const CHANNEL_BUFFER: usize = 256;
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const CAMERA_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
// Allow an in-flight camera request (up to five seconds) to finish before the control deadline.
const CAMERA_STOP_BUDGET: Duration = Duration::from_secs(6);

pub(crate) type ProfileApplicationResult =
    anyhow::Result<Vec<crate::profiles::CameraRuntimeApplication>>;

type EventPublisher = Box<dyn Fn(&TimelineEvent) + Send + Sync>;

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

mod events;
mod isapi;
mod native;
use events::event_only_update;

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

#[derive(Clone)]
pub enum KeepPeekEvent {
    NativeBatch {
        owner: uuid::Uuid,
        lifetime: std::sync::Arc<std::sync::atomic::AtomicBool>,
        changes: Vec<Self>,
        reply: SyncSender<usize>,
    },
    IsapiBatch {
        fence: Option<crate::isapi::callbacks::Fence>,
        changes: Vec<Self>,
        reply: SyncSender<usize>,
    },
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
        event: Box<TimelineEvent>,
    },
    TimelineEventImages {
        event: Box<TimelineEvent>,
        images: Vec<(String, std::sync::Arc<[u8]>)>,
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
    SuspendForPrivacy {
        ip: IpAddr,
        reply: SyncSender<anyhow::Result<()>>,
    },
    Stop {
        ip: IpAddr,
        reply: SyncSender<anyhow::Result<()>>,
    },
    StartCamera {
        camera: Camera,
        reply: SyncSender<anyhow::Result<()>>,
    },
    RestartCamera {
        camera: Camera,
        reply: SyncSender<anyhow::Result<()>>,
    },
    SetRuntimeProfiles {
        profiles: crate::profiles::ProfileRegistry,
        camera_ids: Option<CameraProfileIds>,
        reply: SyncSender<ProfileApplicationResult>,
    },
}

/// Controls cameras owned by the running KeepPeek loop.
#[derive(Clone)]
pub struct KeepPeekControl {
    tx: Sender<KeepPeekCommand>,
    runtime_profile_status:
        std::sync::Arc<std::sync::RwLock<crate::profiles::RuntimeProfileStatus>>,
}

impl KeepPeekControl {
    pub(crate) fn runtime_profile_status_handle(
        &self,
    ) -> std::sync::Arc<std::sync::RwLock<crate::profiles::RuntimeProfileStatus>> {
        self.runtime_profile_status.clone()
    }

    pub(crate) fn submit_privacy_stop(
        &self,
        ip: IpAddr,
    ) -> anyhow::Result<Receiver<anyhow::Result<()>>> {
        let (reply, received) = mpsc::sync_channel(1);
        self.tx
            .send(KeepPeekCommand::SuspendForPrivacy { ip, reply })
            .map_err(|_| anyhow::anyhow!("KeepPeek loop is no longer running"))?;
        Ok(received)
    }

    /// Stops a camera's media and event sources before acknowledging removal.
    pub fn stop_camera(&self, ip: IpAddr) -> anyhow::Result<()> {
        let (reply, received) = mpsc::sync_channel(1);
        self.tx
            .send(KeepPeekCommand::Stop { ip, reply })
            .map_err(|_| anyhow::anyhow!("KeepPeek loop is no longer running"))?;
        received
            .recv_timeout(CAMERA_CONTROL_TIMEOUT)
            .map_err(|_| anyhow::anyhow!("KeepPeek loop did not acknowledge camera removal"))?
    }
    pub fn start_camera(&self, camera: Camera) -> anyhow::Result<()> {
        self.submit_start_camera(camera)?
            .recv_timeout(CAMERA_CONTROL_TIMEOUT)
            .map_err(|error| {
                anyhow::anyhow!("KeepPeek loop did not accept the camera start request: {error}")
            })?
    }

    pub(crate) fn submit_start_camera(
        &self,
        camera: Camera,
    ) -> anyhow::Result<Receiver<anyhow::Result<()>>> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(KeepPeekCommand::StartCamera {
                camera,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("KeepPeek loop is no longer running"))?;
        Ok(reply_rx)
    }

    pub fn restart_camera(&self, camera: Camera) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(KeepPeekCommand::RestartCamera {
                camera,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("KeepPeek loop is no longer running"))?;
        reply_rx
            .recv_timeout(CAMERA_CONTROL_TIMEOUT)
            .map_err(|error| {
                anyhow::anyhow!("KeepPeek loop did not accept the camera restart request: {error}")
            })?
    }

    pub fn set_runtime_profiles(
        &self,
        profiles: crate::profiles::ProfileRegistry,
    ) -> anyhow::Result<Vec<crate::profiles::CameraRuntimeApplication>> {
        self.request_runtime_profiles(profiles, None)
    }

    pub(crate) fn set_runtime_profiles_with_camera_ids(
        &self,
        profiles: crate::profiles::ProfileRegistry,
        camera_ids: BTreeMap<IpAddr, String>,
    ) -> anyhow::Result<Vec<crate::profiles::CameraRuntimeApplication>> {
        self.request_runtime_profiles(profiles, Some(CameraProfileIds::new(camera_ids)?))
    }

    fn request_runtime_profiles(
        &self,
        profiles: crate::profiles::ProfileRegistry,
        camera_ids: Option<CameraProfileIds>,
    ) -> anyhow::Result<Vec<crate::profiles::CameraRuntimeApplication>> {
        self.submit_runtime_profiles(profiles, camera_ids)?
            .recv_timeout(CAMERA_CONTROL_TIMEOUT)
            .map_err(|error| {
                anyhow::anyhow!("KeepPeek loop did not acknowledge profile activation: {error}")
            })?
    }

    pub(crate) fn submit_runtime_profiles(
        &self,
        profiles: crate::profiles::ProfileRegistry,
        camera_ids: Option<CameraProfileIds>,
    ) -> anyhow::Result<Receiver<ProfileApplicationResult>> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(KeepPeekCommand::SetRuntimeProfiles {
                profiles,
                camera_ids,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("KeepPeek loop is no longer running"))?;
        Ok(reply_rx)
    }
}

struct CameraWorkers {
    shutdown: Shutdown,
    handles: Vec<JoinHandle<()>>,
    event_shutdown: Shutdown,
    event_handles: Vec<JoinHandle<()>>,
    configuration: Option<crate::cameras::CameraConfig>,
}

impl CameraWorkers {
    fn new() -> Self {
        Self {
            shutdown: Shutdown::new(),
            handles: Vec::new(),
            event_shutdown: Shutdown::new(),
            event_handles: Vec::new(),
            configuration: None,
        }
    }

    fn cancel(&self) {
        self.shutdown.cancel();
        self.event_shutdown.cancel();
    }

    fn is_finished(&self) -> bool {
        self.handles
            .iter()
            .chain(&self.event_handles)
            .all(JoinHandle::is_finished)
    }

    fn join(self, camera_ip: IpAddr) {
        for handle in self.handles.into_iter().chain(self.event_handles) {
            if handle.join().is_err() {
                tracing::warn!(%camera_ip, "camera worker panicked");
            }
        }
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
        let mut expected_streams = self
            .expected
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        expected_streams.sort_unstable();
        let mut connected_streams = self
            .connected
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        connected_streams.sort_unstable();
        CameraStatus {
            id: self.id.clone(),
            lifecycle,
            expected_streams,
            connected_streams,
            last_error,
        }
    }
}

enum InactiveCameraReason {
    AwaitingStart,
    MissingCredentials,
    PrivacyPaused(crate::cameras::CameraRecordingMode),
    PrivacyFailed(String),
}

struct InactiveCamera {
    recording_mode: crate::cameras::CameraRecordingMode,
    reason: InactiveCameraReason,
}

impl InactiveCamera {
    const fn from_config(config: &crate::cameras::CameraConfig) -> Self {
        Self {
            recording_mode: config.recording_mode,
            reason: if config.username.is_empty() {
                InactiveCameraReason::MissingCredentials
            } else {
                InactiveCameraReason::AwaitingStart
            },
        }
    }

    fn after_privacy_stop(
        config: &crate::cameras::CameraConfig,
        application: Option<&crate::profiles::CameraRuntimeApplication>,
        configured_mode: crate::cameras::CameraRecordingMode,
    ) -> Self {
        let mut camera = Self::from_config(config);
        if matches!(camera.reason, InactiveCameraReason::AwaitingStart)
            && let Some(row) = application
        {
            use crate::profiles::RuntimeApplicationState;
            camera.reason = match row.state {
                RuntimeApplicationState::Applied => {
                    InactiveCameraReason::PrivacyPaused(configured_mode)
                }
                RuntimeApplicationState::Degraded => InactiveCameraReason::PrivacyFailed(
                    row.reason
                        .clone()
                        .unwrap_or_else(|| "camera runtime degraded before privacy pause".into()),
                ),
                RuntimeApplicationState::Pending => InactiveCameraReason::AwaitingStart,
            };
        }
        camera
    }

    const fn acknowledged_recording_mode(&self) -> Option<crate::cameras::CameraRecordingMode> {
        match self.reason {
            InactiveCameraReason::PrivacyPaused(mode) => Some(mode),
            _ => None,
        }
    }

    fn application_state(
        &self,
        policy: &crate::profiles::EffectiveCameraPolicy,
        configured_mode: crate::cameras::CameraRecordingMode,
    ) -> Result<crate::profiles::RuntimeApplicationState, String> {
        use crate::profiles::RuntimeApplicationState;
        match self.reason {
            InactiveCameraReason::MissingCredentials => {
                Err("camera credentials are not configured".into())
            }
            InactiveCameraReason::PrivacyFailed(reason) => Err(reason.clone()),
            InactiveCameraReason::PrivacyPaused(acknowledged_mode)
                if acknowledged_mode == configured_mode
                    && (policy.privacy_enforced || !policy.available) =>
            {
                Ok(RuntimeApplicationState::Applied)
            }
            InactiveCameraReason::AwaitingStart | InactiveCameraReason::PrivacyPaused(_) => {
                Ok(RuntimeApplicationState::Pending)
            }
        }
    }
}

pub struct KeepPeekLoop {
    rx: Receiver<KeepPeekEvent>,
    tx: SyncSender<KeepPeekEvent>,
    command_rx: Receiver<KeepPeekCommand>,
    command_tx: Sender<KeepPeekCommand>,
    shutdown: Shutdown,
    camera_workers: HashMap<IpAddr, CameraWorkers>,
    retiring_workers: HashMap<IpAddr, CameraWorkers>,
    camera_definitions: HashMap<IpAddr, Camera>,
    inactive_configurations: HashMap<IpAddr, InactiveCamera>,
    deferred_camera_starts: HashMap<IpAddr, (bool, bool)>,
    privacy_suspended: HashSet<IpAddr>,
    deferred_privacy_confirmation: Option<IpAddr>,
    recovered_profile_publisher: Option<Box<dyn Fn(u64) + Send + Sync>>,
    isapi_callbacks: Option<crate::isapi::callbacks::Runtime>,
    storage: Option<StorageHandle>,
    events: Option<EventStore>,
    event_publisher: Option<EventPublisher>,
    native_commits: native::Commits,
    live: Option<Publisher>,
    health: HealthRegistry,
    status_tx: Option<FacadeSender<RouterMessage>>,
    stream_statuses: HashMap<IpAddr, CameraStreamStatus>,
    battery_uids: HashMap<IpAddr, String>,
    battery_wake: Option<BatteryWakeHandle>,
    notifications: Option<NotificationHandle>,
    event_forwarder: Option<EventForwarderHandle>,
    privacy: Option<std::sync::Arc<PrivacyRegistry>>,
    camera_names: HashMap<String, String>,
    runtime_profiles: crate::profiles::ProfileRegistry,
    runtime_profile_camera_ids: CameraProfileIds,
    runtime_profile_notification_error: Option<String>,
    runtime_profile_notification_sources: std::collections::BTreeSet<String>,
    runtime_profile_status_publisher: Option<Box<dyn Fn() + Send + Sync>>,
    runtime_profile_status:
        std::sync::Arc<std::sync::RwLock<crate::profiles::RuntimeProfileStatus>>,
    next_recording_status_check: std::time::Instant,
    recording_status_cursor: Option<String>,
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
            camera_workers: HashMap::new(),
            retiring_workers: HashMap::new(),
            camera_definitions: HashMap::new(),
            inactive_configurations: HashMap::new(),
            deferred_camera_starts: HashMap::new(),
            privacy_suspended: HashSet::new(),
            deferred_privacy_confirmation: None,
            recovered_profile_publisher: None,
            isapi_callbacks: None,
            storage,
            events: None,
            event_publisher: None,
            native_commits: native::Commits::default(),
            live: None,
            health: HealthRegistry::new(),
            status_tx: None,
            stream_statuses: HashMap::new(),
            battery_uids: HashMap::new(),
            battery_wake: None,
            notifications: None,
            event_forwarder: None,
            privacy: None,
            camera_names: HashMap::new(),
            runtime_profiles: crate::profiles::ProfileRegistry::default(),
            runtime_profile_camera_ids: CameraProfileIds::default(),
            runtime_profile_notification_error: None,
            runtime_profile_notification_sources: Default::default(),
            runtime_profile_status_publisher: None,
            runtime_profile_status: std::sync::Arc::new(std::sync::RwLock::new(
                crate::profiles::RuntimeProfileStatus::default(),
            )),
            next_recording_status_check: std::time::Instant::now(),
            recording_status_cursor: None,
        }
    }

    pub fn control(&self) -> KeepPeekControl {
        KeepPeekControl {
            tx: self.command_tx.clone(),
            runtime_profile_status: self.runtime_profile_status.clone(),
        }
    }

    pub fn set_live(&mut self, live: Publisher) {
        self.live = Some(live);
    }

    pub(crate) fn set_privacy_registry(&mut self, privacy: std::sync::Arc<PrivacyRegistry>) {
        self.privacy = Some(privacy);
    }

    pub fn set_runtime_profiles(&mut self, profiles: crate::profiles::ProfileRegistry) {
        self.runtime_profiles = profiles;
        self.apply_notification_suppression(None);
        let mut status = self
            .runtime_profile_status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        status.set_selection(&self.runtime_profiles);
        status.cameras.clear();
    }

    fn apply_notification_suppression(
        &mut self,
        fence: Option<crate::notifications::ProfileRevisionFence>,
    ) {
        let desired = active_notification_suppressions(
            &self.runtime_profiles,
            &self.runtime_profile_camera_ids,
        );
        let result = match &self.notifications {
            Some(handle) => handle
                .set_profile_suppressed_sources(desired.clone(), fence)
                .map(|_| ()),
            None if desired.is_empty() => Ok(()),
            None => Err(anyhow::anyhow!("notification runtime is unavailable")),
        };
        self.runtime_profile_notification_error = result.err().map(|error| error.to_string());
        if self.runtime_profile_notification_error.is_some() {
            // A timeout can race application. Both newly requested and previously suppressed
            // sources have uncertain notification state until a later acknowledgement.
            self.runtime_profile_notification_sources.retain(|source| {
                source
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| self.camera_definitions.contains_key(&ip))
            });
            self.runtime_profile_notification_sources.extend(desired);
        } else {
            self.runtime_profile_notification_sources = desired;
        }
    }

    pub(crate) fn set_runtime_profile_camera_ids(
        &mut self,
        camera_ids: BTreeMap<IpAddr, String>,
    ) -> anyhow::Result<()> {
        self.runtime_profile_camera_ids = CameraProfileIds::new(camera_ids)?;
        Ok(())
    }

    pub(crate) fn set_runtime_profile_status_publisher(
        &mut self,
        publish: impl Fn() + Send + Sync + 'static,
    ) {
        self.runtime_profile_status_publisher = Some(Box::new(publish));
    }

    pub(crate) fn set_recovered_profile_publisher(
        &mut self,
        publish: impl Fn(u64) + Send + Sync + 'static,
    ) {
        self.recovered_profile_publisher = Some(Box::new(publish));
    }

    fn publish_runtime_profile_status(&self) {
        if let Some(publish) = &self.runtime_profile_status_publisher {
            publish();
        }
    }

    pub fn set_event_store(&mut self, events: EventStore) {
        self.events = Some(events);
    }

    pub(crate) fn set_event_publisher(
        &mut self,
        publish: impl Fn(&TimelineEvent) + Send + Sync + 'static,
    ) {
        self.event_publisher = Some(Box::new(publish));
    }

    pub fn set_health_registry(&mut self, health: HealthRegistry) {
        self.health = health;
    }

    pub fn set_status_sender(&mut self, status_tx: FacadeSender<RouterMessage>) {
        self.status_tx = Some(status_tx);
    }

    pub fn set_battery_wake(&mut self, battery_wake: BatteryWakeHandle) {
        self.battery_wake = Some(battery_wake);
    }

    pub(crate) fn set_notifications(&mut self, notifications: NotificationHandle) {
        self.notifications = Some(notifications);
    }

    pub(crate) fn set_event_forwarder(&mut self, event_forwarder: EventForwarderHandle) {
        self.event_forwarder = Some(event_forwarder);
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
    /// Seed the inventory before the HTTP listener can expose the first observation snapshot.
    pub(crate) fn seed_profile_observations<'a>(
        &mut self,
        configs: impl Iterator<Item = &'a crate::cameras::CameraConfig>,
    ) {
        self.inactive_configurations = configs
            .map(|config| (config.ip, InactiveCamera::from_config(config)))
            .collect();
        let observations = self.inactive_profile_observations();
        let mut status = self
            .runtime_profile_status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        status.set_selection(&self.runtime_profiles);
        status.cameras = observations
            .into_iter()
            .map(|row| (row.camera_id.clone(), row))
            .collect();
    }

    fn inactive_profile_observations(&self) -> Vec<crate::profiles::CameraRuntimeApplication> {
        let configured_ids = self
            .camera_definitions
            .keys()
            .chain(self.inactive_configurations.keys())
            .map(|ip| self.runtime_profile_camera_ids.camera_id(*ip))
            .collect();
        let mut observations = self
            .runtime_profiles
            .missing_camera_applications(&configured_ids);
        for (&ip, camera) in &self.inactive_configurations {
            let policy = self.profile_policy_for(ip, camera.recording_mode);
            let (configured_mode, _) = crate::profiles::configured_recording_mode(
                camera.recording_mode,
                self.camera_override(ip),
            );
            let mut state = camera.application_state(&policy, configured_mode);
            if matches!(state, Ok(crate::profiles::RuntimeApplicationState::Applied))
                && configured_mode != crate::cameras::CameraRecordingMode::Off
                && self.recording_writer_failed(&ip.to_string())
            {
                state = Err(recording::ACTIVE_RECORDING_WRITE_FAILURE.into());
            }
            observations.push(self.camera_runtime_application(ip, policy, state));
        }
        observations
    }

    pub fn add_cameras(&mut self, cameras: &HashMap<IpAddr, Camera>) -> anyhow::Result<()> {
        let publisher = self.runtime_profile_status_publisher.take();
        let mut pending = Vec::with_capacity(cameras.len());
        let result = cameras
            .values()
            .try_for_each(|camera| self.begin_add_camera(camera, true, true, &mut pending));
        self.finish_camera_recordings(pending);
        self.runtime_profile_status_publisher = publisher;
        self.reconcile_missing_profile_targets();
        self.publish_runtime_profile_status();
        result
    }

    fn reconcile_missing_profile_targets(&self) -> bool {
        let configured_ids = self
            .camera_definitions
            .keys()
            .map(|ip| self.runtime_profile_camera_ids.camera_id(*ip))
            .collect::<HashSet<_>>();
        let inactive = self.inactive_profile_observations();
        let mut status = self
            .runtime_profile_status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if status.revision > self.runtime_profiles.revision {
            return false;
        }
        status
            .cameras
            .retain(|camera_id, _| configured_ids.contains(camera_id));
        status.cameras.extend(
            inactive
                .into_iter()
                .map(|application| (application.camera_id.clone(), application)),
        );
        true
    }

    pub fn add_camera(
        &mut self,
        camera: &Camera,
        enable_main: bool,
        enable_sub: bool,
    ) -> anyhow::Result<()> {
        let mut pending = Vec::with_capacity(1);
        let result = self.begin_add_camera(camera, enable_main, enable_sub, &mut pending);
        self.finish_camera_recordings(pending);
        result
    }

    fn begin_add_camera<'a>(
        &mut self,
        camera: &'a Camera,
        enable_main: bool,
        enable_sub: bool,
        pending: &mut Vec<PendingCameraRecording<'a>>,
    ) -> anyhow::Result<()> {
        self.reap_stopped_cameras();
        if self.camera_workers.contains_key(&camera.config.ip) {
            anyhow::bail!("camera '{}' is already running", camera.config.ip)
        }
        self.camera_definitions
            .insert(camera.config.ip, camera.clone());
        self.inactive_configurations.remove(&camera.config.ip);
        if let Some(name) = &camera.config.name {
            self.camera_names
                .insert(camera.config.ip.to_string(), name.clone());
            self.camera_names.insert(name.clone(), name.clone());
        }
        if let Err(error) = self.ensure_worker_slot(camera) {
            self.deferred_camera_starts
                .insert(camera.config.ip, (enable_main, enable_sub));
            self.record_camera_runtime_status(camera, Err(error.to_string()));
            return Err(error);
        }
        self.begin_camera_change(camera, pending, |recorder| {
            recorder.start_camera_runtime(camera, enable_main, enable_sub)
        })
    }

    fn begin_camera_change<'a>(
        &mut self,
        camera: &'a Camera,
        pending: &mut Vec<PendingCameraRecording<'a>>,
        apply: impl FnOnce(&mut Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let recording = self.configure_profile_recording(camera)?;
        let result = apply(self);
        let runtime_result = result.as_ref().map_err(ToString::to_string).map(|_| ());
        self.record_camera_runtime_state(
            camera,
            runtime_result
                .clone()
                .and_then(|()| recording_request_state(&recording)),
        );
        if let Ok(Some(configuration)) = recording {
            pending.push(PendingCameraRecording {
                camera,
                configuration,
                runtime_result,
            });
        }
        result
    }

    fn start_camera_runtime(
        &mut self,
        camera: &Camera,
        enable_main: bool,
        enable_sub: bool,
    ) -> anyhow::Result<()> {
        self.ensure_worker_slot(camera)?;
        let route = resolve_configured_camera_route(&camera.config, camera.is_reolink).map_err(
            |error| {
                anyhow::anyhow!(
                    "invalid stream configuration for camera '{}': {error}",
                    camera.config.name.as_deref().unwrap_or("unnamed"),
                )
            },
        )?;
        if !self.effective_camera_policy(&camera.config).available
            || self.remain_privacy_suspended(camera.config.ip)?
        {
            self.deferred_camera_starts.remove(&camera.config.ip);
            return Ok(());
        }
        let status_handle = self.runtime_profile_status.clone();
        let status = status_handle
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(
            status.revision <= self.runtime_profiles.revision,
            "runtime profile transition was superseded"
        );
        if let Some(uid) = camera
            .config
            .uid
            .clone()
            .or_else(|| camera.device.p2p_uid.clone())
        {
            self.battery_uids.insert(camera.config.ip, uid);
        }
        self.camera_workers
            .insert(camera.config.ip, CameraWorkers::new());
        self.camera_workers
            .get_mut(&camera.config.ip)
            .expect("registered camera workers")
            .configuration = Some(camera.config.clone());
        self.add_camera_workers(camera, route, enable_main, enable_sub);
        self.deferred_camera_starts.remove(&camera.config.ip);
        drop(status);
        Ok(())
    }

    fn record_camera_runtime_status(&self, camera: &Camera, result: Result<(), String>) {
        self.record_camera_runtime_state(
            camera,
            result.map(|()| crate::profiles::RuntimeApplicationState::Applied),
        );
    }

    fn effective_recording_mode(
        &self,
        camera: &crate::cameras::CameraConfig,
    ) -> crate::cameras::CameraRecordingMode {
        effective_recording_mode(
            &self.runtime_profiles,
            camera,
            &self.runtime_profile_camera_ids.camera_id(camera.ip),
        )
    }

    fn camera_override(&self, ip: IpAddr) -> Option<&crate::profiles::CameraProfileOverride> {
        self.runtime_profiles
            .active_camera_override(&self.runtime_profile_camera_ids.camera_id(ip))
    }

    fn held_privacy_exceeds_profile(&self, ip: IpAddr) -> bool {
        self.camera_override(ip).and_then(|p| p.privacy_enforced) != Some(true)
            && self
                .privacy
                .as_ref()
                .is_some_and(|p| p.profile_overrides_snapshot().get(&ip.to_string()) == Some(&true))
    }

    fn remain_privacy_suspended(&self, ip: IpAddr) -> anyhow::Result<bool> {
        if !self.privacy_suspended.contains(&ip) {
            return Ok(false);
        }
        let Some(schedule) = self
            .privacy
            .as_ref()
            .and_then(|p| p.schedule(&ip.to_string()))
        else {
            return Ok(false);
        };
        Ok(!schedule.keep_camera_connected
            && (self.camera_override(ip).and_then(|p| p.privacy_enforced) == Some(true)
                || schedule.is_active(chrono::Utc::now())?))
    }

    fn effective_camera_policy(
        &self,
        camera: &crate::cameras::CameraConfig,
    ) -> crate::profiles::EffectiveCameraPolicy {
        self.profile_policy_for(camera.ip, camera.recording_mode)
    }

    fn profile_policy_for(
        &self,
        ip: IpAddr,
        recording_mode: crate::cameras::CameraRecordingMode,
    ) -> crate::profiles::EffectiveCameraPolicy {
        // The media gate retains old restrictions until the server commits the transition.
        // Report the accepted profile against the actual schedule, without releasing that gate.
        let scheduled = self
            .privacy
            .as_ref()
            .is_some_and(|privacy| privacy.schedule_active(&ip.to_string(), chrono::Utc::now()));
        let base = crate::profiles::BaseCameraPolicy {
            available: true,
            recording_mode,
            privacy_enforced: scheduled,
            notifications_enabled: true,
        };
        crate::profiles::resolve_camera_policy(&base, self.camera_override(ip))
    }

    fn begin_runtime_profiles(
        &mut self,
        profiles: crate::profiles::ProfileRegistry,
        camera_ids: Option<CameraProfileIds>,
    ) -> anyhow::Result<Vec<Camera>> {
        profiles.validate()?;
        anyhow::ensure!(
            profiles.revision > self.runtime_profiles.revision || profiles == self.runtime_profiles,
            "runtime profile revision is stale or conflicts with the accepted registry"
        );
        let status_handle = self.runtime_profile_status.clone();
        let mut status = status_handle
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(
            status.revision <= profiles.revision,
            "runtime profile transition was superseded"
        );
        if let Some(camera_ids) = camera_ids {
            self.runtime_profile_camera_ids = camera_ids;
        }
        self.runtime_profiles = profiles;
        let cameras = self
            .camera_definitions
            .values()
            .cloned()
            .collect::<Vec<_>>();
        status.set_selection(&self.runtime_profiles);
        status.cameras.extend(
            self.inactive_profile_observations()
                .into_iter()
                .map(|row| (row.camera_id.clone(), row)),
        );
        for camera in &cameras {
            let camera_id = self.runtime_profile_camera_ids.camera_id(camera.config.ip);
            status.cameras.insert(
                camera_id.clone(),
                crate::profiles::CameraRuntimeApplication {
                    camera_id,
                    state: crate::profiles::RuntimeApplicationState::Pending,
                    policy: self.effective_camera_policy(&camera.config),
                    reason: None,
                },
            );
        }
        drop(status);
        self.apply_notification_suppression(Some(crate::notifications::ProfileRevisionFence {
            status: status_handle.clone(),
            revision: self.runtime_profiles.revision,
        }));
        anyhow::ensure!(
            status_handle
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .revision
                == self.runtime_profiles.revision,
            "runtime profile transition was superseded"
        );
        Ok(cameras)
    }

    #[cfg(test)]
    fn apply_runtime_profiles(
        &mut self,
        profiles: crate::profiles::ProfileRegistry,
    ) -> anyhow::Result<Vec<crate::profiles::CameraRuntimeApplication>> {
        self.apply_runtime_profiles_with_camera_ids(profiles, None)
    }

    fn apply_runtime_profiles_with_camera_ids(
        &mut self,
        profiles: crate::profiles::ProfileRegistry,
        camera_ids: Option<CameraProfileIds>,
    ) -> anyhow::Result<Vec<crate::profiles::CameraRuntimeApplication>> {
        // Coalesce camera results so a large batch cannot exhaust watch acknowledgements.
        let publisher = self.runtime_profile_status_publisher.take();
        let result = self.apply_runtime_profiles_inner(profiles, camera_ids);
        self.runtime_profile_status_publisher = publisher;
        if result.is_ok() {
            self.publish_runtime_profile_status();
        }
        result
    }

    fn apply_runtime_profiles_inner(
        &mut self,
        profiles: crate::profiles::ProfileRegistry,
        camera_ids: Option<CameraProfileIds>,
    ) -> anyhow::Result<Vec<crate::profiles::CameraRuntimeApplication>> {
        self.reap_stopped_cameras();
        let cameras = self.begin_runtime_profiles(profiles, camera_ids)?;
        let mut applications = Vec::with_capacity(cameras.len());
        let mut pending = Vec::with_capacity(cameras.len());
        let stop_deadline = std::time::Instant::now() + CAMERA_STOP_BUDGET;
        self.cancel_unavailable_camera_workers(&cameras)?;
        for camera in cameras {
            let recording = match self.configure_profile_recording(&camera) {
                Ok(recording) => recording,
                Err(error) => {
                    self.finish_profile_recordings(&mut applications, pending);
                    return Err(error);
                }
            };
            let policy = self.effective_camera_policy(&camera.config);
            let runtime_result = if policy.available {
                self.apply_available_camera(&camera)
            } else {
                self.finish_camera_stop(camera.config.ip, stop_deadline)
            };
            let runtime_result = runtime_result
                .map_err(|error| error.to_string())
                .and_then(|()| recording_request_state(&recording));
            if let Ok(Some(configuration)) = recording {
                pending.push((applications.len(), configuration));
            }
            applications.push(self.camera_runtime_application(
                camera.config.ip,
                policy,
                runtime_result,
            ));
        }
        self.finish_profile_recordings(&mut applications, pending);
        applications.extend(self.inactive_profile_observations());
        self.complete_runtime_profiles(&applications)?;
        Ok(applications)
    }

    fn cancel_unavailable_camera_workers(&mut self, cameras: &[Camera]) -> anyhow::Result<()> {
        let status_handle = self.runtime_profile_status.clone();
        let status = status_handle
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(
            status.revision == self.runtime_profiles.revision,
            "runtime profile transition was superseded"
        );
        for camera in cameras {
            if !self.effective_camera_policy(&camera.config).available {
                self.begin_camera_stop(camera.config.ip);
            }
        }
        drop(status);
        Ok(())
    }

    fn apply_available_camera(&mut self, camera: &Camera) -> anyhow::Result<()> {
        let Some(workers) = self.camera_workers.get(&camera.config.ip) else {
            if self.ensure_worker_slot(camera).is_err() {
                self.deferred_camera_starts
                    .entry(camera.config.ip)
                    .or_insert((true, true));
            }
            return self.start_camera_runtime(camera, true, true);
        };
        if workers.event_shutdown.is_cancelled() {
            anyhow::ensure!(
                workers.event_handles.iter().all(JoinHandle::is_finished),
                "previous camera event workers are still stopping"
            );
            // A persisted event edit can outlive its first stop deadline.
            let status_handle = self.runtime_profile_status.clone();
            let status = status_handle
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            anyhow::ensure!(
                status.revision == self.runtime_profiles.revision,
                "runtime profile transition was superseded"
            );
            let route = resolve_configured_camera_route(&camera.config, camera.is_reolink)?;
            self.replace_camera_events(camera, route)?;
            drop(status);
        }
        Ok(())
    }

    fn complete_runtime_profiles(
        &self,
        applications: &[crate::profiles::CameraRuntimeApplication],
    ) -> anyhow::Result<()> {
        let mut status = self
            .runtime_profile_status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(
            status.revision <= self.runtime_profiles.revision,
            "runtime profile transition was superseded"
        );
        status.set_selection(&self.runtime_profiles);
        status.cameras = applications
            .iter()
            .cloned()
            .map(|application| (application.camera_id.clone(), application))
            .collect();
        Ok(())
    }

    fn add_camera_workers(
        &mut self,
        camera: &Camera,
        route: CameraRoute,
        enable_main: bool,
        enable_sub: bool,
    ) {
        self.add_event_workers(camera, route);
        match route {
            CameraRoute::Retina(transport) => {
                self.add_rtsp_camera(camera, enable_main, enable_sub, transport);
            }
            CameraRoute::ReoProto(transport) => {
                self.add_reolink_media_camera(camera, enable_main, enable_sub, transport);
            }
        }
    }

    fn add_event_workers(&mut self, camera: &Camera, route: CameraRoute) {
        use crate::cameras::events::EventMode;
        let vendor = matches!(
            camera.config.events.mode,
            EventMode::Auto | EventMode::Vendor
        );
        let known_vendor = camera.is_reolink || crate::isapi::Route::for_camera(camera).is_some();
        let event_shutdown = self.camera_workers[&camera.config.ip]
            .event_shutdown
            .clone();
        self.health.events.configure(
            camera.config.ip,
            camera.config.events.clone(),
            camera.config.record_generic_motion_events,
            event_shutdown.clone(),
        );
        if matches!(
            camera.config.events.mode,
            EventMode::OnvifPullpoint | EventMode::RtspMetadata
        ) || camera.config.events.mode == EventMode::Auto && !known_vendor
        {
            let workers = self
                .camera_workers
                .get_mut(&camera.config.ip)
                .expect("camera worker group exists");
            match crate::camera_events::spawn(
                camera,
                self.tx.clone(),
                self.health.events.clone(),
                self.privacy.clone().unwrap_or_default(),
                event_shutdown,
            ) {
                Ok(handles) => workers.event_handles.extend(handles),
                Err(_) => {
                    tracing::warn!(camera_ip = %camera.config.ip, "unable to start native event workers");
                }
            }
        }
        if vendor {
            if camera.is_reolink && matches!(route, CameraRoute::Retina(_)) {
                self.add_reolink_event_camera(camera);
            } else if !camera.is_reolink {
                self.add_isapi_event_camera(camera);
            }
        }
    }

    fn add_reolink_media_camera(
        &mut self,
        camera: &Camera,
        enable_main: bool,
        enable_sub: bool,
        transport: CameraTransport,
    ) {
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
            camera.config.record_generic_motion_events,
        );
    }

    fn add_isapi_event_camera(&mut self, camera: &Camera) {
        if let Some(callbacks) = &self.isapi_callbacks
            && callbacks.contains(camera.config.ip)
        {
            callbacks.activate(camera.config.ip, camera.config.record_generic_motion_events);
            return;
        }
        let workers = self
            .camera_workers
            .get_mut(&camera.config.ip)
            .expect("camera workers were registered before event startup");
        let result = if camera.config.events.mode == crate::cameras::events::EventMode::Auto {
            let fallback_camera = camera.clone();
            let sent = self.tx.clone();
            let registry = self.health.events.clone();
            let privacy = self.privacy.clone().unwrap_or_default();
            let shutdown = workers.event_shutdown.clone();
            crate::isapi::spawn_then(
                camera,
                self.tx.clone(),
                self.storage.clone(),
                shutdown.clone(),
                move || {
                    tracing::info!(camera_ip = %fallback_camera.config.ip, "ISAPI unsupported; switching to generic ONVIF events");
                    match crate::camera_events::spawn(
                        &fallback_camera,
                        sent,
                        registry,
                        privacy,
                        shutdown,
                    ) {
                        Ok(handles) => {
                            for handle in handles {
                                if handle.join().is_err() {
                                    tracing::warn!("generic fallback event worker panicked");
                                }
                            }
                        }
                        Err(_) => tracing::warn!("unable to start generic fallback event workers"),
                    }
                },
            )
        } else {
            crate::isapi::spawn(
                camera,
                self.tx.clone(),
                self.storage.clone(),
                workers.event_shutdown.clone(),
            )
        };
        match result {
            Ok(Some(handle)) => workers.event_handles.push(handle),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(camera_ip = %camera.config.ip, %error, "unable to start ISAPI event worker");
            }
        }
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
            camera.config.record_generic_motion_events,
        );
        let workers = self
            .camera_workers
            .get_mut(&camera.config.ip)
            .expect("registered Reolink event workers");
        let handle = workers
            .handles
            .pop()
            .expect("Reolink event worker was just started");
        workers.event_handles.push(handle);
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
        let worker_shutdown = self
            .camera_workers
            .entry(camera.config.ip)
            .or_insert_with(CameraWorkers::new)
            .shutdown
            .clone();
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
                timing_camera_id: uuid::Uuid::parse_str(
                    &self.runtime_profile_camera_ids.camera_id(camera.config.ip),
                )
                .ok(),
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
                http_port: camera.config.http_port.or(camera.ports.http).unwrap_or(80),
                talkback_channel: crate::isapi::Route::for_camera(camera)
                    .map(|route| route.channel),
                storage: self.storage.clone(),
                live: self.live.clone(),
                health: self.health.clone(),
                tx: self.tx.clone(),
                shutdown: worker_shutdown.clone(),
            };

            let handle = std::thread::Builder::new()
                .name(format!("rtsp-{}-{stream_kind}", camera.config.ip))
                .spawn(move || camera_loop.run())
                .expect("failed to spawn RTSP camera worker");
            self.camera_workers
                .get_mut(&camera.config.ip)
                .expect("camera worker group was registered before spawning")
                .handles
                .push(handle);
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
        record_generic_motion_events: bool,
    ) {
        let workers = self
            .camera_workers
            .entry(camera_ip)
            .or_insert_with(CameraWorkers::new);
        let worker_shutdown = if enable_main || enable_sub {
            workers.shutdown.clone()
        } else {
            workers.event_shutdown.clone()
        };
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
            record_generic_motion_events,
            storage: self.storage.clone(),
            live: self.live.clone(),
            health: self.health.clone(),
            tx: self.tx.clone(),
            shutdown: worker_shutdown,
            battery_wake: self.battery_wake.clone(),
        };
        let handle = std::thread::Builder::new()
            .name(format!("baichuan-{camera_ip}"))
            .spawn(move || reolink.run())
            .expect("failed to spawn Baichuan camera worker");
        self.camera_workers
            .get_mut(&camera_ip)
            .expect("camera worker group was registered before spawning")
            .handles
            .push(handle);
    }

    fn restart_camera(&mut self, camera: &Camera) -> anyhow::Result<()> {
        let route = resolve_configured_camera_route(&camera.config, camera.is_reolink)?;
        self.camera_definitions
            .insert(camera.config.ip, camera.clone());
        if self
            .camera_workers
            .get(&camera.config.ip)
            .and_then(|workers| workers.configuration.as_ref())
            .is_some_and(|previous| event_only_update(previous, &camera.config))
        {
            let mut pending = Vec::with_capacity(1);
            let result = self.begin_camera_change(camera, &mut pending, |recorder| {
                recorder.replace_camera_events(camera, route)
            });
            self.finish_camera_recordings(pending);
            return result;
        }
        if let Err(error) = self.stop_camera(camera.config.ip) {
            self.deferred_camera_starts
                .insert(camera.config.ip, (true, true));
            self.record_camera_runtime_status(camera, Err(error.to_string()));
            return Err(error);
        }
        self.add_camera(camera, true, true)
    }

    fn replace_camera_events(&mut self, camera: &Camera, route: CameraRoute) -> anyhow::Result<()> {
        if let Err(error) = self.stop_camera_events(camera.config.ip) {
            self.deferred_camera_starts
                .insert(camera.config.ip, (true, true));
            return Err(error);
        }
        self.deferred_camera_starts.remove(&camera.config.ip);
        self.camera_workers
            .get_mut(&camera.config.ip)
            .expect("existing camera workers")
            .configuration = Some(camera.config.clone());
        self.add_event_workers(camera, route);
        Ok(())
    }

    fn stop_camera_events(&mut self, camera_ip: IpAddr) -> anyhow::Result<()> {
        if let Some(callbacks) = &self.isapi_callbacks {
            callbacks.deactivate(camera_ip);
        }
        let Some(workers) = self.camera_workers.get_mut(&camera_ip) else {
            return Ok(());
        };
        workers.event_shutdown.cancel();
        let handles = std::mem::take(&mut workers.event_handles);
        let finished = self.wait_for_camera_workers(
            std::time::Instant::now() + CAMERA_STOP_BUDGET,
            None,
            || handles.iter().all(JoinHandle::is_finished),
        );
        if !finished {
            self.camera_workers
                .get_mut(&camera_ip)
                .expect("media workers retained during event replacement")
                .event_handles = handles;
            anyhow::bail!("camera event workers did not stop before the deadline");
        }
        for handle in handles {
            if handle.join().is_err() {
                tracing::warn!(%camera_ip, "camera event worker panicked");
            }
        }
        self.close_retired_native_events();
        self.health.events.remove(camera_ip);
        self.camera_workers
            .get_mut(&camera_ip)
            .expect("media workers retained during event replacement")
            .event_shutdown = Shutdown::new();
        Ok(())
    }

    fn stop_camera(&mut self, camera_ip: IpAddr) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.retiring_workers.contains_key(&camera_ip),
            "previous camera workers are still stopping"
        );
        let deadline = std::time::Instant::now() + CAMERA_STOP_BUDGET;
        self.begin_camera_stop(camera_ip);
        self.finish_camera_stop(camera_ip, deadline)
    }

    fn begin_camera_stop(&mut self, camera_ip: IpAddr) {
        if let Some(callbacks) = &self.isapi_callbacks {
            callbacks.deactivate(camera_ip);
        }
        if let Some(workers) = self.camera_workers.remove(&camera_ip) {
            workers.cancel();
            self.retiring_workers.insert(camera_ip, workers);
        }
    }

    fn clear_stopped_camera_state(&mut self, camera_ip: IpAddr) {
        self.stream_statuses.remove(&camera_ip);
        self.battery_uids.remove(&camera_ip);
        self.health.remove(camera_ip);
        self.health.events.remove(camera_ip);
        if let Some(live) = &self.live {
            live.reset_camera(camera_ip);
        }
    }

    fn finish_camera_stop(
        &mut self,
        camera_ip: IpAddr,
        deadline: std::time::Instant,
    ) -> anyhow::Result<()> {
        let mut stopped = true;
        if let Some(workers) = self.retiring_workers.remove(&camera_ip) {
            stopped =
                self.wait_for_camera_workers(deadline, Some(camera_ip), || workers.is_finished());
            if stopped {
                workers.join(camera_ip);
            } else {
                self.retiring_workers.insert(camera_ip, workers);
            }
        }
        self.clear_stopped_camera_state(camera_ip);
        anyhow::ensure!(stopped, "camera workers did not stop before the deadline");
        Ok(())
    }

    fn wait_for_camera_workers(
        &mut self,
        deadline: std::time::Instant,
        stopping: Option<IpAddr>,
        finished: impl Fn() -> bool,
    ) -> bool {
        while !finished() {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match self.rx.recv_timeout(remaining.min(EVENT_POLL_INTERVAL)) {
                Ok(event) => self.handle_event_while_stopping(stopping, event),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return finished(),
            }
        }
        self.drain_camera_queue(stopping);
        true
    }

    fn drain_camera_queue(&mut self, stopping: Option<IpAddr>) {
        // A finished producer can have at most one channel capacity of messages outstanding.
        for _ in 0..CHANNEL_BUFFER {
            let Ok(event) = self.rx.try_recv() else { break };
            self.handle_event_while_stopping(stopping, event);
        }
    }

    fn ensure_worker_slot(&self, camera: &Camera) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.retiring_workers.contains_key(&camera.config.ip),
            "previous camera workers are still stopping"
        );
        // Bound the number of stalled groups that can accumulate through remove/add cycles.
        anyhow::ensure!(
            self.retiring_workers.len() < crate::profiles::MAX_PROFILE_TARGETS
                || !self.effective_camera_policy(&camera.config).available,
            "too many camera workers are still stopping"
        );
        Ok(())
    }

    fn reap_stopped_cameras(&mut self) {
        if self.retiring_workers.is_empty() {
            return;
        }
        let finished: Vec<_> = self
            .retiring_workers
            .iter()
            .filter(|(_, workers)| workers.is_finished())
            .map(|(ip, _)| *ip)
            .collect();
        if finished.is_empty() {
            return;
        }
        self.drain_camera_queue(None);
        for ip in finished {
            self.retiring_workers
                .remove(&ip)
                .expect("finished worker retained until queue drained")
                .join(ip);
            self.clear_stopped_camera_state(ip);
        }
    }

    fn retry_ready_camera_start(&mut self) {
        if self.deferred_camera_starts.is_empty()
            || self
                .runtime_profile_status
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .revision
                > self.runtime_profiles.revision
        {
            return;
        }
        // Execute one ready change per loop. Retiring workers still own their source.
        let ready = self.deferred_camera_starts.iter().find_map(|(ip, flags)| {
            let camera = self.camera_definitions.get(ip)?;
            let workers_ready = self.camera_workers.get(ip).is_none_or(|workers| {
                workers.event_shutdown.is_cancelled()
                    && workers.event_handles.iter().all(JoinHandle::is_finished)
            });
            (workers_ready && self.ensure_worker_slot(camera).is_ok())
                .then(|| (camera.clone(), *flags))
        });
        let Some((camera, (main, sub))) = ready else {
            return;
        };
        self.deferred_camera_starts.remove(&camera.config.ip);
        if !self.effective_camera_policy(&camera.config).available {
            return;
        }
        self.deferred_privacy_confirmation = self
            .held_privacy_exceeds_profile(camera.config.ip)
            .then_some(camera.config.ip);
        let result = if self.camera_workers.contains_key(&camera.config.ip) {
            self.restart_camera(&camera)
        } else {
            self.add_camera(&camera, main, sub)
        };
        self.deferred_privacy_confirmation = None;
        if let Err(error) = result {
            tracing::warn!(ip = %camera.config.ip, %error, "deferred camera change could not be applied");
        }
    }

    pub fn run(mut self) {
        tracing::info!("KeepPeek loop started");

        while !self.shutdown.is_cancelled() {
            self.reap_stopped_cameras();
            self.handle_commands();
            self.retry_ready_camera_start();
            self.refresh_recording_runtime_status();
            self.close_retired_native_events();
            match self.rx.recv_timeout(EVENT_POLL_INTERVAL) {
                Ok(event) => self.handle_event_while_stopping(None, event),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        tracing::info!("KeepPeek loop shutting down");

        if let Some(callbacks) = &self.isapi_callbacks {
            callbacks.cancel();
        }

        let mut camera_workers = std::mem::take(&mut self.camera_workers);
        camera_workers.extend(self.retiring_workers.drain());
        for workers in camera_workers.values() {
            workers.cancel();
        }
        while camera_workers
            .values()
            .any(|workers| !workers.is_finished())
            || self
                .isapi_callbacks
                .as_ref()
                .is_some_and(|callbacks| !callbacks.is_finished())
        {
            match self.rx.recv_timeout(EVENT_POLL_INTERVAL) {
                Ok(event) => self.handle_event(event),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        while let Ok(event) = self.rx.try_recv() {
            self.handle_event(event);
        }
        for (camera_ip, workers) in camera_workers {
            workers.join(camera_ip);
        }
        self.drain_retired_native_events();
        if let Some(callbacks) = self.isapi_callbacks.take() {
            callbacks.join();
        }
        drop(self.tx);

        tracing::info!("KeepPeek loop stopped");
    }

    fn handle_event(&mut self, event: KeepPeekEvent) {
        match event {
            KeepPeekEvent::NativeBatch {
                owner,
                lifetime,
                changes,
                reply,
            } => self.commit_native_batch(owner, lifetime, changes, reply),
            KeepPeekEvent::IsapiBatch {
                changes,
                reply,
                fence,
            } => {
                let mut committed = 0;
                for change in changes {
                    if fence.as_ref().is_some_and(|fence| !fence.is_current()) {
                        break;
                    }
                    if let Err(error) = self.commit_isapi_change(change) {
                        tracing::warn!(%error, committed, "ISAPI callback commit paused");
                        break;
                    }
                    committed += 1;
                }
                let _ = reply.send(committed);
            }
            KeepPeekEvent::StreamConnected { camera_ip, stream } => {
                tracing::info!(%camera_ip, %stream, "stream connected");
                if let Some(uid) = self.battery_uids.get(&camera_ip)
                    && let Some(battery_wake) = &self.battery_wake
                {
                    battery_wake.note_media_connected(uid);
                }
                let status = self
                    .stream_statuses
                    .get_mut(&camera_ip)
                    .map(|status| status.connected(stream));
                if let Some(status) = status {
                    self.publish_status(status);
                }
            }
            KeepPeekEvent::StreamError {
                camera_ip,
                stream,
                error,
            } => {
                tracing::warn!(%camera_ip, %stream, %error, "stream error");
                if let Some(uid) = self.battery_uids.get(&camera_ip)
                    && let Some(battery_wake) = &self.battery_wake
                {
                    battery_wake.note_media_disconnected(uid);
                }
                let status = self
                    .stream_statuses
                    .get_mut(&camera_ip)
                    .map(|status| status.error(stream, error));
                if let Some(status) = status {
                    self.publish_status(status);
                }
            }
            KeepPeekEvent::TimelineEventStarted { event } => {
                let event = *event;
                if let Some(storage) = &self.storage {
                    storage.note_camera_event(&event.camera_id);
                }
                if let Some(events) = self.events.clone() {
                    let event_id = event.id.clone();
                    let camera_id = event.camera_id.clone();
                    let kind = event.kind.clone();
                    if let Err(error) = events.insert(event.clone()) {
                        tracing::warn!(%event_id, %camera_id, %kind, %error, "unable to store camera event");
                    } else {
                        self.publish_event_revision(
                            &event,
                            Trigger::EventCreated,
                            NotificationStage::Preliminary,
                            None,
                            event.start_time_ms,
                        );
                    }
                }
            }
            KeepPeekEvent::TimelineEventImages { event, images } => {
                if self.privacy_active(&event.camera_id) {
                    return;
                }
                if let Some(events) = self.events.clone() {
                    match events.commit_native_images(*event, &images) {
                        Ok(event) => {
                            let attachment_path = events
                                .thumbnail_path(&event.camera_id, &event.id)
                                .ok()
                                .flatten();
                            let trigger = if event.revision == 1 {
                                Trigger::EventCreated
                            } else {
                                Trigger::EventUpdated
                            };
                            self.publish_event_revision(
                                &event,
                                trigger,
                                NotificationStage::Enriched,
                                attachment_path.as_deref(),
                                unix_time_ms(),
                            );
                        }
                        Err(error) => {
                            tracing::warn!(%error, "unable to commit native event images");
                        }
                    }
                }
            }
            KeepPeekEvent::TimelineEventEnded { id, end_time_ms } => {
                if let Some(events) = self.events.clone() {
                    if let Err(error) = events.close(&id, end_time_ms) {
                        tracing::warn!(event_id = %id, %error, "unable to close camera event");
                    } else {
                        match events.event_by_id(&id) {
                            Ok(Some(event)) => {
                                let attachment_path =
                                    events.thumbnail_path(&event.camera_id, &id).ok().flatten();
                                self.publish_event_revision(
                                    &event,
                                    Trigger::EventEnded,
                                    NotificationStage::Enriched,
                                    attachment_path.as_deref(),
                                    end_time_ms,
                                );
                            }
                            Ok(None) => {
                                tracing::warn!(event_id = %id, "closed camera event disappeared");
                            }
                            Err(error) => {
                                tracing::warn!(event_id = %id, %error, "unable to load closed camera event");
                            }
                        }
                    }
                }
            }
            KeepPeekEvent::TimelineEventThumbnail {
                camera_id,
                event_id,
                jpeg,
            } => {
                if self.privacy_active(&camera_id) {
                    return;
                }
                if let Some(events) = self.events.clone() {
                    if let Err(error) = events.save_thumbnail(&camera_id, &event_id, &jpeg) {
                        tracing::warn!(%camera_id, %event_id, %error, "unable to store event thumbnail");
                    } else {
                        match events.event_by_id(&event_id) {
                            Ok(Some(event)) => {
                                let attachment_path =
                                    events.thumbnail_path(&camera_id, &event_id).ok().flatten();
                                self.publish_event_revision(
                                    &event,
                                    Trigger::EventUpdated,
                                    NotificationStage::Enriched,
                                    attachment_path.as_deref(),
                                    unix_time_ms(),
                                );
                            }
                            Ok(None) => {
                                tracing::warn!(%event_id, "camera event with thumbnail disappeared");
                            }
                            Err(error) => {
                                tracing::warn!(%event_id, %error, "unable to load enriched camera event");
                            }
                        }
                    }
                }
            }
        }
    }

    fn publish_event_revision(
        &self,
        event: &TimelineEvent,
        trigger: Trigger,
        stage: NotificationStage,
        attachment_path: Option<&std::path::Path>,
        occurred_at_ms: i64,
    ) {
        if self.privacy_active(&event.camera_id) {
            return;
        }
        if let Some(publish) = &self.event_publisher {
            publish(event);
        }
        if let Some(event_forwarder) = &self.event_forwarder {
            let transition = match trigger {
                Trigger::EventCreated => EventTransition::Created,
                Trigger::EventUpdated => EventTransition::Updated,
                Trigger::EventEnded => EventTransition::Ended,
                _ => EventTransition::Updated,
            };
            if let Err(error) = event_forwarder.publish_timeline(event, transition, occurred_at_ms)
            {
                tracing::warn!(
                    event_id = %event.id,
                    revision = event.revision,
                    %error,
                    "unable to enqueue event revision for MQTT forwarding"
                );
            }
        }
        let Some(notifications) = &self.notifications else {
            return;
        };
        let duration_ms = event.end_time_ms.and_then(|end_time_ms| {
            u64::try_from(end_time_ms.saturating_sub(event.start_time_ms)).ok()
        });
        notifications.publish(NotificationCandidate {
            trigger,
            source_id: event.camera_id.clone(),
            source_name: self.camera_names.get(&event.camera_id).cloned(),
            source_identity: event.id.clone(),
            lifecycle: NotificationLifecycle::Event,
            event_kind: Some(event.kind.clone()),
            payload: None,
            group_ids: Vec::new(),
            zone: event.zone.clone(),
            confidence: event.confidence,
            attachment_path: attachment_path.map(|path| path.to_string_lossy().into_owned()),
            canonical_attachment: event.canonical_attachment().cloned(),
            icon_key: Some(event.icon_key.clone()),
            image_available: event.canonical_image_available(),
            duration_ms,
            severity: Severity::Info,
            reviewed: None,
            bookmarked: None,
            privacy_active: false,
            revision: event.revision,
            stage,
            occurred_at_ms,
            deep_link: event_deep_link(&event.camera_id, &event.id),
        });
    }

    fn privacy_active(&self, camera_id: &str) -> bool {
        self.privacy.as_ref().is_some_and(|privacy| {
            privacy
                .decision(camera_id, chrono::Utc::now())
                .map_or(true, |decision| decision.0)
        })
    }

    fn handle_event_while_stopping(&mut self, camera_ip: Option<IpAddr>, event: KeepPeekEvent) {
        match event {
            KeepPeekEvent::StreamConnected {
                camera_ip: event_ip,
                ..
            }
            | KeepPeekEvent::StreamError {
                camera_ip: event_ip,
                ..
            } if Some(event_ip) == camera_ip || self.retiring_workers.contains_key(&event_ip) => {}
            event => self.handle_event(event),
        }
    }

    fn handle_commands(&mut self) {
        loop {
            let command = match self.command_rx.try_recv() {
                Ok(command) => command,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            match command {
                KeepPeekCommand::SuspendForPrivacy { ip, reply } => {
                    if self.camera_definitions.contains_key(&ip) {
                        self.privacy_suspended.insert(ip);
                    }
                    self.deferred_camera_starts.remove(&ip);
                    let outcome = self.stop_camera(ip);
                    if let Some(camera) = self.camera_definitions.get(&ip) {
                        let state = outcome
                            .as_ref()
                            .map_err(ToString::to_string)
                            .and_then(|()| {
                                let status = self
                                    .runtime_profile_status
                                    .read()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                let id = self.runtime_profile_camera_ids.camera_id(ip);
                                let (configured_mode, _) =
                                    crate::profiles::configured_recording_mode(
                                        camera.config.recording_mode,
                                        self.camera_override(ip),
                                    );
                                let stopped = InactiveCamera::after_privacy_stop(
                                    &camera.config,
                                    status.cameras.get(&id),
                                    configured_mode,
                                );
                                stopped.application_state(
                                    &self.effective_camera_policy(&camera.config),
                                    configured_mode,
                                )
                            });
                        self.record_camera_runtime_state(camera, state);
                    } else if outcome.is_ok() && self.reconcile_missing_profile_targets() {
                        self.publish_runtime_profile_status();
                    }
                    let _ = reply.send(outcome);
                }
                KeepPeekCommand::Stop { ip, reply } => {
                    self.privacy_suspended.remove(&ip);
                    self.camera_definitions.remove(&ip);
                    self.inactive_configurations.remove(&ip);
                    self.deferred_camera_starts.remove(&ip);
                    let outcome = self.stop_camera(ip);
                    if self.reconcile_missing_profile_targets() {
                        self.publish_runtime_profile_status();
                    }
                    let _ = reply.send(outcome);
                }
                KeepPeekCommand::StartCamera { camera, reply } => {
                    let ip = camera.config.ip;
                    // Base can prestart the saved camera behind its held privacy gate.
                    let already_resumed = self.privacy_suspended.contains(&ip)
                        && self
                            .camera_workers
                            .get(&ip)
                            .and_then(|w| w.configuration.as_ref())
                            .is_some_and(|current| current == &camera.config);
                    let result = if already_resumed {
                        Ok(())
                    } else {
                        self.add_camera(&camera, true, true)
                    };
                    if result.is_ok() && self.camera_workers.contains_key(&ip) {
                        self.privacy_suspended.remove(&ip);
                    }
                    let _ = reply.send(result);
                }
                KeepPeekCommand::RestartCamera { camera, reply } => {
                    let result = self.restart_camera(&camera);
                    let _ = reply.send(result);
                }
                KeepPeekCommand::SetRuntimeProfiles {
                    profiles,
                    camera_ids,
                    reply,
                } => {
                    let result = self.apply_runtime_profiles_with_camera_ids(profiles, camera_ids);
                    let _ = reply.send(result);
                }
            }
        }
    }
}

fn event_deep_link(camera_id: &str, event_id: &str) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("camera", camera_id);
    query.append_pair("event", event_id);
    format!("/events?{}", query.finish())
}

fn active_notification_suppressions(
    profiles: &crate::profiles::ProfileRegistry,
    identities: &CameraProfileIds,
) -> std::collections::BTreeSet<String> {
    profiles
        .active_profile_id
        .as_ref()
        .and_then(|profile_id| profiles.profiles.get(profile_id))
        .map(|profile| {
            profile
                .cameras
                .iter()
                .filter_map(|(camera_id, camera)| {
                    if camera.notifications_suppressed == Some(true) {
                        identities.source(camera_id).map(|ip| ip.to_string())
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn effective_recording_mode(
    profiles: &crate::profiles::ProfileRegistry,
    camera: &crate::cameras::CameraConfig,
    camera_id: &str,
) -> crate::cameras::CameraRecordingMode {
    let overlay = profiles.active_camera_override(camera_id);
    crate::profiles::resolve_camera_policy(
        &crate::profiles::BaseCameraPolicy {
            available: true,
            recording_mode: camera.recording_mode,
            privacy_enforced: false,
            notifications_enabled: true,
        },
        overlay,
    )
    .recording_mode
}

fn unix_time_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    mod isapi;
    mod native;
    mod profile_identity;
    mod profile_workers;
    mod recording_ack;
    mod recording_admission;
    mod recording_health;

    use super::*;
    use crate::cameras::{CameraCapabilities, CameraConfig, CameraPorts, DeviceInfo, MediaProfile};
    use std::{
        net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
        time::Instant,
    };

    fn runtime_rtsp_camera(address: SocketAddr) -> Camera {
        Camera {
            config: CameraConfig {
                events: Default::default(),
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                name: Some("runtime-rtsp".to_owned()),
                display_name: None,
                manufacturer: None,
                username: "operator".to_owned(),
                password: "secret".to_owned(),
                onvif_port: None,
                http_port: None,
                main_rtsp_url: Some(format!("rtsp://{address}/main")),
                sub_rtsp_url: None,
                uid: None,
                backend: CameraBackend::Retina,
                transport: CameraTransport::Tcp,
                record_generic_motion_events: false,
                recording_mode: Default::default(),
                event_recording_duration_secs: 60,
                event_pre_recording_duration_secs: 0,
                event_recording_stream: Default::default(),
                recorded_audio: Default::default(),
                live_audio: Default::default(),
            },
            device: DeviceInfo::default(),
            reported_manufacturer: None,
            hostname: None,
            mac_address: None,
            ports: CameraPorts {
                rtsp: Some(address.port()),
                ..CameraPorts::default()
            },
            capabilities: CameraCapabilities::default(),
            event_service: None,
            profiles: vec![MediaProfile {
                token: "main".to_owned(),
                name: "Main".to_owned(),
                stream_uri: None,
                snapshot_uri: None,
                video: None,
                audio: None,
            }],
            is_reolink: false,
            ptz: None,
            imaging: None,
        }
    }

    fn accept_rtsp_connection(listener: &TcpListener) -> TcpStream {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match listener.accept() {
                Ok((stream, _)) => return stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "RTSP worker did not connect");
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("RTSP listener failed: {error}"),
            }
        }
    }

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
    fn active_profile_recording_mode_respects_base_disabled_and_profile_privacy() {
        let mut camera = runtime_rtsp_camera(
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .unwrap()
                .local_addr()
                .unwrap(),
        )
        .config;
        camera.recording_mode = crate::cameras::CameraRecordingMode::Both;
        let mut profiles = crate::profiles::ProfileRegistry {
            revision: 1,
            active_profile_id: Some("away".to_owned()),
            profiles: std::collections::BTreeMap::from([(
                "away".to_owned(),
                crate::profiles::RuntimeProfile {
                    id: "away".to_owned(),
                    name: "Away".to_owned(),
                    revision: 1,
                    cameras: std::collections::BTreeMap::from([(
                        camera.ip.to_string(),
                        crate::profiles::CameraProfileOverride {
                            recording_mode: Some(crate::cameras::CameraRecordingMode::Main),
                            ..Default::default()
                        },
                    )]),
                },
            )]),
        };
        assert_eq!(
            effective_recording_mode(&profiles, &camera, &camera.ip.to_string()),
            crate::cameras::CameraRecordingMode::Main
        );

        camera.recording_mode = crate::cameras::CameraRecordingMode::Off;
        assert_eq!(
            effective_recording_mode(&profiles, &camera, &camera.ip.to_string()),
            crate::cameras::CameraRecordingMode::Off
        );

        camera.recording_mode = crate::cameras::CameraRecordingMode::Both;
        profiles
            .profiles
            .get_mut("away")
            .unwrap()
            .cameras
            .get_mut(&camera.ip.to_string())
            .unwrap()
            .privacy_enforced = Some(true);
        assert_eq!(
            effective_recording_mode(&profiles, &camera, &camera.ip.to_string()),
            crate::cameras::CameraRecordingMode::Off
        );
    }

    #[test]
    fn runtime_profile_base_restores_the_latest_event_only_camera_edit() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut camera = runtime_rtsp_camera(listener.local_addr().unwrap());
        camera.config.events.mode = crate::cameras::events::EventMode::Disabled;
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        recorder.add_camera(&camera, true, true).unwrap();
        let _initial_connection = accept_rtsp_connection(&listener);
        edit_generic_motion_without_media_restart(&mut recorder, &mut camera);
        let away = serde_json::from_value(serde_json::json!({
            "revision": 1, "active_profile_id": "away", "profiles": {"away": {
                "id": "away", "name": "Away", "revision": 1,
                "cameras": {camera.config.ip.to_string(): {"availability": false}}
            }}
        }))
        .unwrap();
        recorder.apply_runtime_profiles(away).unwrap();
        assert!(!recorder.camera_workers.contains_key(&camera.config.ip));
        recorder
            .apply_runtime_profiles(crate::profiles::ProfileRegistry {
                revision: 2,
                ..Default::default()
            })
            .unwrap();
        let _restored_connection = accept_rtsp_connection(&listener);
        let restored = recorder.camera_workers[&camera.config.ip]
            .configuration
            .clone()
            .unwrap();
        let retained_motion = recorder
            .health
            .events
            .record_motion(camera.config.ip, false);
        recorder.stop_camera(camera.config.ip).unwrap();
        assert!(camera.config.record_generic_motion_events);
        assert!(
            restored.record_generic_motion_events,
            "Base must restore the accepted event edit after profile deactivation"
        );
        assert_eq!(restored.events, camera.config.events);
        assert!(
            retained_motion,
            "restored event policy must retain generic motion"
        );
        assert!(
            recorder.camera_definitions[&camera.config.ip]
                .config
                .record_generic_motion_events
        );
        assert_eq!(
            recorder.camera_definitions[&camera.config.ip].config.events,
            camera.config.events
        );
    }

    fn edit_generic_motion_without_media_restart(recorder: &mut KeepPeekLoop, camera: &mut Camera) {
        let media_worker = recorder.camera_workers[&camera.config.ip].handles[0]
            .thread()
            .id();
        let mut durations_us = Vec::with_capacity(31);
        for _ in 0..31 {
            camera.config.record_generic_motion_events =
                !camera.config.record_generic_motion_events;
            camera.config.events.snapshots = !camera.config.events.snapshots;
            let started = Instant::now();
            recorder.restart_camera(camera).unwrap();
            durations_us.push(started.elapsed().as_micros());
            assert_eq!(
                recorder.camera_workers[&camera.config.ip].handles[0]
                    .thread()
                    .id(),
                media_worker
            );
        }
        durations_us.sort_unstable();
        eprintln!(
            "31 event-only edits: median={}us p95={}us max={}us",
            durations_us[15], durations_us[29], durations_us[30]
        );
    }

    #[test]
    fn runtime_profile_availability_stops_and_restores_camera_workers() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let camera = runtime_rtsp_camera(listener.local_addr().unwrap());
        let camera_id = camera.config.ip.to_string();
        let mut away = crate::profiles::ProfileRegistry {
            revision: 1,
            active_profile_id: Some("away".to_owned()),
            profiles: std::collections::BTreeMap::from([(
                "away".to_owned(),
                crate::profiles::RuntimeProfile {
                    id: "away".to_owned(),
                    name: "Away".to_owned(),
                    revision: 1,
                    cameras: std::collections::BTreeMap::from([(
                        camera_id.clone(),
                        crate::profiles::CameraProfileOverride {
                            availability: Some(false),
                            ..Default::default()
                        },
                    )]),
                },
            )]),
        };
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        recorder.set_runtime_profiles(away.clone());
        recorder.add_camera(&camera, true, true).unwrap();
        assert!(!recorder.camera_workers.contains_key(&camera.config.ip));
        assert!(recorder.camera_definitions.contains_key(&camera.config.ip));
        assert!(
            !recorder.runtime_profile_status.read().unwrap().cameras[&camera_id]
                .policy
                .available
        );

        let base = crate::profiles::ProfileRegistry {
            revision: 2,
            ..Default::default()
        };
        let applied = recorder.apply_runtime_profiles(base).unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(
            applied[0].state,
            crate::profiles::RuntimeApplicationState::Applied
        );
        assert_eq!(recorder.runtime_profile_status.read().unwrap().revision, 2);
        let _connection = accept_rtsp_connection(&listener);
        assert!(recorder.camera_workers.contains_key(&camera.config.ip));

        away.revision = 3;
        let applied = recorder.apply_runtime_profiles(away).unwrap();
        assert_eq!(
            applied[0].state,
            crate::profiles::RuntimeApplicationState::Applied
        );
        assert!(!recorder.camera_workers.contains_key(&camera.config.ip));
        assert!(recorder.camera_definitions.contains_key(&camera.config.ip));
        let status = recorder.runtime_profile_status.read().unwrap().clone();
        let stale_base = crate::profiles::ProfileRegistry {
            revision: 2,
            ..Default::default()
        };
        assert!(recorder.apply_runtime_profiles(stale_base).is_err());
        assert!(!recorder.camera_workers.contains_key(&camera.config.ip));
        assert_eq!(*recorder.runtime_profile_status.read().unwrap(), status);
        assert_eq!(recorder.runtime_profiles.revision, 3);
    }

    fn active_privacy_schedule(id: &str) -> std::sync::Arc<crate::privacy::PrivacyRegistry> {
        let windows = [("00:00", "12:00"), ("12:00", "00:00")].map(|(start, end)| {
            crate::privacy::PrivacyWindow {
                weekdays: (1..=7).collect(),
                start: start.into(),
                end: end.into(),
            }
        });
        let schedule = crate::privacy::PrivacySchedule {
            enabled: true,
            timezone: "UTC".into(),
            windows: windows.into(),
            temporary_override: None,
            keep_camera_connected: false,
        };
        std::sync::Arc::new(
            crate::privacy::PrivacyRegistry::new([(id.to_owned(), schedule)].into()).unwrap(),
        )
    }

    #[test]
    fn acknowledged_camera_privacy_pause_reports_applied_until_resumption_is_required() {
        use crate::profiles::RuntimeApplicationState::{Applied, Degraded, Pending};
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let camera = runtime_rtsp_camera(listener.local_addr().unwrap());
        let mut other = camera.config.clone();
        other.ip = "127.0.0.2".parse().unwrap();
        other.username.clear();
        let ip = camera.config.ip;
        let id = ip.to_string();
        recorder.seed_profile_observations([&camera.config, &other].into_iter());
        let status = recorder.control().runtime_profile_status_handle();
        assert_eq!(status.read().unwrap().cameras[&id].state, Pending);
        recorder.add_camera(&camera, true, false).unwrap();
        let acknowledged_mode = status.read().unwrap().cameras[&id].policy.recording_mode;
        assert_eq!(acknowledged_mode, camera.config.recording_mode);
        assert_ne!(acknowledged_mode, crate::cameras::CameraRecordingMode::Off);
        let privacy = active_privacy_schedule(&id);
        assert!(privacy.decision(&id, chrono::Utc::now()).unwrap().0);
        recorder.set_privacy_registry(privacy.clone());
        let (reply, paused) = mpsc::sync_channel(1);
        recorder
            .command_tx
            .send(KeepPeekCommand::SuspendForPrivacy { ip, reply })
            .unwrap();
        recorder.handle_commands();
        paused.try_recv().unwrap().unwrap();
        for revision in 0..=1 {
            if revision != 0 {
                recorder
                    .apply_runtime_profiles(crate::profiles::ProfileRegistry {
                        revision,
                        ..Default::default()
                    })
                    .unwrap();
            }
            let view = status.read().unwrap();
            assert_eq!(view.cameras[&id].state, Applied);
            assert!(view.cameras[&id].policy.privacy_enforced);
            assert_eq!(
                view.cameras[&id].policy.recording_mode,
                crate::cameras::CameraRecordingMode::Off
            );
            assert_eq!(view.cameras[&other.ip.to_string()].state, Degraded);
            assert!(!recorder.camera_workers.contains_key(&ip));
        }
        privacy.replace_schedules(Default::default()).unwrap();
        assert!(!recorder.remain_privacy_suspended(ip).unwrap());
        let (reply, resumed) = mpsc::sync_channel(1);
        recorder
            .command_tx
            .send(KeepPeekCommand::StartCamera { camera, reply })
            .unwrap();
        recorder.handle_commands();
        resumed.try_recv().unwrap().unwrap();
        let view = status.read().unwrap();
        assert_eq!(view.cameras[&id].state, Applied);
        assert!(!view.cameras[&id].policy.privacy_enforced);
        assert!(recorder.camera_workers.contains_key(&ip));
        drop(view);
        recorder.stop_camera(ip).unwrap();
    }

    #[test]
    fn paused_camera_requires_writer_ack_for_changed_recording_policy() {
        use crate::profiles::RuntimeApplicationState::{Applied, Degraded};
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let camera = runtime_rtsp_camera(listener.local_addr().unwrap());
        let ip = camera.config.ip;
        let id = ip.to_string();
        let (storage, respond) = StorageHandle::with_test_ack_queue(1);
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
        let mut pending = Vec::new();
        recorder
            .begin_add_camera(&camera, true, false, &mut pending)
            .unwrap();
        assert_eq!(respond(vec![Some(Ok(()))]), 1);
        recorder.finish_camera_recordings(pending);
        recorder.set_privacy_registry(active_privacy_schedule(&id));
        let (reply, paused) = mpsc::sync_channel(1);
        recorder
            .command_tx
            .send(KeepPeekCommand::SuspendForPrivacy { ip, reply })
            .unwrap();
        recorder.handle_commands();
        paused.try_recv().unwrap().unwrap();
        for (revision, response, expected) in [
            (1, Err("writer rejected policy"), Degraded),
            (2, Ok(()), Applied),
        ] {
            let profile = serde_json::from_value(serde_json::json!({
                "revision": revision, "active_profile_id": "away", "profiles": {"away": {
                    "id": "away", "name": "Away", "revision": revision,
                    "cameras": {id.clone(): {"recording_mode": "off"}}
                }}
            }))
            .unwrap();
            std::thread::scope(|scope| {
                let writer = scope.spawn(|| respond(vec![Some(response)]));
                let rows = recorder.apply_runtime_profiles(profile).unwrap();
                assert_eq!(writer.join().unwrap(), 1);
                let row = rows.iter().find(|row| row.camera_id == id).unwrap();
                assert!(row.policy.privacy_enforced);
                assert_eq!(
                    row.policy.recording_mode,
                    crate::cameras::CameraRecordingMode::Off
                );
                assert_eq!(row.state, expected);
            });
            assert!(!recorder.camera_workers.contains_key(&ip));
        }
    }

    #[test]
    fn acknowledged_camera_privacy_pause_waits_for_restart_when_the_final_profile_releases_it() {
        use crate::profiles::RuntimeApplicationState::{Applied, Pending};
        for switch_to_base in [true, false] {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let camera = runtime_rtsp_camera(listener.local_addr().unwrap());
            let ip = camera.config.ip;
            let id = ip.to_string();
            let (storage, respond) = StorageHandle::with_test_ack_queue(1);
            let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
            let privacy = std::sync::Arc::new(crate::privacy::PrivacyRegistry::default());
            privacy
                .replace_profile_overrides([(id.clone(), true)].into())
                .unwrap();
            recorder.set_privacy_registry(privacy.clone());
            let mut selected: crate::profiles::ProfileRegistry =
                serde_json::from_value(serde_json::json!({
                    "revision": 1, "active_profile_id": "away", "profiles": {"away": {
                        "id": "away", "name": "Away", "revision": 1,
                        "cameras": {id.clone(): {"privacy_enforced": true}}
                    }}
                }))
                .unwrap();
            recorder.set_runtime_profiles(selected.clone());
            let status = recorder.control().runtime_profile_status_handle();
            let mut pending = Vec::new();
            recorder
                .begin_add_camera(&camera, true, false, &mut pending)
                .unwrap();
            assert_eq!(respond(vec![Some(Ok(()))]), 1);
            recorder.finish_camera_recordings(pending);
            let (reply, stopped) = mpsc::sync_channel(1);
            recorder
                .command_tx
                .send(KeepPeekCommand::SuspendForPrivacy { ip, reply })
                .unwrap();
            recorder.handle_commands();
            stopped.try_recv().unwrap().unwrap();
            assert_eq!(status.read().unwrap().cameras[&id].state, Applied);
            selected.revision += 1;
            if switch_to_base {
                selected.active_profile_id = None;
            } else {
                let profile = selected.profiles.get_mut("away").unwrap();
                profile.revision += 1;
                profile.cameras.get_mut(&id).unwrap().privacy_enforced = Some(false);
            }
            assert!(!recorder.camera_workers.contains_key(&ip));
            std::thread::scope(|scope| {
                let writer = scope.spawn(|| {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    loop {
                        let view = status.read().unwrap();
                        if view.revision == 2 && view.cameras[&id].state == Pending {
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "recorder did not publish the pending transition"
                        );
                        drop(view);
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    assert!(privacy.decision(&id, chrono::Utc::now()).unwrap().0);
                    assert_eq!(respond(vec![Some(Ok(()))]), 1);
                });
                let rows = recorder.apply_runtime_profiles(selected).unwrap();
                writer.join().unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].state, Applied);
                assert!(!rows[0].policy.privacy_enforced);
            });
            assert!(recorder.camera_workers.contains_key(&ip));
            assert!(privacy.decision(&id, chrono::Utc::now()).unwrap().0);
            privacy
                .replace_profile_overrides(Default::default())
                .unwrap();
            assert!(!privacy.decision(&id, chrono::Utc::now()).unwrap().0);
            recorder.stop_camera(ip).unwrap();
        }
    }

    #[test]
    fn configured_camera_without_credentials_remains_degraded_across_startup_and_profile_changes() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let mut config = runtime_rtsp_camera("127.0.0.1:8554".parse().unwrap()).config;
        config.username.clear();
        let id = config.ip.to_string();
        recorder.seed_profile_observations(std::iter::once(&config));
        let status = recorder.control().runtime_profile_status_handle();
        let verify = || {
            let view = status.read().unwrap();
            let row = &view.cameras[&id];
            assert_eq!(
                row.state,
                crate::profiles::RuntimeApplicationState::Degraded
            );
            assert_eq!(
                row.reason.as_deref(),
                Some("camera credentials are not configured")
            );
        };
        verify();
        recorder.add_cameras(&HashMap::new()).unwrap();
        verify();
        let (reply, stopped) = mpsc::sync_channel(1);
        recorder
            .command_tx
            .send(KeepPeekCommand::SuspendForPrivacy {
                ip: config.ip,
                reply,
            })
            .unwrap();
        recorder.handle_commands();
        stopped.try_recv().unwrap().unwrap();
        verify();
        recorder
            .apply_runtime_profiles(crate::profiles::ProfileRegistry {
                revision: 1,
                ..Default::default()
            })
            .unwrap();
        verify();
        assert!(recorder.camera_workers.is_empty());

        let (reply, removed) = mpsc::sync_channel(1);
        recorder
            .command_tx
            .send(KeepPeekCommand::Stop {
                ip: config.ip,
                reply,
            })
            .unwrap();
        recorder.handle_commands();
        removed.try_recv().unwrap().unwrap();
        assert!(!status.read().unwrap().cameras.contains_key(&id));
    }

    #[test]
    fn startup_observations_include_configured_and_missing_targets_before_any_worker_starts() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let mut camera = runtime_rtsp_camera("127.0.0.1:8554".parse().unwrap());
        camera.config.ip = "127.0.0.2".parse().unwrap();
        recorder.set_runtime_profiles(missing_target_profiles());
        recorder.seed_profile_observations(std::iter::once(&camera.config));
        let status = recorder.runtime_profile_status.read().unwrap();
        assert_eq!(status.cameras.len(), 2);
        assert_eq!(
            status.cameras["127.0.0.2"].state,
            crate::profiles::RuntimeApplicationState::Pending
        );
        assert_eq!(
            status
                .cameras
                .values()
                .filter(|app| app.state == crate::profiles::RuntimeApplicationState::Degraded)
                .count(),
            1
        );
    }

    #[test]
    fn runtime_profile_missing_targets_survive_startup_and_completion() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let profiles = missing_target_profiles();
        recorder.set_runtime_profiles(profiles.clone());
        recorder.add_cameras(&HashMap::new()).unwrap();
        assert_missing_target_status(&recorder);
        let applications = recorder.apply_runtime_profiles(profiles).unwrap();
        assert_eq!(applications.len(), 1);
        assert_missing_target_status(&recorder);
        recorder
            .apply_runtime_profiles(crate::profiles::ProfileRegistry {
                revision: 2,
                ..Default::default()
            })
            .unwrap();
        assert!(
            recorder
                .runtime_profile_status
                .read()
                .unwrap()
                .cameras
                .is_empty()
        );
    }

    fn missing_target_profiles() -> crate::profiles::ProfileRegistry {
        serde_json::from_value(serde_json::json!({
            "revision": 1, "active_profile_id": "away", "profiles": {"away": {
                "id": "away", "name": "Away", "revision": 1,
                "cameras": {"127.0.0.1": {"availability": false, "privacy_enforced": true}}
            }}
        }))
        .unwrap()
    }

    #[test]
    fn runtime_profile_missing_target_reconciliation_preserves_newer_pending_status() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        recorder.set_runtime_profiles(missing_target_profiles());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let camera = runtime_rtsp_camera(listener.local_addr().unwrap());
        recorder.add_camera(&camera, true, true).unwrap();
        let expected = {
            let mut status = recorder.runtime_profile_status.write().unwrap();
            status.revision = 2;
            status.active_profile_id = None;
            status.cameras.get_mut("127.0.0.1").unwrap().state =
                crate::profiles::RuntimeApplicationState::Pending;
            status.clone()
        };
        let publications = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let published = publications.clone();
        recorder.set_runtime_profile_status_publisher(move || {
            published.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let (reply, result) = mpsc::sync_channel(1);
        recorder
            .command_tx
            .send(KeepPeekCommand::Stop {
                ip: camera.config.ip,
                reply,
            })
            .unwrap();
        recorder.handle_commands();
        result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(*recorder.runtime_profile_status.read().unwrap(), expected);
        assert_eq!(publications.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    fn assert_missing_target_status(recorder: &KeepPeekLoop) {
        let status = recorder.runtime_profile_status.read().unwrap();
        let application = status
            .cameras
            .get("127.0.0.1")
            .expect("active missing target must remain visible");
        assert_eq!(
            application.state,
            crate::profiles::RuntimeApplicationState::Degraded
        );
        assert_eq!(
            application.reason.as_deref(),
            Some("configured camera is missing")
        );
        assert!(!application.policy.available);
        assert!(application.policy.privacy_enforced);
    }

    #[test]
    fn runtime_profile_missing_targets_follow_removal_and_readdition() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        recorder.set_runtime_profiles(missing_target_profiles());
        let camera = runtime_rtsp_camera("127.0.0.1:8554".parse().unwrap());
        recorder.add_camera(&camera, true, true).unwrap();
        assert_eq!(
            recorder.runtime_profile_status.read().unwrap().cameras["127.0.0.1"].state,
            crate::profiles::RuntimeApplicationState::Applied
        );
        let (reply, result) = mpsc::sync_channel(1);
        recorder
            .command_tx
            .send(KeepPeekCommand::Stop {
                ip: camera.config.ip,
                reply,
            })
            .unwrap();
        recorder.handle_commands();
        result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_missing_target_status(&recorder);
        recorder.add_camera(&camera, true, true).unwrap();
        let status = recorder.runtime_profile_status.read().unwrap();
        assert_eq!(
            status.cameras["127.0.0.1"].state,
            crate::profiles::RuntimeApplicationState::Applied
        );
        assert!(status.cameras["127.0.0.1"].reason.is_none());
    }

    #[test]
    fn runtime_profile_status_publisher_reports_camera_startup() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let camera = runtime_rtsp_camera("127.0.0.1:8554".parse().unwrap());
        let profiles = serde_json::from_value(serde_json::json!({
            "revision": 1, "active_profile_id": "away", "profiles": {"away": {
                "id": "away", "name": "Away", "revision": 1,
                "cameras": {camera.config.ip.to_string(): {"availability": false}}
            }}
        }))
        .unwrap();
        recorder.set_runtime_profiles(profiles);
        let observations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = observations.clone();
        let status = recorder.runtime_profile_status.clone();
        recorder.set_runtime_profile_status_publisher(move || {
            observed
                .lock()
                .unwrap()
                .push(status.read().unwrap().clone());
        });

        recorder.add_camera(&camera, true, true).unwrap();

        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].revision, 1);
        assert_eq!(
            observations[0].cameras[&camera.config.ip.to_string()].state,
            crate::profiles::RuntimeApplicationState::Applied
        );
        assert!(
            !observations[0].cameras[&camera.config.ip.to_string()]
                .policy
                .available
        );
    }

    #[test]
    fn runtime_profile_status_publisher_coalesces_camera_batches_and_survives_rejection() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let template = runtime_rtsp_camera("127.0.0.1:8554".parse().unwrap());
        let mut cameras = HashMap::new();
        let mut targets = std::collections::BTreeMap::new();
        for index in 1..=crate::profiles::MAX_PROFILE_TARGETS {
            let mut camera = template.clone();
            camera.config.ip = Ipv4Addr::new(192, 0, 2, u8::try_from(index).unwrap()).into();
            targets.insert(
                camera.config.ip.to_string(),
                crate::profiles::CameraProfileOverride {
                    availability: Some(false),
                    ..Default::default()
                },
            );
            cameras.insert(camera.config.ip, camera);
        }
        let mut profiles = crate::profiles::ProfileRegistry {
            revision: 1,
            active_profile_id: Some("away".to_owned()),
            profiles: std::collections::BTreeMap::from([(
                "away".to_owned(),
                crate::profiles::RuntimeProfile {
                    id: "away".to_owned(),
                    name: "Away".to_owned(),
                    revision: 1,
                    cameras: targets,
                },
            )]),
        };
        recorder.set_runtime_profiles(profiles.clone());
        let publications = std::sync::Arc::new(AtomicUsize::new(0));
        let published = publications.clone();
        recorder.set_runtime_profile_status_publisher(move || {
            published.fetch_add(1, Ordering::SeqCst);
        });
        recorder.add_cameras(&cameras).unwrap();
        assert_eq!(publications.load(Ordering::SeqCst), 1);
        assert_eq!(
            recorder
                .runtime_profile_status
                .read()
                .unwrap()
                .cameras
                .len(),
            127
        );
        profiles.revision = 2;
        recorder.apply_runtime_profiles(profiles.clone()).unwrap();
        assert_eq!(publications.load(Ordering::SeqCst), 2);
        profiles.revision = 1;
        assert!(recorder.apply_runtime_profiles(profiles.clone()).is_err());
        assert_eq!(publications.load(Ordering::SeqCst), 2);
        profiles.revision = 3;
        recorder.apply_runtime_profiles(profiles).unwrap();
        assert_eq!(publications.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn runtime_profile_disabled_camera_scale() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let template = runtime_rtsp_camera("127.0.0.1:8554".parse().unwrap());
        let mut targets = std::collections::BTreeMap::new();
        for index in 1..=crate::profiles::MAX_PROFILE_TARGETS {
            let mut camera = template.clone();
            camera.config.ip = Ipv4Addr::new(192, 0, 2, u8::try_from(index).unwrap()).into();
            targets.insert(
                camera.config.ip.to_string(),
                crate::profiles::CameraProfileOverride {
                    availability: Some(false),
                    ..Default::default()
                },
            );
            recorder.camera_definitions.insert(camera.config.ip, camera);
        }
        let mut profiles = crate::profiles::ProfileRegistry {
            revision: 1,
            active_profile_id: Some("away".to_owned()),
            profiles: std::collections::BTreeMap::from([(
                "away".to_owned(),
                crate::profiles::RuntimeProfile {
                    id: "away".to_owned(),
                    name: "Away".to_owned(),
                    revision: 1,
                    cameras: targets,
                },
            )]),
        };
        recorder.apply_runtime_profiles(profiles.clone()).unwrap();
        let mut samples = Vec::with_capacity(30);
        for revision in 2..=31 {
            profiles.revision = revision;
            let input = profiles.clone();
            let started = Instant::now();
            let applications = recorder.apply_runtime_profiles(input).unwrap();
            samples.push(started.elapsed());
            assert_eq!(applications.len(), crate::profiles::MAX_PROFILE_TARGETS);
            assert!(
                applications
                    .iter()
                    .all(|application| !application.policy.available
                        && application.state == crate::profiles::RuntimeApplicationState::Applied)
            );
            assert_eq!(
                recorder.runtime_profile_status.read().unwrap().revision,
                revision
            );
        }
        samples.sort_unstable();
        eprintln!(
            "127 disabled cameras, 30 transitions: median={}us p95={}us",
            samples[15].as_micros(),
            samples[28].as_micros()
        );
        assert!(recorder.camera_workers.is_empty());
    }

    #[test]
    fn runtime_profile_supersession_preserves_newer_pending_status() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let accepted = crate::profiles::ProfileRegistry {
            revision: 2,
            ..Default::default()
        };
        recorder.set_runtime_profiles(accepted.clone());
        recorder.runtime_profile_status.write().unwrap().revision = 4;
        let pending = recorder.runtime_profile_status.read().unwrap().clone();
        let camera = runtime_rtsp_camera("127.0.0.1:8554".parse().unwrap());
        recorder.record_camera_runtime_status(&camera, Ok(()));
        assert_eq!(*recorder.runtime_profile_status.read().unwrap(), pending);
        assert!(recorder.add_camera(&camera, true, true).is_err());
        assert!(recorder.camera_workers.is_empty());
        recorder.camera_definitions.clear();
        let queued = crate::profiles::ProfileRegistry {
            revision: 3,
            ..Default::default()
        };
        assert!(recorder.apply_runtime_profiles(queued).is_err());
        assert_eq!(recorder.runtime_profiles, accepted);
        assert_eq!(*recorder.runtime_profile_status.read().unwrap(), pending);

        let newest = crate::profiles::ProfileRegistry {
            revision: 4,
            ..Default::default()
        };
        assert!(recorder.apply_runtime_profiles(newest.clone()).is_ok());
        assert!(recorder.apply_runtime_profiles(newest.clone()).is_ok());
        assert_eq!(recorder.runtime_profiles, newest);
        assert_eq!(recorder.runtime_profile_status.read().unwrap().revision, 4);
    }

    #[test]
    fn runtime_profile_supersession_rejects_changed_same_revision() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let accepted = crate::profiles::ProfileRegistry {
            revision: 2,
            ..Default::default()
        };
        recorder.set_runtime_profiles(accepted.clone());
        let mut changed = accepted.clone();
        changed.profiles.insert(
            "away".to_owned(),
            crate::profiles::RuntimeProfile {
                id: "away".to_owned(),
                name: "Away".to_owned(),
                revision: 1,
                cameras: Default::default(),
            },
        );
        let status = recorder.runtime_profile_status.read().unwrap().clone();
        assert!(recorder.apply_runtime_profiles(changed).is_err());
        assert_eq!(recorder.runtime_profiles, accepted);
        assert_eq!(*recorder.runtime_profile_status.read().unwrap(), status);
    }

    #[test]
    fn profile_reports_all_blocked_stops_within_one_shared_deadline() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let mut targets = BTreeMap::new();
        let mut releases = Vec::new();
        let mut cancellations = Vec::new();
        for octet in 1..=4 {
            let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, octet));
            let mut camera = runtime_rtsp_camera((Ipv4Addr::LOCALHOST, 0).into());
            camera.config.ip = ip;
            recorder.camera_definitions.insert(ip, camera);
            targets.insert(
                ip.to_string(),
                crate::profiles::CameraProfileOverride {
                    availability: Some(false),
                    ..Default::default()
                },
            );
            let mut workers = CameraWorkers::new();
            cancellations.push(workers.shutdown.clone());
            let (release, blocked) = mpsc::sync_channel(1);
            workers.handles.push(std::thread::spawn(move || {
                let _ = blocked.recv();
            }));
            releases.push(release);
            recorder.camera_workers.insert(ip, workers);
        }
        let profile = crate::profiles::RuntimeProfile {
            id: "away".into(),
            name: "Away".into(),
            revision: 1,
            cameras: targets,
        };
        let selection = crate::profiles::ProfileRegistry {
            revision: 1,
            active_profile_id: Some(profile.id.clone()),
            profiles: BTreeMap::from([(profile.id.clone(), profile)]),
        };
        let start = Instant::now();
        let applications = recorder.apply_runtime_profiles(selection).unwrap();
        let elapsed = start.elapsed();
        for release in releases {
            release.send(()).unwrap();
        }
        assert!(cancellations.iter().all(Shutdown::is_cancelled));
        assert_eq!(applications.len(), 4);
        assert!(applications.iter().all(|application| {
            application.state == crate::profiles::RuntimeApplicationState::Degraded
                && application
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("did not stop"))
        }));
        assert!(
            elapsed < 3 * CAMERA_STOP_BUDGET,
            "four workers must share one stop budget, elapsed {elapsed:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while !recorder.retiring_workers.is_empty() {
            recorder.reap_stopped_cameras();
            assert!(Instant::now() < deadline, "finished workers must be joined");
            std::thread::yield_now();
        }
        eprintln!("four blocked workers, shared stop deadline: {elapsed:?}");
    }

    #[test]
    fn runtime_profile_supersession_during_worker_stop_preserves_pending_status() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let camera = runtime_rtsp_camera(listener.local_addr().unwrap());
        let ip = camera.config.ip;
        recorder.camera_definitions.insert(ip, camera);
        let mut workers = CameraWorkers::new();
        let shutdown = workers.shutdown.clone();
        let (stopped_tx, stopped_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        workers.handles.push(std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !shutdown.is_cancelled() {
                assert!(Instant::now() < deadline, "camera stop was not requested");
                std::thread::sleep(Duration::from_millis(1));
            }
            stopped_tx.send(()).unwrap();
            resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }));
        recorder.camera_workers.insert(ip, workers);
        let status = recorder.runtime_profile_status.clone();
        let profiles: crate::profiles::ProfileRegistry =
            serde_json::from_value(serde_json::json!({
                "revision": 1,
                "active_profile_id": "away",
                "profiles": { "away": {
                    "id": "away", "name": "Away", "revision": 1,
                    "cameras": { ip.to_string(): { "availability": false } }
                }}
            }))
            .unwrap();
        let transition = std::thread::spawn(move || {
            let result = recorder.apply_runtime_profiles(profiles);
            (recorder, result)
        });
        stopped_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let pending = {
            let mut pending = status.write().unwrap();
            pending.revision = 2;
            pending.active_profile_id = None;
            pending.clone()
        };
        resume_tx.send(()).unwrap();
        let (mut recorder, result) = transition.join().unwrap();
        assert!(result.is_err());
        assert_eq!(*status.read().unwrap(), pending);
        assert!(!recorder.camera_workers.contains_key(&ip));
        assert!(
            recorder
                .apply_runtime_profiles(crate::profiles::ProfileRegistry {
                    revision: 2,
                    ..Default::default()
                })
                .is_ok()
        );
        assert!(recorder.camera_workers.contains_key(&ip));
        let _connection = accept_rtsp_connection(&listener);
        recorder.stop_camera(ip).unwrap();
    }

    #[test]
    fn explicit_reo_proto_takes_precedence_over_saved_rtsp_urls() {
        let config = CameraConfig {
            events: Default::default(),
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
            record_generic_motion_events: false,
            recording_mode: Default::default(),
            event_recording_duration_secs: 60,
            event_pre_recording_duration_secs: 0,
            event_recording_stream: Default::default(),
            recorded_audio: Default::default(),
            live_audio: Default::default(),
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
    fn rtsp_profile_workers_begin_connecting_in_parallel() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = Shutdown::new();
        let mut loop_ = KeepPeekLoop::new(shutdown.clone(), None);
        let camera = Camera {
            config: CameraConfig {
                events: Default::default(),
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                name: Some("parallel-rtsp".to_owned()),
                display_name: None,
                manufacturer: None,
                username: "operator".to_owned(),
                password: "secret".to_owned(),
                onvif_port: None,
                http_port: None,
                main_rtsp_url: Some(format!("rtsp://{address}/main")),
                sub_rtsp_url: Some(format!("rtsp://{address}/sub")),
                uid: None,
                backend: CameraBackend::Retina,
                transport: CameraTransport::Tcp,
                record_generic_motion_events: false,
                recording_mode: Default::default(),
                event_recording_duration_secs: 60,
                event_pre_recording_duration_secs: 0,
                event_recording_stream: Default::default(),
                recorded_audio: Default::default(),
                live_audio: Default::default(),
            },
            device: DeviceInfo::default(),
            reported_manufacturer: None,
            hostname: None,
            mac_address: None,
            ports: CameraPorts {
                rtsp: Some(address.port()),
                ..CameraPorts::default()
            },
            capabilities: CameraCapabilities::default(),
            event_service: None,
            profiles: vec![
                MediaProfile {
                    token: "main".to_owned(),
                    name: "Main".to_owned(),
                    stream_uri: None,
                    snapshot_uri: None,
                    video: None,
                    audio: None,
                },
                MediaProfile {
                    token: "sub".to_owned(),
                    name: "Sub".to_owned(),
                    stream_uri: None,
                    snapshot_uri: None,
                    video: None,
                    audio: None,
                },
            ],
            is_reolink: false,
            ptz: None,
            imaging: None,
        };

        loop_.add_camera(&camera, true, true).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut connections = Vec::new();
        while connections.len() < 2 && Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => connections.push(stream),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("RTSP listener failed: {error}"),
            }
        }

        let worker_count = connections.len();
        drop(connections);
        shutdown.cancel();
        loop_.run();

        assert_eq!(
            worker_count, 2,
            "main and sub workers must connect concurrently"
        );
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
                    events: Default::default(),
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
                    record_generic_motion_events: false,
                    recording_mode: Default::default(),
                    event_recording_duration_secs: 60,
                    event_pre_recording_duration_secs: 0,
                    event_recording_stream: Default::default(),
                    recorded_audio: Default::default(),
                    live_audio: Default::default(),
                },
                device: DeviceInfo::default(),
                reported_manufacturer: None,
                hostname: None,
                mac_address: None,
                ports: CameraPorts::default(),
                capabilities: CameraCapabilities::default(),
                event_service: None,
                profiles: Vec::new(),
                is_reolink: false,
                ptz: None,
                imaging: None,
            })
            .unwrap();

        shutdown.cancel();
        handle.join().unwrap();
    }

    #[test]
    fn an_unresponsive_camera_worker_cannot_block_the_control_loop_or_be_replaced() {
        let shutdown = Shutdown::new();
        let mut recorder = KeepPeekLoop::new(shutdown.clone(), None);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let camera = runtime_rtsp_camera(listener.local_addr().unwrap());
        let ip = camera.config.ip;
        let mut workers = CameraWorkers::new();
        let stopping = workers.shutdown.clone();
        let (release, blocked) = mpsc::sync_channel(1);
        workers.handles.push(std::thread::spawn(move || {
            let _ = blocked.recv();
        }));
        recorder.camera_workers.insert(ip, workers);
        let control = recorder.control();
        let loop_thread = std::thread::spawn(move || recorder.run());
        let (done, result) = mpsc::sync_channel(1);
        let request = control.clone();
        let caller = std::thread::spawn(move || done.send(request.stop_camera(ip)).unwrap());
        let cancelled = stopping.wait_timeout(Duration::from_secs(5));
        let outcome = result.recv_timeout(CAMERA_STOP_BUDGET + Duration::from_secs(2));
        let replacement = if matches!(outcome, Ok(Err(_))) {
            Some(control.start_camera(camera))
        } else {
            None
        };
        assert!(listener.accept().is_err());
        release.send(()).unwrap();
        let _connection = accept_rtsp_connection(&listener);
        shutdown.cancel();
        caller.join().unwrap();
        loop_thread.join().unwrap();
        assert!(cancelled, "the worker must receive cancellation");
        assert!(
            matches!(outcome, Ok(Err(_))),
            "the stop must report its deadline while the worker is blocked: {outcome:?}"
        );
        assert!(
            matches!(replacement, Some(Err(_))),
            "a replacement must wait for the previous worker to exit"
        );
    }

    #[test]
    fn retired_worker_capacity_preserves_existing_starts_and_disabled_definitions() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let mut camera = runtime_rtsp_camera((Ipv4Addr::LOCALHOST, 0).into());
        camera.config.ip = Ipv4Addr::new(127, 0, 1, 1).into();
        for index in 1..=crate::profiles::MAX_PROFILE_TARGETS {
            let ip = Ipv4Addr::new(127, 0, 0, u8::try_from(index).unwrap()).into();
            recorder.retiring_workers.insert(ip, CameraWorkers::new());
        }
        assert!(recorder.ensure_worker_slot(&camera).is_err());
        let id = camera.config.ip.to_string();
        let disabled = serde_json::from_value(serde_json::json!({
            "revision": 1, "active_profile_id": "away", "profiles": { "away": {
                "id": "away", "name":"Away","revision":1, "cameras": { id: {"availability": false} }
            }}
        }))
        .unwrap();
        recorder.set_runtime_profiles(disabled);
        recorder.ensure_worker_slot(&camera).unwrap();
        recorder.reap_stopped_cameras();
        assert!(recorder.retiring_workers.is_empty());
        recorder.set_runtime_profiles(crate::profiles::ProfileRegistry::default());
        recorder.ensure_worker_slot(&camera).unwrap();
    }

    #[test]
    fn event_worker_timeout_keeps_its_handle_and_media_until_retry() {
        let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
        let ip = Ipv4Addr::LOCALHOST.into();
        let mut workers = CameraWorkers::new();
        let media = workers.shutdown.clone();
        let (release, blocked) = mpsc::sync_channel(1);
        workers.event_handles.push(std::thread::spawn(move || {
            let _ = blocked.recv();
        }));
        recorder.camera_workers.insert(ip, workers);
        let result = recorder.stop_camera_events(ip);
        let retained = recorder.camera_workers[&ip].event_handles.len();
        release.send(()).unwrap();
        assert!(result.is_err());
        assert_eq!(retained, 1);
        assert!(!media.is_cancelled());
        recorder.stop_camera_events(ip).unwrap();
        assert!(recorder.camera_workers[&ip].event_handles.is_empty());
        assert!(!recorder.camera_workers[&ip].event_shutdown.is_cancelled());
        recorder.stop_camera(ip).unwrap();
    }

    #[test]
    fn control_restarts_camera_workers_without_stopping_the_loop() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let camera = runtime_rtsp_camera(listener.local_addr().unwrap());
        let shutdown = Shutdown::new();
        let loop_ = KeepPeekLoop::new(shutdown.clone(), None);
        let control = loop_.control();
        let handle = std::thread::spawn(move || loop_.run());

        control.start_camera(camera.clone()).unwrap();
        let first_connection = accept_rtsp_connection(&listener);

        control.restart_camera(camera).unwrap();
        let second_connection = accept_rtsp_connection(&listener);

        assert_ne!(
            first_connection.peer_addr().unwrap(),
            second_connection.peer_addr().unwrap()
        );
        shutdown.cancel();
        handle.join().unwrap();
    }
}
