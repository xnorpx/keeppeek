use crate::api::proto::{
    self, camera_configuration_command, camera_control_command, event_search_command,
    health_command, logging_command, ok as control_ok, optional_string_update,
    request as control_request, response as control_response, runtime_configuration_command,
    server_command, stored_media_command,
};
use crate::{
    access::{AccessKey, AccessKeyFingerprint},
    api::{
        ApiError, AudioProfileSummary, CameraInfo, CameraLifecycle, CameraStatus, CreateRequest,
        CreateResponse, DeleteRequest, MotionDetection, ProfileSummary, RecordingCapacityEstimate,
        SanitizedConfig, SanitizedStorage, SdpAnswer as ApiSdpAnswer, Status,
    },
    battery_wake::BatteryWakeHandle,
    camera_database::{CameraDatabase, CameraMatch, CatalogCamera, StreamHints},
    cameras::{
        Camera, CameraBackend, CameraConfig, CameraPorts, CameraRecordingMode, CameraTransport,
        MediaProfile, probe_onvif_camera,
        reolink::{PtzOp, ReolinkClient},
    },
    config::{self, Config, StorageMigration, StorageMigrationPaths, StorageToml},
    health::{
        CAMERA_HEALTH_CONTRACT_VERSION, CameraHealth, CameraHealthDimensions, CameraHealthEvidence,
        CameraHealthState, HealthIssue, HealthTotals, STREAM_REPORT_FRESHNESS_THRESHOLD_MS,
        ServerHealthResponse, StorageHealth, StreamHealth, StreamHealthDimensions, SystemMonitor,
        project_camera_health,
    },
    keeppeek::{KeepPeekControl, StreamKind},
    logging::{LogStreamError, LoggingService, LoggingSettings},
    rtsp::{RtspTransport, probe_rtsp_video},
    runtime::{
        FacadeSendError, FacadeSender, RouterError, RouterMessage, RouterQuery, RouterResponse,
    },
    shutdown::{Restart, Shutdown},
    stats::{HealthRegistry, REPORT_INTERVAL, StreamHealthReport},
    storage::{
        CatalogMediaFragment, DEFAULT_PREVIEW_AFTER_MS, DEFAULT_PREVIEW_BEFORE_MS, EventEmbedding,
        EventSearch, EventSearchField, EventSearchTerm, EventSemanticSearchQuery, EventStore,
        EventTextSearchQuery, RecordingCatalogHandle, RecordingDemand, RecordingDemandGuard,
        RecordingHealthRegistry, RecordingStreamHealthSnapshot, StorageConfig,
        metadata::{EventSource, TimelineEvent},
    },
    webrtc::{
        ControlDispatch, ControlHandlerError, ControlRequestHandler, DataChannelTarget,
        MediaSubscriptionPlan, OutboundDataMessage, PostSendAction, SessionId, StreamQuality,
        WebRtc,
    },
};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use include_dir::{Dir, File as EmbeddedFile, include_dir};
use rouille::{Request, Response, ResponseBody, Server, router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom, Write},
    net::{IpAddr, Ipv4Addr, TcpListener, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use url::Url;

const SERVER_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SERVER_SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(3);
const ROUTER_REPLY_TIMEOUT: Duration = Duration::from_secs(2);
const TEST_RECORDING_DEMAND_GRACE: Duration = Duration::from_secs(30);
const MAX_WRITE_BUFFER_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_LOG_STREAM_TAIL: usize = 200;
const MAX_LOG_STREAM_TAIL: usize = 1_000;
const MEBIBYTE_BYTES: u64 = 1_048_576;
const GIBIBYTE_BYTES: u64 = 1_073_741_824;
const MAX_CREATE_BODY_BYTES: u64 = 4 * 1_024 * 1_024;
const PUBLISHED_DETECTION_EVENT_TYPES: [&str; 2] = ["person", "vehicle"];
const STORED_QUERY_PAGE_ITEMS: usize = 128;
const DATA_MESSAGE_CHUNK_BYTES: usize = 32 * 1_024;
const DEFAULT_STORED_MEDIA_BUFFER: Duration = Duration::from_secs(120);
const MAX_STORED_MEDIA_BUFFER: Duration = Duration::from_secs(300);
const MAX_STORED_OBJECT_BYTES: u64 = 256 * 1_024 * 1_024;
const MAX_STORED_KEYFRAME_BYTES: u64 = 4 * 1_024 * 1_024;
const STORED_MEDIA_TARGET_BUFFER_MS: u64 = 10_000;
const MAX_EVENT_SEARCH_MEDIA_OBJECTS: usize = 64;
const MAX_EVENT_SEARCH_MEDIA_BYTES: u64 = 32 * 1_024 * 1_024;
const MAX_EVENT_SEARCH_TASKS_PER_SESSION: usize = 4;
const MAX_EVENT_SEARCH_TASKS: usize = 64;
type EventSearchTaskKey = (SessionId, String);
type EventSearchTasks = Arc<Mutex<HashMap<EventSearchTaskKey, Arc<AtomicBool>>>>;
type CameraDiscoveryTaskKey = (SessionId, String);
type CameraDiscoveryTasks = Arc<Mutex<HashMap<CameraDiscoveryTaskKey, CameraDiscoveryTask>>>;
const PTZ_STOP_SPEED: u32 = 32;
const EXPORT_JOB_EXPIRY: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_EXPORT_DURATION: Duration = Duration::from_secs(2 * 60);
const MAX_EXPORT_DOWNLOAD_BYTES: u64 = 512 * 1_024 * 1_024;
const CAMERA_CATALOG_WEBSITE: &str = "https://www.cctv-database.com/";
const DEFAULT_CAMERA_CATALOG_SEARCH_LIMIT: usize = 20;
const CAMERA_STREAM_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CAMERA_DISCOVERY_TASKS: usize = 16;
const MAX_CAMERA_METADATA_WORKERS: usize = 4;
// Limits user-provided search text before it becomes part of in-memory matching work.
const MAX_CAMERA_CATALOG_QUERY_CHARS: usize = 128;

static UI_ASSETS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/ui/build");

#[derive(Default, Deserialize)]
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
    record_generic_motion_events: Option<bool>,
    recording_mode: Option<CameraRecordingMode>,
    event_recording_duration_secs: Option<u64>,
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
    record_generic_motion_events: bool,
    recording_mode: String,
    event_recording_duration_secs: u64,
    health: Option<String>,
    model: Option<String>,
}

struct DiscoveredCameraSettings {
    ip: String,
    brand: String,
    name: Option<String>,
    model: Option<String>,
    onvif_port: Option<u16>,
    sources: Vec<String>,
    configured: bool,
    health: Option<String>,
    catalog: Option<DiscoveredCameraCatalog>,
}

struct DiscoveredCameraCatalog {
    camera: CatalogCamera,
    stream_hints: Option<StreamHints>,
}

#[derive(Clone)]
struct CameraDiscoveryTask {
    cameras: Vec<crate::cameras::DiscoveredCamera>,
    complete: bool,
    cancelled: Arc<AtomicBool>,
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

#[derive(Clone)]
struct CameraEntry {
    info: CameraInfo,
    reported_manufacturer: Option<String>,
    configuration: CameraConfig,
    battery_uid: Option<String>,
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

struct ServerControlHandler {
    state: ServerState,
    router_tx: FacadeSender<RouterMessage>,
}

#[derive(Debug)]
struct ControlCommandError {
    code: proto::ErrorCode,
    _http_status: u16,
    message: String,
}

impl ControlCommandError {
    fn new(code: proto::ErrorCode, http_status: u16, message: impl Into<String>) -> Self {
        Self {
            code,
            _http_status: http_status,
            message: message.into(),
        }
    }
}

impl ControlRequestHandler for ServerControlHandler {
    fn handle(&self, request: proto::Request) -> ControlDispatch {
        self.handle_for_session(SessionId::from_u64(0), request)
    }

    fn handle_for_session(
        &self,
        session_id: SessionId,
        request: proto::Request,
    ) -> ControlDispatch {
        let request_id = request.request_id;
        let mut after_send = None;
        let mut data_messages = Vec::new();
        let mut notifications = Vec::new();
        let result = match request.command {
            Some(control_request::Command::CameraControlCommand(command)) => {
                self.handle_camera_control(session_id, command).map(Some)
            }
            Some(control_request::Command::CameraConfigurationCommand(command)) => self
                .handle_camera_configuration(session_id, command)
                .map(Some),
            Some(control_request::Command::LoggingCommand(command)) => {
                self.handle_logging(command).map(Some)
            }
            Some(control_request::Command::ServerCommand(command)) => {
                match self.handle_server(session_id, command) {
                    Ok((result, action)) => {
                        after_send = Some(action);
                        Ok(Some(result))
                    }
                    Err(error) => Err(error),
                }
            }
            Some(control_request::Command::RuntimeConfigurationCommand(command)) => {
                self.handle_runtime_configuration(command).map(Some)
            }
            Some(control_request::Command::HealthCommand(command)) => {
                self.handle_health(command).map(Some)
            }
            Some(control_request::Command::ExportCommand(command)) => {
                match self.handle_export(command) {
                    Ok((result, messages)) => {
                        data_messages = messages;
                        Ok(Some(result))
                    }
                    Err(error) => Err(error),
                }
            }
            Some(control_request::Command::EventSearchCommand(command)) => {
                match self.handle_event_search(session_id, command) {
                    Ok((result, messages)) => {
                        data_messages = messages;
                        Ok(result)
                    }
                    Err(error) => Err(error),
                }
            }
            Some(control_request::Command::StoredMediaCommand(command)) => {
                match self.handle_stored_media(session_id, command) {
                    Ok(dispatch) => {
                        data_messages = dispatch.messages;
                        notifications = dispatch.notifications;
                        Ok(dispatch.result)
                    }
                    Err(error) => Err(error),
                }
            }
            Some(control_request::Command::PublishEvent(command)) => {
                self.handle_publish_event(command).map(|()| None)
            }
            Some(_) => Err(ControlCommandError::new(
                proto::ErrorCode::UnsupportedRequest,
                501,
                "control command is not implemented by this server",
            )),
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "control request has no command",
            )),
        };
        ControlDispatch {
            response: proto::Response {
                request_id,
                result: Some(match result {
                    Ok(result) => control_response::Result::Ok(proto::Ok { result }),
                    Err(error) => control_response::Result::Error(proto::Error {
                        code: error.code as i32,
                        message: error.message,
                        details: Vec::new(),
                    }),
                }),
            },
            after_send,
            data_messages,
            notifications,
        }
    }

    fn session_closed(&self, session_id: SessionId) {
        let cancelled = self
            .state
            .event_search_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|((owner_session_id, _), _)| *owner_session_id == session_id)
            .map(|(_, cancelled)| cancelled.clone())
            .collect::<Vec<_>>();
        for token in cancelled {
            token.store(true, Ordering::Release);
        }
        let discovery_tasks = {
            let mut tasks = self
                .state
                .camera_discovery_tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let cancelled = tasks
                .iter()
                .filter(|((owner_session_id, _), _)| *owner_session_id == session_id)
                .map(|(_, task)| task.cancelled.clone())
                .collect::<Vec<_>>();
            tasks.retain(|(owner_session_id, _), _| *owner_session_id != session_id);
            cancelled
        };
        for token in discovery_tasks {
            token.store(true, Ordering::Release);
        }
        self.state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(owner_session_id, _), _| *owner_session_id != session_id);
        let source_ids = {
            let mut owners = self
                .state
                .ptz_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let source_ids = owners
                .iter()
                .filter_map(|(source_id, owner)| {
                    (*owner == session_id).then_some(source_id.clone())
                })
                .collect::<Vec<_>>();
            owners.retain(|_, owner| *owner != session_id);
            source_ids
        };
        for source_id in source_ids {
            if let Some(camera) = self.state.camera(&source_id)
                && let Some(control) = camera.control
                && let Err(error) = reolink_ptz(&control, PtzOp::Stop, PTZ_STOP_SPEED)
            {
                tracing::warn!(%source_id, %error, "unable to stop session-owned PTZ movement");
            }
        }
    }

    fn initial_capabilities(&self, session_id: SessionId) -> Option<proto::ServerCapabilities> {
        let self_source_session_id = format!("webrtc-client-{session_id}");
        let camera_entries = self.state.camera_entries();
        let camera_info = camera_entries
            .iter()
            .map(|camera| self.state.camera_info(camera))
            .collect::<Vec<_>>();
        let cameras = camera_entries
            .iter()
            .zip(camera_info.iter())
            .map(|(entry, info)| proto_camera_info(info, entry.control.is_some()))
            .collect();
        let mut source_sessions = vec![proto::SourceSession {
            source_session_id: self_source_session_id.clone(),
            source_id: String::new(),
            display_name: "WebRTC client".to_owned(),
            audio: None,
            video: None,
            data_payloads: Vec::new(),
            event_types: Vec::new(),
            publication_capabilities: Vec::new(),
        }];
        source_sessions.extend(
            camera_info
                .iter()
                .filter_map(|camera| proto_camera_source_session(camera, &self.state.webrtc)),
        );
        let stored_media_sources = camera_info
            .iter()
            .filter_map(proto_camera_stored_media_source)
            .collect();
        Some(proto::ServerCapabilities {
            revision: 1,
            cameras,
            source_sessions,
            stored_media_sources,
            self_source_session_id,
            capability_ids: vec![
                "keeppeek.media-export.v1".to_owned(),
                "keeppeek.event-search".to_owned(),
                "keeppeek.event-publication.v1".to_owned(),
                "stored-media-keyframe-preview.v1".to_owned(),
            ],
        })
    }

    fn resolve_media_subscription(
        &self,
        request: &proto::SubscribeMedia,
    ) -> Result<MediaSubscriptionPlan, ControlHandlerError> {
        if proto::MediaKind::try_from(request.kind) != Ok(proto::MediaKind::Video) {
            return Err(ControlHandlerError::new(
                proto::ErrorCode::InvalidRequest,
                "only video media subscriptions are currently supported",
            ));
        }
        let delivery_transport = proto::DeliveryTransport::try_from(
            request.requested_delivery_transport,
        )
        .map_err(|_| {
            ControlHandlerError::new(
                proto::ErrorCode::InvalidRequest,
                "video delivery transport is invalid",
            )
        })?;
        if !matches!(
            delivery_transport,
            proto::DeliveryTransport::Rtp | proto::DeliveryTransport::ReliableData
        ) {
            return Err(ControlHandlerError::new(
                proto::ErrorCode::InvalidRequest,
                "camera video requires RTP or reliable data delivery",
            ));
        }
        let camera = self
            .state
            .camera_entries()
            .into_iter()
            .find(|camera| camera_source_session_id(&camera.info.id) == request.source_session_id)
            .ok_or_else(|| {
                ControlHandlerError::new(
                    proto::ErrorCode::NotFound,
                    "media source session not found",
                )
            })?;
        let camera_ip = camera.info.ip.parse().map_err(|_| {
            ControlHandlerError::new(
                proto::ErrorCode::Internal,
                "camera has an invalid IP address",
            )
        })?;
        let live_sources = self.state.webrtc.live_video_sources(camera_ip);
        let has_main_stream = live_sources
            .iter()
            .any(|source| source.stream == StreamKind::Main);
        let has_sub_stream = live_sources
            .iter()
            .any(|source| source.stream == StreamKind::Sub);
        if !has_main_stream && !has_sub_stream {
            return Err(ControlHandlerError::new(
                proto::ErrorCode::NotFound,
                "camera has no advertised video variant",
            ));
        }
        let requested_quality =
            proto::VideoQuality::try_from(request.video_quality).map_err(|_| {
                ControlHandlerError::new(
                    proto::ErrorCode::InvalidRequest,
                    "video quality is invalid",
                )
            })?;
        let (quality, selected_variant_id) = if request.variant_id.is_empty() {
            match requested_quality {
                proto::VideoQuality::High => {
                    if has_main_stream {
                        (StreamQuality::High, "main".to_owned())
                    } else {
                        (StreamQuality::Low, "sub".to_owned())
                    }
                }
                proto::VideoQuality::Low => {
                    if has_sub_stream {
                        (StreamQuality::Low, "sub".to_owned())
                    } else {
                        (StreamQuality::High, "main".to_owned())
                    }
                }
                proto::VideoQuality::Auto => {
                    if has_sub_stream {
                        (StreamQuality::Auto, "sub".to_owned())
                    } else {
                        (StreamQuality::Auto, "main".to_owned())
                    }
                }
            }
        } else {
            if requested_quality != proto::VideoQuality::Auto {
                return Err(ControlHandlerError::new(
                    proto::ErrorCode::InvalidRequest,
                    "an exact video variant requires automatic quality",
                ));
            }
            match request.variant_id.as_str() {
                "main" if has_main_stream => (StreamQuality::High, "main".to_owned()),
                "sub" if has_sub_stream => (StreamQuality::Low, "sub".to_owned()),
                _ => {
                    return Err(ControlHandlerError::new(
                        proto::ErrorCode::NotFound,
                        "video variant not found",
                    ));
                }
            }
        };
        let selected_variant = proto_camera_source_session(&camera.info, &self.state.webrtc)
            .and_then(|source| source.video)
            .and_then(|video| {
                video
                    .variants
                    .into_iter()
                    .find(|variant| variant.variant_id == selected_variant_id)
            })
            .ok_or_else(|| {
                ControlHandlerError::new(
                    proto::ErrorCode::NotFound,
                    "selected video variant is no longer available",
                )
            })?;
        let codec = selected_variant.codec.ok_or_else(|| {
            ControlHandlerError::new(
                proto::ErrorCode::Internal,
                "selected video variant has no codec",
            )
        })?;
        let format = selected_variant.format.ok_or_else(|| {
            ControlHandlerError::new(
                proto::ErrorCode::Internal,
                "selected video variant has no format",
            )
        })?;
        Ok(MediaSubscriptionPlan {
            source_session_id: request.source_session_id.clone(),
            camera_ip,
            has_sub_stream,
            recording_label: camera.recording_label,
            quality,
            delivery_transport,
            codec,
            format,
            selected_variant_id,
        })
    }
}

fn camera_source_session_id(source_id: &str) -> String {
    format!("camera:{source_id}")
}

fn proto_camera_source_session(
    camera: &CameraInfo,
    webrtc: &WebRtc,
) -> Option<proto::SourceSession> {
    let camera_ip = camera.ip.parse().ok()?;
    let live_sources = webrtc.live_video_sources(camera_ip);
    let variants = ["main", "sub"]
        .into_iter()
        .filter_map(|stream| {
            let live_source = live_sources
                .iter()
                .find(|source| source.stream.to_string() == stream)?;
            let profile = camera
                .profiles
                .iter()
                .find(|profile| profile.stream == stream);
            let codec = profile
                .and_then(|profile| profile.encoding.as_deref())
                .filter(|encoding| {
                    encoding.eq_ignore_ascii_case("h264") || encoding.eq_ignore_ascii_case("h265")
                })
                .map(str::to_lowercase)
                .unwrap_or_else(|| live_source.codec.to_owned());
            let (width, height) = profile
                .and_then(|profile| profile.resolution.as_deref())
                .and_then(|resolution| resolution.split_once('x'))
                .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
                .unwrap_or((0, 0));
            Some(proto::MediaVariantCapability {
                variant_id: stream.to_owned(),
                codec: Some(proto::CodecDescriptor {
                    name: codec,
                    parameters: HashMap::new(),
                }),
                format: Some(proto::MediaDataFormat {
                    format: Some(proto::media_data_format::Format::Video(
                        proto::VideoDataFormat {
                            width,
                            height,
                            decoder_config: Vec::new(),
                        },
                    )),
                }),
                delivery_transports: vec![
                    proto::DeliveryTransport::Rtp as i32,
                    proto::DeliveryTransport::ReliableData as i32,
                ],
                nominal_bitrate_bps: u64::from(
                    profile
                        .and_then(|profile| profile.bitrate_kbps)
                        .unwrap_or(0),
                ) * 1_000,
                quality_rank: if stream == "main" { 2 } else { 1 },
                origin: proto::MediaVariantOrigin::Native as i32,
                lineage: Vec::new(),
            })
        })
        .collect::<Vec<_>>();
    (!variants.is_empty()).then(|| proto::SourceSession {
        source_session_id: camera_source_session_id(&camera.id),
        source_id: camera.id.clone(),
        display_name: camera.name.clone().unwrap_or_else(|| camera.id.clone()),
        audio: None,
        video: Some(proto::MediaStreamCapability { variants }),
        data_payloads: Vec::new(),
        event_types: PUBLISHED_DETECTION_EVENT_TYPES
            .into_iter()
            .map(|event_type| proto::EventType {
                event_type: event_type.to_owned(),
                metadata: None,
                attachments: Vec::new(),
            })
            .collect(),
        publication_capabilities: Vec::new(),
    })
}

fn proto_camera_stored_media_source(
    camera: &CameraInfo,
) -> Option<proto::StoredMediaSourceCapability> {
    let mut streams = camera
        .profiles
        .iter()
        .filter(|profile| matches!(profile.stream.as_str(), "main" | "sub"))
        .map(|profile| proto::StoredMediaStreamCapability {
            stream_id: profile.stream.clone(),
            content_type: "video/mp4".to_owned(),
            delivery_channels: vec![
                proto::DataChannelKind::ReliableData as i32,
                proto::DataChannelKind::UnreliableData as i32,
            ],
        })
        .collect::<Vec<_>>();
    streams.sort_unstable_by(|left, right| left.stream_id.cmp(&right.stream_id));
    streams.dedup_by(|left, right| left.stream_id == right.stream_id);
    (!streams.is_empty()).then(|| proto::StoredMediaSourceCapability {
        source_id: camera.id.clone(),
        display_name: camera.name.clone().unwrap_or_else(|| camera.id.clone()),
        streams,
        data_payloads: Vec::new(),
    })
}

fn proto_camera_info(camera: &CameraInfo, control_available: bool) -> proto::CameraInfo {
    let ptz_supported = camera.capabilities.ptz && control_available;
    proto::CameraInfo {
        source_id: camera.id.clone(),
        display_name: camera.name.clone().unwrap_or_else(|| camera.id.clone()),
        manufacturer: camera.manufacturer.clone(),
        model: camera.model.clone(),
        firmware_version: camera.firmware_version.clone(),
        serial_number: camera.serial_number.clone(),
        hardware_id: camera.hardware_id.clone(),
        ip: Some(camera.ip.clone()),
        hostname: camera.hostname.clone(),
        mac_address: camera.mac_address.clone(),
        web_url: Some(camera.web_url.clone()),
        http_port: camera.ports.http.map(u32::from),
        https_port: camera.ports.https.map(u32::from),
        rtsp_port: camera.ports.rtsp.map(u32::from),
        onvif_port: camera.ports.onvif.map(u32::from),
        is_reolink: camera.is_reolink,
        device_capabilities: Some(proto::CameraDeviceCapabilities {
            audio: camera.capabilities.audio,
            events: camera.capabilities.events,
            recording: camera.capabilities.recording,
            analytics: camera.capabilities.analytics,
            imaging: camera.capabilities.imaging,
            two_way_audio: camera.capabilities.two_way_audio,
        }),
        ptz: Some(proto::PtzCapability {
            supported: ptz_supported,
            continuous: ptz_supported,
            relative: false,
            presets: ptz_supported,
            zoom: ptz_supported,
        }),
    }
}

impl ServerControlHandler {
    const fn new(state: ServerState, router_tx: FacadeSender<RouterMessage>) -> Self {
        Self { state, router_tx }
    }

    fn handle_publish_event(
        &self,
        command: proto::PublishEvent,
    ) -> Result<(), ControlCommandError> {
        let event = command.event.ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "published event is missing",
            )
        })?;
        validate_client_id(&event.event_id, "event ID")?;
        validate_client_id(&event.source_id, "event source ID")?;
        if event.revision != 1 {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "envelope-only event revision must be one",
            ));
        }
        if event.subscription_id.is_some() || !event.attachments.is_empty() {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "envelope-only event publication cannot include subscription or attachment data",
            ));
        }
        if !PUBLISHED_DETECTION_EVENT_TYPES.contains(&event.event_type.as_str()) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::UnsupportedRequest,
                400,
                "published event type must be person or vehicle",
            ));
        }
        if let Some(media_kind) = event.media_kind
            && proto::MediaKind::try_from(media_kind) != Ok(proto::MediaKind::Video)
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "published detection media kind must be video",
            ));
        }
        let source_session_id = event.source_session_id.as_deref().ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "published event source session is missing",
            )
        })?;
        let camera = self
            .state
            .camera_entries()
            .into_iter()
            .find(|camera| camera.info.id == event.source_id)
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::NotFound,
                    404,
                    "published event source was not found",
                )
            })?;
        if camera_source_session_id(&camera.info.id) != source_session_id
            || proto_camera_source_session(&camera.info, &self.state.webrtc).is_none()
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "published event source session is not active",
            ));
        }
        let start_time_ms = required_timestamp_ms(event.start_time.as_ref(), "event start time")?;
        let end_time_ms = event
            .end_time
            .as_ref()
            .map(|timestamp| required_timestamp_ms(Some(timestamp), "event end time"))
            .transpose()?;
        if end_time_ms.is_some_and(|end| end < start_time_ms) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event end time precedes its start time",
            ));
        }
        if event
            .confidence
            .is_some_and(|confidence| !confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event confidence must be between zero and one",
            ));
        }
        let bbox = event
            .bounding_box
            .as_ref()
            .map(|bbox| [bbox.x, bbox.y, bbox.width, bbox.height]);
        if bbox.is_some_and(|[x, y, width, height]| {
            [x, y, width, height]
                .into_iter()
                .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
                || x + width > 1.0
                || y + height > 1.0
        }) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event bounding box must be normalized within the frame",
            ));
        }
        let stream = event
            .payload
            .as_ref()
            .and_then(|payload| payload.fields.get("stream_id"))
            .and_then(|value| value.kind.as_ref())
            .and_then(|kind| match kind {
                prost_types::value::Kind::StringValue(stream) => Some(stream.as_str()),
                _ => None,
            })
            .filter(|stream| matches!(*stream, "main" | "sub"))
            .map(str::to_owned);
        let stored_event = TimelineEvent {
            id: event.event_id,
            camera_id: event.source_id,
            stream,
            source: EventSource::KeepPeek,
            kind: event.event_type,
            start_time_ms,
            end_time_ms,
            confidence: event.confidence,
            bbox,
            zone: event.zone,
            thumbnail_filename: None,
        };
        let events = self.state.events.as_ref().ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "event storage is unavailable",
            )
        })?;
        if let Some(existing) = events.event_by_id(&stored_event.id).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                format!("unable to read existing event: {error}"),
            )
        })? {
            return if existing == stored_event {
                Ok(())
            } else {
                Err(ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    409,
                    "event ID already exists with different content",
                ))
            };
        }
        events.insert(stored_event).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                format!("unable to store published event: {error}"),
            )
        })
    }

    fn camera_database(&self) -> Result<&CameraDatabase, ControlCommandError> {
        self.state.camera_database.as_deref().ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "camera catalog is unavailable",
            )
        })
    }

    fn handle_camera_control(
        &self,
        session_id: SessionId,
        command: proto::CameraControlCommand,
    ) -> Result<control_ok::Result, ControlCommandError> {
        match command.action {
            Some(camera_control_command::Action::Ptz(command)) => {
                self.handle_ptz(session_id, command)
            }
            Some(camera_control_command::Action::GetMotionDetection(request)) => {
                let Some(camera) = self.state.camera(&request.source_id) else {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::NotFound,
                        404,
                        "camera not found",
                    ));
                };
                Ok(control_ok::Result::MotionDetectionResult(
                    proto_motion_detection(motion_detection_status(&camera)),
                ))
            }
            Some(camera_control_command::Action::SetMotionDetection(update)) => {
                let result = set_camera_motion(&self.state, &update.source_id, update.enabled)?;
                Ok(control_ok::Result::MotionDetectionResult(
                    proto_motion_detection(result),
                ))
            }
            Some(camera_control_command::Action::SetManufacturer(update)) => {
                let manufacturer = match update.manufacturer.and_then(|update| update.value) {
                    Some(optional_string_update::Value::Set(manufacturer)) => Some(manufacturer),
                    Some(optional_string_update::Value::Clear(true)) => None,
                    Some(optional_string_update::Value::Clear(false)) | None => {
                        return Err(ControlCommandError::new(
                            proto::ErrorCode::InvalidRequest,
                            400,
                            "manufacturer update must set or clear the override",
                        ));
                    }
                };
                let camera = set_camera_manufacturer(&self.state, &update.source_id, manufacturer)?;
                Ok(control_ok::Result::CameraManufacturerResult(
                    proto::CameraManufacturerResult {
                        source_id: camera.id,
                        manufacturer: camera.manufacturer,
                    },
                ))
            }
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "camera control command has no action",
            )),
        }
    }

    fn handle_ptz(
        &self,
        session_id: SessionId,
        command: proto::PtzCommand,
    ) -> Result<control_ok::Result, ControlCommandError> {
        let camera = self.state.camera(&command.source_id).ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::NotFound, 404, "camera not found")
        })?;
        if !camera.info.capabilities.ptz {
            return Err(ControlCommandError::new(
                proto::ErrorCode::UnsupportedRequest,
                501,
                "camera does not report PTZ support",
            ));
        }
        let control = camera.control.ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                409,
                "PTZ command transport is unavailable for this camera",
            )
        })?;
        let result = match command.action {
            Some(proto::ptz_command::Action::Continuous(continuous)) => {
                let (operation, speed) = ptz_continuous_operation(&continuous)?;
                {
                    let mut owners = self
                        .state
                        .ptz_owners
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if owners
                        .get(&command.source_id)
                        .is_some_and(|owner| *owner != session_id)
                    {
                        return Err(ControlCommandError::new(
                            proto::ErrorCode::Rejected,
                            409,
                            "camera PTZ is owned by another connection",
                        ));
                    }
                    owners.insert(command.source_id.clone(), session_id);
                }
                if let Err(error) = reolink_ptz(&control, operation, speed) {
                    self.release_ptz_owner(&command.source_id, session_id);
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::Unavailable,
                        502,
                        format!("camera PTZ movement failed: {error}"),
                    ));
                }
                proto::PtzResult {
                    source_id: command.source_id,
                    presets: Vec::new(),
                }
            }
            Some(proto::ptz_command::Action::Stop(_)) => {
                self.require_ptz_owner(&command.source_id, session_id)?;
                reolink_ptz(&control, PtzOp::Stop, PTZ_STOP_SPEED).map_err(|error| {
                    ControlCommandError::new(
                        proto::ErrorCode::Unavailable,
                        502,
                        format!("camera PTZ stop failed: {error}"),
                    )
                })?;
                self.release_ptz_owner(&command.source_id, session_id);
                proto::PtzResult {
                    source_id: command.source_id,
                    presets: Vec::new(),
                }
            }
            Some(proto::ptz_command::Action::ListPresets(_)) => proto::PtzResult {
                source_id: command.source_id,
                presets: reolink_ptz_presets(&control)?
                    .into_iter()
                    .map(proto_ptz_preset)
                    .collect::<Result<Vec<_>, _>>()?,
            },
            Some(proto::ptz_command::Action::GotoPreset(goto)) => {
                self.require_ptz_unowned(&command.source_id, session_id)?;
                if goto.preset_id == 0 {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "PTZ preset ID must be nonzero",
                    ));
                }
                reolink_goto_preset(&control, goto.preset_id).map_err(|error| {
                    ControlCommandError::new(
                        proto::ErrorCode::Unavailable,
                        502,
                        format!("camera PTZ preset failed: {error}"),
                    )
                })?;
                proto::PtzResult {
                    source_id: command.source_id,
                    presets: Vec::new(),
                }
            }
            Some(
                proto::ptz_command::Action::Relative(_)
                | proto::ptz_command::Action::SavePreset(_)
                | proto::ptz_command::Action::DeletePreset(_),
            ) => {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::UnsupportedRequest,
                    501,
                    "PTZ action is not implemented by this camera transport",
                ));
            }
            None => {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "PTZ command has no action",
                ));
            }
        };
        Ok(control_ok::Result::PtzResult(result))
    }

    fn release_ptz_owner(&self, source_id: &str, session_id: SessionId) {
        let mut owners = self
            .state
            .ptz_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if owners.get(source_id) == Some(&session_id) {
            owners.remove(source_id);
        }
    }

    fn require_ptz_owner(
        &self,
        source_id: &str,
        session_id: SessionId,
    ) -> Result<(), ControlCommandError> {
        let owners = self
            .state
            .ptz_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if owners
            .get(source_id)
            .is_some_and(|owner| *owner != session_id)
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "camera PTZ is owned by another connection",
            ));
        }
        Ok(())
    }

    fn require_ptz_unowned(
        &self,
        source_id: &str,
        session_id: SessionId,
    ) -> Result<(), ControlCommandError> {
        self.require_ptz_owner(source_id, session_id)?;
        let owners = self
            .state
            .ptz_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if owners.contains_key(source_id) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "stop continuous PTZ movement before selecting a preset",
            ));
        }
        Ok(())
    }

    fn handle_logging(
        &self,
        command: proto::LoggingCommand,
    ) -> Result<control_ok::Result, ControlCommandError> {
        let settings = match command.action {
            Some(logging_command::Action::GetSettings(_)) => get_logging_settings(&self.state)?,
            Some(logging_command::Action::SetFilter(update)) => {
                set_logging_filter(&self.state, &update.filter)?
            }
            None => {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "logging command has no action",
                ));
            }
        };
        Ok(control_ok::Result::LoggingSettingsResult(
            proto_logging_settings(settings),
        ))
    }

    fn handle_camera_configuration(
        &self,
        session_id: SessionId,
        command: proto::CameraConfigurationCommand,
    ) -> Result<control_ok::Result, ControlCommandError> {
        match command.action {
            Some(camera_configuration_command::Action::Get(_)) => Ok(
                control_ok::Result::CameraConfigurationResult(proto::CameraConfigurationResult {
                    camera: None,
                    restart_required: false,
                    removed: false,
                    cameras: camera_settings(&self.router_tx, &self.state)
                        .into_iter()
                        .map(proto_camera_settings)
                        .collect(),
                }),
            ),
            Some(camera_configuration_command::Action::Discover(request)) => {
                let discovery_id = request.discovery_id.trim().to_owned();
                if discovery_id.len() > 128 || discovery_id.chars().any(char::is_control) {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "camera discovery ID must be at most 128 printable characters",
                    ));
                }
                let task_key =
                    (!discovery_id.is_empty()).then(|| (session_id, discovery_id.clone()));
                if let Some(key) = &task_key {
                    let mut tasks = self
                        .state
                        .camera_discovery_tasks
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if tasks.len() >= MAX_CAMERA_DISCOVERY_TASKS && !tasks.contains_key(key) {
                        return Err(ControlCommandError::new(
                            proto::ErrorCode::Rejected,
                            429,
                            "too many camera discovery tasks are active",
                        ));
                    }
                    tasks.insert(
                        key.clone(),
                        CameraDiscoveryTask {
                            cameras: Vec::new(),
                            complete: false,
                            cancelled: Arc::new(AtomicBool::new(false)),
                        },
                    );
                }
                let networks = request
                    .networks
                    .iter()
                    .map(|network| {
                        let network = network.parse::<ipnet::Ipv4Net>().map_err(|_| {
                            ControlCommandError::new(
                                proto::ErrorCode::InvalidRequest,
                                400,
                                "camera discovery networks must be IPv4 CIDRs",
                            )
                        })?;
                        if network.prefix_len() != 24 || !network.network().is_private() {
                            return Err(ControlCommandError::new(
                                proto::ErrorCode::InvalidRequest,
                                400,
                                "camera discovery networks must be private IPv4 /24 CIDRs",
                            ));
                        }
                        Ok(network)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let subnets = request
                    .subnets
                    .into_iter()
                    .map(|subnet| {
                        u8::try_from(subnet).map_err(|_| {
                            ControlCommandError::new(
                                proto::ErrorCode::InvalidRequest,
                                400,
                                "camera discovery subnet prefixes must be between 0 and 255",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let cameras = discover_camera_settings(
                    networks,
                    subnets,
                    &self.router_tx,
                    &self.state,
                    task_key.as_ref(),
                )?;
                let cancelled = task_key
                    .as_ref()
                    .and_then(|key| {
                        self.state
                            .camera_discovery_tasks
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .get_mut(key)
                            .map(|task| {
                                task.complete = true;
                                task.cancelled.load(Ordering::Acquire)
                            })
                    })
                    .unwrap_or(false);
                if let Some(key) = &task_key {
                    self.state
                        .camera_discovery_tasks
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(key);
                }
                Ok(control_ok::Result::CameraDiscoveryResult(
                    proto_camera_discovery_result(discovery_id, cameras, true, cancelled),
                ))
            }
            Some(camera_configuration_command::Action::GetDiscovery(request)) => {
                let key = (session_id, request.discovery_id.clone());
                let task = self
                    .state
                    .camera_discovery_tasks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| {
                        ControlCommandError::new(
                            proto::ErrorCode::InvalidRequest,
                            404,
                            "camera discovery task was not found",
                        )
                    })?;
                let cameras =
                    present_discovered_cameras(task.cameras, &self.router_tx, &self.state);
                Ok(control_ok::Result::CameraDiscoveryResult(
                    proto_camera_discovery_result(
                        request.discovery_id,
                        cameras,
                        task.complete,
                        task.cancelled.load(Ordering::Acquire),
                    ),
                ))
            }
            Some(camera_configuration_command::Action::CancelDiscovery(request)) => {
                let key = (session_id, request.discovery_id.clone());
                let task = self
                    .state
                    .camera_discovery_tasks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| {
                        ControlCommandError::new(
                            proto::ErrorCode::InvalidRequest,
                            404,
                            "camera discovery task was not found",
                        )
                    })?;
                task.cancelled.store(true, Ordering::Release);
                let cameras =
                    present_discovered_cameras(task.cameras, &self.router_tx, &self.state);
                Ok(control_ok::Result::CameraDiscoveryResult(
                    proto_camera_discovery_result(request.discovery_id, cameras, false, true),
                ))
            }
            Some(camera_configuration_command::Action::ProbeStreams(request)) => {
                self.handle_camera_stream_probe(request)
            }
            Some(camera_configuration_command::Action::GetCatalog(_)) => {
                Ok(control_ok::Result::CameraCatalogInfo(
                    proto_camera_catalog_info(self.camera_database()?),
                ))
            }
            Some(camera_configuration_command::Action::GetOnboardingDefaults(_)) => {
                let defaults = self
                    .state
                    .camera_config_path
                    .as_deref()
                    .map(config::load_camera_defaults)
                    .transpose()
                    .map_err(|error| {
                        ControlCommandError::new(
                            proto::ErrorCode::Internal,
                            500,
                            format!("unable to load camera credential defaults: {error}"),
                        )
                    })?
                    .unwrap_or_default();
                let mut networks =
                    crate::cameras::camera_discovery_networks().unwrap_or_else(|error| {
                        tracing::debug!(%error, "camera discovery networks are unavailable");
                        Vec::new()
                    });
                let configured_networks = self
                    .state
                    .camera_entries()
                    .into_iter()
                    .filter_map(|camera| camera.info.ip.parse::<Ipv4Addr>().ok())
                    .filter(Ipv4Addr::is_private)
                    .filter_map(|ip| {
                        ipnet::Ipv4Net::new(ip, 24)
                            .ok()
                            .map(|network| network.trunc())
                    })
                    .collect::<HashSet<_>>();
                if !configured_networks.is_empty() {
                    for network in &mut networks {
                        network.preferred = false;
                    }
                    for network in configured_networks {
                        let cidr = network.to_string();
                        if let Some(existing) = networks.iter_mut().find(|item| item.cidr == cidr) {
                            existing.preferred = true;
                        } else {
                            networks.push(crate::cameras::CameraDiscoveryNetwork {
                                cidr,
                                interface_name: "configured cameras".to_owned(),
                                preferred: true,
                            });
                        }
                    }
                    networks.sort_by(|left, right| {
                        right
                            .preferred
                            .cmp(&left.preferred)
                            .then_with(|| left.cidr.cmp(&right.cidr))
                    });
                }
                let networks = networks
                    .into_iter()
                    .map(|network| proto::CameraDiscoveryNetwork {
                        cidr: network.cidr,
                        interface_name: network.interface_name,
                        preferred: network.preferred,
                    })
                    .collect();
                Ok(control_ok::Result::CameraOnboardingDefaults(
                    proto::CameraOnboardingDefaults {
                        username_configured: !defaults.username.is_empty(),
                        password_configured: !defaults.password.is_empty(),
                        networks,
                    },
                ))
            }
            Some(camera_configuration_command::Action::SearchCatalog(request)) => {
                let query = request.query.trim();
                if query.is_empty() || query.chars().count() > MAX_CAMERA_CATALOG_QUERY_CHARS {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "camera catalog search query must be between 1 and 128 characters",
                    ));
                }
                let ip = request
                    .ip
                    .as_deref()
                    .map(|value| {
                        value.parse::<IpAddr>().map_err(|_| {
                            ControlCommandError::new(
                                proto::ErrorCode::InvalidRequest,
                                400,
                                "camera catalog stream hints require a valid IP address",
                            )
                        })
                    })
                    .transpose()?;
                let limit = request
                    .limit
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(DEFAULT_CAMERA_CATALOG_SEARCH_LIMIT);
                let database = self.camera_database()?;
                let cameras = database
                    .search(query, limit)
                    .into_iter()
                    .map(|camera| {
                        let stream_hints = ip.and_then(|ip| database.stream_hints(&camera.id, ip));
                        proto_camera_catalog_camera(camera, stream_hints)
                    })
                    .collect();
                Ok(control_ok::Result::CameraCatalogSearchResult(
                    proto::CameraCatalogSearchResult { cameras },
                ))
            }
            Some(camera_configuration_command::Action::Update(request)) => {
                let camera_id = request.ip.clone();
                let update = camera_settings_update_from_proto(request)?;
                let result =
                    save_camera_settings(update, &self.router_tx, &self.state, &camera_id)?;
                Ok(control_ok::Result::CameraConfigurationResult(
                    proto::CameraConfigurationResult {
                        camera: Some(proto_camera_settings(result.camera)),
                        restart_required: result.restart_required,
                        removed: false,
                        cameras: Vec::new(),
                    },
                ))
            }
            Some(camera_configuration_command::Action::Remove(request)) => {
                delete_camera_settings(&self.state, &request.ip)?;
                Ok(control_ok::Result::CameraConfigurationResult(
                    proto::CameraConfigurationResult {
                        camera: None,
                        restart_required: false,
                        removed: true,
                        cameras: Vec::new(),
                    },
                ))
            }
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "camera configuration command has no action",
            )),
        }
    }

    fn handle_camera_stream_probe(
        &self,
        request: proto::ProbeCameraStreams,
    ) -> Result<control_ok::Result, ControlCommandError> {
        let ip = request.ip.parse::<IpAddr>().map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "camera stream probe requires a valid IP address",
            )
        })?;
        let onvif_port = request
            .onvif_port
            .map(|port| {
                let port = u16::try_from(port).map_err(|_| {
                    ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "ONVIF port must be between 1 and 65535",
                    )
                })?;
                if port == 0 {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "ONVIF port must be between 1 and 65535",
                    ));
                }
                Ok(port)
            })
            .transpose()?;
        let resolve = |field: &str, value: String| {
            if let Some(config_path) = &self.state.camera_config_path {
                resolve_setting_secret(config_path, field, &value)
            } else if config::contains_secret_reference(&value) {
                Err(ControlCommandError::new(
                    proto::ErrorCode::Unavailable,
                    409,
                    format!("{field} secret references require configuration persistence"),
                ))
            } else {
                Ok(value)
            }
        };
        let defaults = self
            .state
            .camera_config_path
            .as_deref()
            .map(config::load_camera_defaults)
            .transpose()
            .map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::Internal,
                    500,
                    format!("unable to load camera credential defaults: {error}"),
                )
            })?
            .unwrap_or_default();
        let username = if request.username.trim().is_empty() {
            defaults.username
        } else {
            resolve("username", request.username.trim().to_owned())?
        };
        let password = if request.password.is_empty() {
            defaults.password
        } else {
            resolve("password", request.password)?
        };
        if username.is_empty() || password.is_empty() {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "camera stream probe requires an effective username and password",
            ));
        }
        let requested_main_rtsp_url = request
            .main_rtsp_url
            .map(|value| resolve("main RTSP URL", value))
            .transpose()?
            .map(Some)
            .map(normalize_rtsp_url)
            .transpose()
            .map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    format!("main RTSP URL {error}"),
                )
            })?
            .flatten();
        let requested_sub_rtsp_url = request
            .sub_rtsp_url
            .map(|value| resolve("sub RTSP URL", value))
            .transpose()?
            .map(Some)
            .map(normalize_rtsp_url)
            .transpose()
            .map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    format!("sub RTSP URL {error}"),
                )
            })?
            .flatten();
        let transport = optional_camera_transport(request.transport)?.unwrap_or_default();
        let rtsp_transport = match transport {
            CameraTransport::Tcp => RtspTransport::Tcp,
            CameraTransport::Udp => RtspTransport::Udp,
        };
        let config = CameraConfig {
            ip,
            name: None,
            display_name: None,
            manufacturer: None,
            username: username.clone(),
            password: password.clone(),
            onvif_port,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Retina,
            transport,
            record_generic_motion_events: false,
            recording_mode: Default::default(),
            event_recording_duration_secs: 60,
        };
        let (camera, onvif_error) = if request.query_onvif.unwrap_or(true) {
            match probe_onvif_camera(&config) {
                Ok(camera) => (Some(camera), None),
                Err(error) => {
                    tracing::debug!(%error, %ip, "candidate ONVIF stream probe failed");
                    if requested_main_rtsp_url.is_none() && requested_sub_rtsp_url.is_none() {
                        return Err(ControlCommandError::new(
                            proto::ErrorCode::Unavailable,
                            502,
                            "ONVIF could not retrieve stream endpoints for these credentials",
                        ));
                    }
                    (
                        None,
                        Some(
                            "ONVIF did not respond; supplied RTSP endpoints were verified directly."
                                .to_owned(),
                        ),
                    )
                }
            }
        } else {
            (None, None)
        };
        let main_rtsp_url = requested_main_rtsp_url.or_else(|| {
            camera
                .as_ref()
                .and_then(|camera| camera.main_rtsp_url.clone())
        });
        let sub_rtsp_url = requested_sub_rtsp_url.or_else(|| {
            camera
                .as_ref()
                .and_then(|camera| camera.sub_rtsp_url.clone())
        });
        let stream_requests = [
            ("main", main_rtsp_url.as_deref()),
            ("sub", sub_rtsp_url.as_deref()),
        ];
        let username = username.as_str();
        let password = password.as_str();
        let streams = std::thread::scope(|scope| {
            let handles = stream_requests.map(|(stream, rtsp_url)| {
                scope.spawn(move || {
                    proto_camera_stream_verification(
                        stream,
                        rtsp_url,
                        username,
                        password,
                        rtsp_transport,
                    )
                })
            });
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .expect("camera stream verification worker must not panic")
                })
                .collect::<Vec<_>>()
        });
        let profiles = camera
            .as_ref()
            .map(|camera| {
                camera
                    .profiles
                    .iter()
                    .enumerate()
                    .map(|(index, profile)| {
                        proto_health_profile(ProfileSummary {
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
                            bitrate_kbps: profile
                                .video
                                .as_ref()
                                .and_then(|video| video.bitrate_kbps),
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
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(control_ok::Result::CameraStreamProbeResult(
            proto::CameraStreamProbeResult {
                main_rtsp_url,
                sub_rtsp_url,
                onvif_port: camera.as_ref().map(|camera| u32::from(camera.onvif_port)),
                manufacturer: camera
                    .as_ref()
                    .and_then(|camera| camera.device.manufacturer.clone()),
                model: camera
                    .as_ref()
                    .and_then(|camera| camera.device.model.clone()),
                firmware_version: camera
                    .as_ref()
                    .and_then(|camera| camera.device.firmware_version.clone()),
                serial_number: camera
                    .as_ref()
                    .and_then(|camera| camera.device.serial_number.clone()),
                hardware_id: camera
                    .as_ref()
                    .and_then(|camera| camera.device.hardware_id.clone()),
                profiles,
                streams,
                onvif_error,
            },
        ))
    }

    fn handle_server(
        &self,
        session_id: SessionId,
        command: proto::ServerCommand,
    ) -> Result<(control_ok::Result, PostSendAction), ControlCommandError> {
        match command.action {
            Some(server_command::Action::Restart(_)) => {
                let Some(control) = self.state.restart_control.clone() else {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::Unavailable,
                        409,
                        "server restart is unavailable",
                    ));
                };
                let action = Box::new(move || {
                    control.restart.request();
                    control.shutdown.cancel();
                });
                Ok((
                    control_ok::Result::RestartResult(proto::RestartResult { restarting: true }),
                    action,
                ))
            }
            Some(server_command::Action::GetAccessKey(_)) => {
                self.require_loopback_administrator(session_id)?;
                let access_key = *self
                    .state
                    .access_key
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if access_key.is_unset() {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::Unavailable,
                        409,
                        "remote access key is not configured",
                    ));
                }
                Ok((
                    control_ok::Result::AccessKeyResult(proto::AccessKeyResult {
                        access_key: access_key.canonical(),
                        rotated: false,
                    }),
                    Box::new(|| {}),
                ))
            }
            Some(server_command::Action::RotateAccessKey(_)) => {
                self.require_loopback_administrator(session_id)?;
                let Some(config_path) = &self.state.camera_config_path else {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::Unavailable,
                        409,
                        "remote access key rotation requires a persisted configuration",
                    ));
                };
                let _update = self
                    .state
                    .config_update
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let access_key =
                    config::rotate_access_key_secret(config_path).map_err(|error| {
                        ControlCommandError::new(
                            proto::ErrorCode::Unavailable,
                            409,
                            format!("remote access key could not be rotated: {error}"),
                        )
                    })?;
                *self
                    .state
                    .access_key
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = access_key;
                let remote_sessions = {
                    let mut owners = self
                        .state
                        .api_session_owners
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let sessions = owners
                        .iter()
                        .filter_map(|(session_id, principal)| {
                            matches!(principal, ApiPrincipal::AccessKey(_)).then_some(*session_id)
                        })
                        .collect::<Vec<_>>();
                    owners.retain(|_, principal| !matches!(principal, ApiPrincipal::AccessKey(_)));
                    sessions
                };
                let webrtc = self.state.webrtc.clone();
                tracing::info!(
                    revoked_sessions = remote_sessions.len(),
                    "remote access key rotated by loopback administrator"
                );
                Ok((
                    control_ok::Result::AccessKeyResult(proto::AccessKeyResult {
                        access_key: access_key.canonical(),
                        rotated: true,
                    }),
                    Box::new(move || {
                        for session_id in remote_sessions {
                            webrtc.close_api_session(session_id);
                        }
                    }),
                ))
            }
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "server command has no action",
            )),
        }
    }

    fn require_loopback_administrator(
        &self,
        session_id: SessionId,
    ) -> Result<(), ControlCommandError> {
        let owners = self
            .state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            owners.get(&session_id),
            Some(ApiPrincipal::LoopbackAdministrator)
        ) {
            return Ok(());
        }
        Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "remote access key material is available only to a loopback administrator session",
        ))
    }

    fn handle_runtime_configuration(
        &self,
        command: proto::RuntimeConfigurationCommand,
    ) -> Result<control_ok::Result, ControlCommandError> {
        match command.action {
            Some(runtime_configuration_command::Action::Get(_)) => {
                Ok(control_ok::Result::RuntimeConfigurationResult(
                    proto_runtime_configuration_result(RuntimeSettingsUpdateResponse {
                        config: current_config(&self.state),
                        restart_required: false,
                    }),
                ))
            }
            Some(runtime_configuration_command::Action::Update(update)) => {
                let port = u16::try_from(update.port).map_err(|_| {
                    ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "server port must be between 1 and 65535",
                    )
                })?;
                let Some(storage) = update.storage else {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "runtime configuration requires storage settings",
                    ));
                };
                let write_buffer_bytes =
                    usize::try_from(storage.write_buffer_bytes).map_err(|_| {
                        ControlCommandError::new(
                            proto::ErrorCode::InvalidRequest,
                            400,
                            "write buffer size is too large",
                        )
                    })?;
                let result = save_runtime_settings(
                    RuntimeSettingsUpdate {
                        host: update.host,
                        port,
                        storage: RuntimeStorageSettingsUpdate {
                            medium_term_path: storage.medium_term_path,
                            long_term_path: storage.long_term_path,
                            recording_catalog_path: storage.recording_catalog_path,
                            event_thumbnail_path: storage.event_thumbnail_path,
                            event_thumbnail_max_mb: storage.event_thumbnail_max_mb,
                            short_term_secs: storage.short_term_secs,
                            medium_term_secs: storage.medium_term_secs,
                            flush_interval_secs: storage.flush_interval_secs,
                            write_buffer_bytes,
                            long_term_max_gb: storage.long_term_max_gb,
                        },
                        move_existing_recordings: update.move_existing_recordings,
                    },
                    &self.state,
                )?;
                Ok(control_ok::Result::RuntimeConfigurationResult(
                    proto_runtime_configuration_result(result),
                ))
            }
            Some(runtime_configuration_command::Action::ProbeStorage(request)) => {
                let path = if let Some(config_path) = &self.state.camera_config_path {
                    config::resolve_secret_references(config_path, &request.path).map_err(
                        |error| {
                            ControlCommandError::new(
                                proto::ErrorCode::InvalidRequest,
                                400,
                                format!("storage path secret reference is invalid: {error}"),
                            )
                        },
                    )?
                } else {
                    request.path
                };
                let path = normalize_storage_path(&path).ok_or_else(|| {
                    ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "storage path must be nonempty and cannot contain NUL",
                    )
                })?;
                let result = storage_write_probe(Path::new(&path));
                Ok(control_ok::Result::StorageWriteProbeResult(
                    proto::StorageWriteProbeResult {
                        writable: result.is_ok(),
                        detail: match result {
                            Ok(()) => "Write, flush, rename, and cleanup succeeded.".to_owned(),
                            Err(error) => format!("Storage write verification failed: {error}"),
                        },
                    },
                ))
            }
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "runtime configuration command has no action",
            )),
        }
    }

    fn handle_health(
        &self,
        command: proto::HealthCommand,
    ) -> Result<control_ok::Result, ControlCommandError> {
        match command.action {
            Some(health_command::Action::Get(_)) => Ok(control_ok::Result::HealthResult(
                proto_health_snapshot(server_health(&self.router_tx, &self.state)),
            )),
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "health command has no action",
            )),
        }
    }

    fn handle_export(
        &self,
        command: proto::ExportCommand,
    ) -> Result<(control_ok::Result, Vec<OutboundDataMessage>), ControlCommandError> {
        cleanup_expired_exports(&self.state);
        match command.action {
            Some(proto::export_command::Action::Create(request)) => {
                let job = create_export_job(&self.state, request)?;
                Ok((control_ok::Result::ExportJob(job), Vec::new()))
            }
            Some(proto::export_command::Action::List(_)) => {
                let mut jobs = self
                    .state
                    .export_jobs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .values()
                    .map(|record| record.job.clone())
                    .collect::<Vec<_>>();
                jobs.sort_unstable_by(|left, right| {
                    export_requested_start(right).cmp(&export_requested_start(left))
                });
                Ok((
                    control_ok::Result::ExportJobs(proto::ExportJobList { jobs }),
                    Vec::new(),
                ))
            }
            Some(proto::export_command::Action::Get(request)) => {
                let job = export_job(&self.state, &request.job_id)?;
                Ok((control_ok::Result::ExportJob(job), Vec::new()))
            }
            Some(proto::export_command::Action::Cancel(request)) => {
                let job = cancel_export_job(&self.state, &request.job_id)?;
                Ok((control_ok::Result::ExportJob(job), Vec::new()))
            }
            Some(proto::export_command::Action::Retry(request)) => {
                let job = retry_export_job(&self.state, &request.job_id)?;
                Ok((control_ok::Result::ExportJob(job), Vec::new()))
            }
            Some(proto::export_command::Action::Download(request)) => {
                let (result, messages) = download_export(&self.state, request)?;
                Ok((control_ok::Result::ExportDownload(result), messages))
            }
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "export command has no action",
            )),
        }
    }

    fn handle_event_search(
        &self,
        session_id: SessionId,
        command: proto::EventSearchCommand,
    ) -> Result<(Option<control_ok::Result>, Vec<OutboundDataMessage>), ControlCommandError> {
        match command.action {
            Some(event_search_command::Action::ReplaceTerms(request)) => {
                replace_event_search_terms(&self.state, &request)?;
                Ok((
                    Some(control_ok::Result::EventSearchMutation(
                        proto::EventSearchMutationResult {
                            event_id: request.event_id,
                        },
                    )),
                    Vec::new(),
                ))
            }
            Some(event_search_command::Action::SetEmbedding(request)) => {
                set_event_search_embedding(&self.state, &request)?;
                Ok((
                    Some(control_ok::Result::EventSearchMutation(
                        proto::EventSearchMutationResult {
                            event_id: request.event_id,
                        },
                    )),
                    Vec::new(),
                ))
            }
            Some(event_search_command::Action::Query(request)) => {
                if self.state.webrtc.has_api_session(session_id) {
                    let delivery = start_event_search_query(&self.state, session_id, request)?;
                    return Ok((
                        Some(control_ok::Result::EventSearchDelivery(delivery)),
                        Vec::new(),
                    ));
                }
                let (delivery, messages) = query_events(&self.state, request)?;
                Ok((
                    Some(control_ok::Result::EventSearchDelivery(delivery)),
                    messages,
                ))
            }
            Some(event_search_command::Action::FetchMedia(request)) => {
                if self.state.webrtc.has_api_session(session_id) {
                    let delivery = start_event_search_media(&self.state, session_id, request)?;
                    return Ok((
                        Some(control_ok::Result::EventSearchMediaDelivery(delivery)),
                        Vec::new(),
                    ));
                }
                let (delivery, messages) = fetch_event_search_media(&self.state, request)?;
                Ok((
                    Some(control_ok::Result::EventSearchMediaDelivery(delivery)),
                    messages,
                ))
            }
            Some(event_search_command::Action::CancelQuery(request)) => {
                validate_client_id(&request.query_id, "event search query ID")?;
                cancel_event_search_task(
                    &self.state,
                    session_id,
                    &format!("event-search-query:{}", request.query_id),
                );
                Ok((None, Vec::new()))
            }
            Some(event_search_command::Action::CancelMedia(request)) => {
                validate_client_id(&request.transfer_id, "event search transfer ID")?;
                cancel_event_search_task(
                    &self.state,
                    session_id,
                    &format!("event-search-media:{}", request.transfer_id),
                );
                Ok((None, Vec::new()))
            }
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event search command has no action",
            )),
        }
    }

    fn handle_stored_media(
        &self,
        session_id: SessionId,
        command: proto::StoredMediaCommand,
    ) -> Result<StoredMediaDispatch, ControlCommandError> {
        match command.action {
            Some(stored_media_command::Action::Open(open)) => {
                let (cursor, state, messages) = open_stored_media(&self.state, open)?;
                let key = (session_id, state.stored_media_id.clone());
                let mut cursors = self
                    .state
                    .stored_media_cursors
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if cursors.contains_key(&key) {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::Rejected,
                        409,
                        "stored media cursor ID is already active on this connection",
                    ));
                }
                cursors.insert(key, cursor);
                let notifications = terminal_stored_media_notification(&state);
                Ok(StoredMediaDispatch {
                    result: Some(control_ok::Result::StoredMediaState(state)),
                    messages,
                    notifications,
                })
            }
            Some(stored_media_command::Action::Seek(seek)) => {
                let (state, messages) = seek_stored_media(&self.state, session_id, seek)?;
                let notifications = terminal_stored_media_notification(&state);
                Ok(StoredMediaDispatch {
                    result: Some(control_ok::Result::StoredMediaState(state)),
                    messages,
                    notifications,
                })
            }
            Some(stored_media_command::Action::Refill(refill)) => {
                let (state, messages) = refill_stored_media(&self.state, session_id, refill)?;
                let notifications = terminal_stored_media_notification(&state);
                Ok(StoredMediaDispatch {
                    result: Some(control_ok::Result::StoredMediaState(state)),
                    messages,
                    notifications,
                })
            }
            Some(stored_media_command::Action::SetPlayback(update)) => {
                let (state, messages) = set_stored_media_playback(&self.state, session_id, update)?;
                Ok(StoredMediaDispatch {
                    result: Some(control_ok::Result::StoredMediaState(state)),
                    messages,
                    notifications: Vec::new(),
                })
            }
            Some(stored_media_command::Action::Close(close)) => {
                let removed = self
                    .state
                    .stored_media_cursors
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&(session_id, close.stored_media_id));
                if removed.is_none() {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::NotFound,
                        404,
                        "stored media cursor was not found",
                    ));
                }
                Ok(StoredMediaDispatch {
                    result: None,
                    messages: Vec::new(),
                    notifications: Vec::new(),
                })
            }
            Some(stored_media_command::Action::QueryTimeline(query)) => {
                let (delivery, messages) = query_stored_media_timeline(&self.state, query)?;
                Ok(StoredMediaDispatch {
                    result: Some(control_ok::Result::StoredMediaQueryDelivery(delivery)),
                    messages,
                    notifications: Vec::new(),
                })
            }
            Some(stored_media_command::Action::CancelTimelineQuery(_)) => Ok(StoredMediaDispatch {
                result: None,
                messages: Vec::new(),
                notifications: Vec::new(),
            }),
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "stored media command has no action",
            )),
        }
    }
}

fn storage_write_probe(path: &Path) -> anyhow::Result<()> {
    use std::io::Write as _;

    std::fs::create_dir_all(path)?;
    let nonce = rand::random::<u64>();
    let pending = path.join(format!(".keeppeek-write-probe-{nonce}.pending"));
    let renamed = path.join(format!(".keeppeek-write-probe-{nonce}.complete"));
    let cleanup = || {
        let _ = std::fs::remove_file(&pending);
        let _ = std::fs::remove_file(&renamed);
    };
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending)?;
        file.write_all(b"keeppeek storage write probe\n")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&pending, &renamed)?;
        std::fs::remove_file(&renamed)?;
        Ok(())
    })();
    if result.is_err() {
        cleanup();
    }
    result
}

#[derive(Clone)]
struct StoredTimelineRange {
    source_id: String,
    stream_id: String,
    start_ms: i64,
    end_ms: i64,
}

struct StoredTimelineEvent {
    event: proto::Event,
    attachment: Option<StoredTimelineAttachment>,
}

struct StoredTimelineAttachment {
    event_id: String,
    timestamp_ms: i64,
    path: PathBuf,
}

struct StoredMediaBatch {
    content_type: String,
    fragment_time_ms: i64,
    delivered_through_ms: i64,
    messages: Vec<OutboundDataMessage>,
}

#[derive(PartialEq, Eq)]
struct StoredMediaPeriodKey {
    recording_id: String,
    sample_descriptions: Vec<u32>,
}

struct StoredMediaPeriod {
    sample_descriptions: Vec<u32>,
    initialization: Vec<u8>,
    fragment: Vec<u8>,
    content_type: String,
}

struct StoredMediaDispatch {
    result: Option<control_ok::Result>,
    messages: Vec<OutboundDataMessage>,
    notifications: Vec<proto::Notification>,
}

struct StoredMediaBatchRequest<'a> {
    stored_media_id: &'a str,
    source_id: &'a str,
    stream_id: &'a str,
    recording_stream_id: &'a str,
    requested_time_ms: i64,
    end_time_ms: Option<i64>,
    mode: proto::StoredMediaMode,
    media_target: DataChannelTarget,
    max_buffer_ms: u64,
    generation: u64,
}

fn create_export_job(
    state: &ServerState,
    request: proto::CreateExportJob,
) -> Result<proto::ExportJob, ControlCommandError> {
    validate_client_id(&request.job_id, "export job ID")?;
    let camera = state.camera(&request.source_id).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "export source was not found",
        )
    })?;
    if !matches!(request.stream_id.as_str(), "main" | "sub")
        || !camera
            .info
            .profiles
            .iter()
            .any(|profile| profile.stream == request.stream_id)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "export stream was not found",
        ));
    }
    let start_ms = required_timestamp_ms(request.start_time.as_ref(), "export start time")?;
    let end_ms = required_timestamp_ms(request.end_time.as_ref(), "export end time")?;
    if end_ms <= start_ms {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export end time must follow its start time",
        ));
    }
    if end_ms.saturating_sub(start_ms)
        > i64::try_from(MAX_EXPORT_DURATION.as_millis()).unwrap_or(i64::MAX)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export range exceeds 2 minutes",
        ));
    }
    {
        let jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if jobs.contains_key(&request.job_id) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "export job ID already exists",
            ));
        }
    }
    let recording_stream_id = recording_stream_id(&camera, &request.stream_id);
    let catalog = state.catalog.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        )
    })?;
    let fragments = catalog
        .media_fragments_in_range(&recording_stream_id, start_ms, end_ms)
        .map_err(|error| stored_catalog_error("query export fragments", error))?;
    let missing_ranges = export_missing_ranges(&fragments, start_ms, end_ms);
    let estimated_bytes = export_estimated_bytes(&fragments);
    let aligned_start_ms = fragments.first().map(|fragment| fragment.start_ms);
    let file_name = export_file_name(
        camera
            .info
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&camera.recording_label),
        start_ms,
        end_ms,
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let status = if request.burn_in_timestamp {
        proto::ExportJobStatus::Failed
    } else if fragments.is_empty() || (!missing_ranges.is_empty() && !request.allow_partial) {
        proto::ExportJobStatus::Partial
    } else {
        proto::ExportJobStatus::Running
    };
    let error = request
        .burn_in_timestamp
        .then(|| "Timestamp burn-in requires a configured re-encoding worker".to_owned());
    let job = proto::ExportJob {
        job_id: request.job_id.clone(),
        source_id: request.source_id.clone(),
        stream_id: request.stream_id.clone(),
        requested_start_time: Some(millis_timestamp(start_ms)),
        requested_end_time: Some(millis_timestamp(end_ms)),
        aligned_start_time: aligned_start_ms.map(millis_timestamp),
        status: status as i32,
        progress_per_mille: 0,
        bytes_written: 0,
        estimated_bytes: Some(estimated_bytes),
        file_name: None,
        sha256: None,
        expires_at: None,
        missing_ranges: missing_ranges
            .iter()
            .map(|(start, end)| proto::ExportMissingRange {
                start_time: Some(millis_timestamp(*start)),
                end_time: Some(millis_timestamp(*end)),
            })
            .collect(),
        error,
        retryable: false,
        burn_in_timestamp: request.burn_in_timestamp,
    };
    state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            request.job_id.clone(),
            ExportJobRecord {
                request: request.clone(),
                job: job.clone(),
                path: None,
                cancel: cancel.clone(),
            },
        );
    if status == proto::ExportJobStatus::Running {
        spawn_export_worker(state.clone(), request, fragments, cancel, end_ms, file_name);
    }
    Ok(job)
}

fn spawn_export_worker(
    state: ServerState,
    request: proto::CreateExportJob,
    fragments: Vec<CatalogMediaFragment>,
    cancel: Arc<AtomicBool>,
    end_ms: i64,
    file_name: String,
) {
    std::thread::spawn(move || {
        let directory = state.storage_config.long_term_path.join(".exports");
        let path = directory.join(&request.job_id).join(&file_name);
        let result =
            crate::storage::playback::export_fragment_ranges(&fragments, end_ms, &path, || {
                cancel.load(Ordering::Acquire)
            })
            .and_then(|artifact| {
                let checksum = sha256_file(&path)?;
                Ok((artifact, checksum))
            });
        let mut jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(record) = jobs.get_mut(&request.job_id) else {
            let _ = std::fs::remove_file(path);
            return;
        };
        if !Arc::ptr_eq(&record.cancel, &cancel) {
            let _ = std::fs::remove_file(path);
            return;
        }
        if cancel.load(Ordering::Acquire) {
            let _ = std::fs::remove_file(path);
            record.job.status = proto::ExportJobStatus::Cancelled as i32;
            record.job.error = Some("Export was cancelled".to_owned());
            record.job.retryable = true;
            return;
        }
        match result {
            Ok((artifact, checksum)) => {
                let expires_ms = unix_time_ms().saturating_add(
                    u64::try_from(EXPORT_JOB_EXPIRY.as_millis()).unwrap_or(u64::MAX),
                );
                record.job.status = proto::ExportJobStatus::Ready as i32;
                record.job.progress_per_mille = 1_000;
                record.job.bytes_written = artifact.bytes;
                record.job.aligned_start_time = Some(millis_timestamp(artifact.aligned_start_ms));
                record.job.file_name = Some(file_name);
                record.job.sha256 = Some(checksum);
                record.job.expires_at = Some(millis_timestamp(
                    i64::try_from(expires_ms).unwrap_or(i64::MAX),
                ));
                record.job.error = None;
                record.job.retryable = false;
                record.path = Some(path);
            }
            Err(error) => {
                let _ = std::fs::remove_file(path);
                record.job.status = proto::ExportJobStatus::Failed as i32;
                record.job.error = Some(error.to_string());
                record.job.retryable = true;
            }
        }
    });
}

fn export_job(state: &ServerState, job_id: &str) -> Result<proto::ExportJob, ControlCommandError> {
    state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(job_id)
        .map(|record| record.job.clone())
        .ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::NotFound, 404, "export job was not found")
        })
}

fn cancel_export_job(
    state: &ServerState,
    job_id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let record = jobs.get_mut(job_id).ok_or_else(|| {
        ControlCommandError::new(proto::ErrorCode::NotFound, 404, "export job was not found")
    })?;
    if record.job.status != proto::ExportJobStatus::Running as i32 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "only a running export can be cancelled",
        ));
    }
    record.cancel.store(true, Ordering::Release);
    record.job.status = proto::ExportJobStatus::Cancelled as i32;
    record.job.error = Some("Export was cancelled".to_owned());
    record.job.retryable = true;
    Ok(record.job.clone())
}

fn retry_export_job(
    state: &ServerState,
    job_id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    let request = {
        let mut jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = jobs.get(job_id).ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::NotFound, 404, "export job was not found")
        })?;
        if !matches!(
            proto::ExportJobStatus::try_from(record.job.status),
            Ok(proto::ExportJobStatus::Failed | proto::ExportJobStatus::Cancelled)
        ) || !record.job.retryable
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "export job is not retryable",
            ));
        }
        record.cancel.store(true, Ordering::Release);
        let request = record.request.clone();
        if let Some(path) = &record.path {
            let _ = std::fs::remove_file(path);
        }
        jobs.remove(job_id);
        request
    };
    create_export_job(state, request)
}

fn download_export(
    state: &ServerState,
    request: proto::DownloadExport,
) -> Result<(proto::ExportDownloadResult, Vec<OutboundDataMessage>), ControlCommandError> {
    let (target, channel) = data_channel_target(request.channel)?;
    if target != DataChannelTarget::Reliable {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export downloads require reliable-data",
        ));
    }
    let (job, path) = {
        let jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = jobs.get(&request.job_id).ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::NotFound, 404, "export job was not found")
        })?;
        if record.job.status != proto::ExportJobStatus::Ready as i32 {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "export file is not ready",
            ));
        }
        let path = record.path.clone().ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::Internal, 500, "ready export has no file")
        })?;
        (record.job.clone(), path)
    };
    let size = path
        .metadata()
        .map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                format!("export file is unavailable: {error}"),
            )
        })?
        .len();
    if size > MAX_EXPORT_DOWNLOAD_BYTES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            413,
            "export file exceeds the browser download limit",
        ));
    }
    let payload = std::fs::read(&path).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to read export file: {error}"),
        )
    })?;
    let chunk_count = payload.len().div_ceil(DATA_MESSAGE_CHUNK_BYTES);
    let chunk_count_u32 = u32::try_from(chunk_count).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "export has too many chunks",
        )
    })?;
    let messages = payload
        .chunks(DATA_MESSAGE_CHUNK_BYTES)
        .enumerate()
        .map(|(chunk_index, chunk)| OutboundDataMessage {
            target,
            group: format!("export:{}", request.job_id),
            message: proto::Message {
                message: Some(proto::message::Message::Export(proto::ExportMessage {
                    message: Some(proto::export_message::Message::FileChunk(
                        proto::ExportFileChunk {
                            job_id: request.job_id.clone(),
                            chunk_index: u32::try_from(chunk_index).unwrap_or(u32::MAX),
                            chunk_count: chunk_count_u32,
                            payload: chunk.to_vec(),
                        },
                    )),
                })),
            },
        })
        .collect();
    Ok((
        proto::ExportDownloadResult {
            job: Some(job),
            channel: channel as i32,
            chunk_count: chunk_count_u32,
        },
        messages,
    ))
}

fn cleanup_expired_exports(state: &ServerState) {
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let mut paths = Vec::new();
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for record in jobs.values_mut() {
        if record.job.status == proto::ExportJobStatus::Ready as i32
            && record
                .job
                .expires_at
                .as_ref()
                .and_then(timestamp_ms)
                .is_some_and(|expires| expires <= now_ms)
        {
            record.job.status = proto::ExportJobStatus::Expired as i32;
            record.job.retryable = true;
            if let Some(path) = record.path.take() {
                paths.push(path);
            }
        }
    }
    drop(jobs);
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn export_missing_ranges(
    fragments: &[CatalogMediaFragment],
    start_ms: i64,
    end_ms: i64,
) -> Vec<(i64, i64)> {
    let mut cursor = start_ms;
    let mut missing = Vec::new();
    for fragment in fragments {
        let start = fragment.start_ms.max(start_ms);
        let end = fragment
            .start_ms
            .saturating_add(i64::try_from(fragment.duration_ms).unwrap_or(i64::MAX))
            .min(end_ms);
        if end <= cursor {
            continue;
        }
        if start > cursor {
            missing.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < end_ms {
        missing.push((cursor, end_ms));
    }
    missing
}

fn export_estimated_bytes(fragments: &[CatalogMediaFragment]) -> u64 {
    let mut initialization = HashSet::new();
    fragments.iter().fold(0u64, |total, fragment| {
        let init = if initialization.insert(fragment.recording_id.as_str()) {
            fragment.init_len
        } else {
            0
        };
        total.saturating_add(init).saturating_add(fragment.byte_len)
    })
}

fn export_file_name(camera_name: &str, start_ms: i64, end_ms: i64) -> String {
    let camera_name = export_file_component(camera_name);
    format!(
        "{camera_name}_{}_to_{}.mp4",
        export_file_timestamp(start_ms),
        export_file_timestamp(end_ms),
    )
}

fn export_file_component(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(64));
    let mut separator = false;
    for character in value.trim().chars() {
        if character.is_alphanumeric() {
            if output.len().saturating_add(character.len_utf8()) > 64 {
                break;
            }
            output.push(character);
            separator = false;
        } else if !output.is_empty() && !separator && output.len() < 64 {
            output.push('-');
            separator = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        "camera".to_owned()
    } else {
        output
    }
}

fn export_file_timestamp(timestamp_ms: i64) -> String {
    let timestamp =
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp_ms) * 1_000_000)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}-{:03}Z",
        timestamp.year(),
        timestamp.month() as u8,
        timestamp.day(),
        timestamp.hour(),
        timestamp.minute(),
        timestamp.second(),
        timestamp.millisecond(),
    )
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1_024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(encode_lower_hex(hasher.finalize()))
}

fn encode_lower_hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn export_requested_start(job: &proto::ExportJob) -> i64 {
    job.requested_start_time
        .as_ref()
        .and_then(timestamp_ms)
        .unwrap_or(0)
}

fn open_stored_media(
    state: &ServerState,
    open: proto::OpenStoredMedia,
) -> Result<
    (
        StoredMediaCursor,
        proto::StoredMediaState,
        Vec<OutboundDataMessage>,
    ),
    ControlCommandError,
> {
    validate_client_id(&open.stored_media_id, "stored media cursor ID")?;
    let camera = state.camera(&open.source_id).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "stored media source was not found",
        )
    })?;
    if !matches!(open.stream_id.as_str(), "main" | "sub")
        || !camera
            .info
            .profiles
            .iter()
            .any(|profile| profile.stream == open.stream_id)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "stored media stream was not found",
        ));
    }
    if !open.data_payload_routes.is_empty() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::UnsupportedRequest,
            501,
            "stored timed-data playback is unavailable",
        ));
    }
    let requested_time_ms = required_timestamp_ms(open.timestamp.as_ref(), "stored media time")?;
    let end_time_ms = open
        .end_time
        .as_ref()
        .map(|timestamp| required_timestamp_ms(Some(timestamp), "stored media end time"))
        .transpose()?;
    if end_time_ms.is_some_and(|end_time_ms| end_time_ms <= requested_time_ms) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media end time must follow its initial time",
        ));
    }
    if !open.playback_rate.is_finite() || open.playback_rate <= 0.0 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media playback rate must be finite and positive",
        ));
    }
    let mode = proto::StoredMediaMode::try_from(open.mode).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media mode is invalid",
        )
    })?;
    if mode == proto::StoredMediaMode::Unspecified {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media mode is required",
        ));
    }
    if mode == proto::StoredMediaMode::Scrub && open.playing {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media scrub mode must be paused",
        ));
    }
    let (media_target, media_channel) = data_channel_target(open.media_channel)?;
    let requested_buffer_ms = optional_duration_ms(open.max_buffer_duration.as_ref())?;
    let max_buffer_ms = if requested_buffer_ms == 0 {
        u64::try_from(DEFAULT_STORED_MEDIA_BUFFER.as_millis()).unwrap_or(u64::MAX)
    } else {
        requested_buffer_ms
    };
    if max_buffer_ms > u64::try_from(MAX_STORED_MEDIA_BUFFER.as_millis()).unwrap_or(u64::MAX) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media buffer duration exceeds 300 seconds",
        ));
    }
    let stored_stream_id = recording_stream(&camera, &open.stream_id).to_owned();
    let recording_stream_id = recording_stream_id(&camera, &open.stream_id);
    let batch = stored_media_batch(
        state,
        StoredMediaBatchRequest {
            stored_media_id: &open.stored_media_id,
            source_id: &open.source_id,
            stream_id: &stored_stream_id,
            recording_stream_id: &recording_stream_id,
            requested_time_ms,
            end_time_ms,
            mode,
            media_target,
            max_buffer_ms,
            generation: 1,
        },
    )?;
    let demand = state.recording_demand.acquire(recording_stream_id.clone());
    let status = stored_media_status(end_time_ms, batch.delivered_through_ms);
    let cursor = StoredMediaCursor {
        source_id: open.source_id,
        stream_id: stored_stream_id,
        recording_stream_id,
        requested_time_ms,
        end_time_ms,
        mode,
        playing: open.playing,
        playback_rate: open.playback_rate,
        media_target,
        media_channel,
        max_buffer_ms,
        generation: 1,
        content_type: batch.content_type.clone(),
        fragment_time_ms: batch.fragment_time_ms,
        delivered_through_ms: batch.delivered_through_ms,
        status,
        _demand: demand,
    };
    let cursor_state = proto_stored_media_state(&open.stored_media_id, &cursor);
    Ok((cursor, cursor_state, batch.messages))
}

fn seek_stored_media(
    state: &ServerState,
    session_id: SessionId,
    seek: proto::SeekStoredMedia,
) -> Result<(proto::StoredMediaState, Vec<OutboundDataMessage>), ControlCommandError> {
    let requested_time_ms =
        required_timestamp_ms(seek.timestamp.as_ref(), "stored media seek time")?;
    let (
        source_id,
        stream_id,
        recording_stream_id,
        end_time_ms,
        mode,
        media_target,
        max_buffer_ms,
        previous_generation,
    ) = {
        let cursors = state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursor = cursors
            .get(&(session_id, seek.stored_media_id.clone()))
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::NotFound,
                    404,
                    "stored media cursor was not found",
                )
            })?;
        if cursor
            .end_time_ms
            .is_some_and(|end_time_ms| requested_time_ms >= end_time_ms)
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "stored media seek time is unavailable",
            ));
        }
        (
            cursor.source_id.clone(),
            cursor.stream_id.clone(),
            cursor.recording_stream_id.clone(),
            cursor.end_time_ms,
            cursor.mode,
            cursor.media_target,
            cursor.max_buffer_ms,
            cursor.generation,
        )
    };
    let generation = previous_generation.checked_add(1).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "stored media generation overflowed",
        )
    })?;
    let batch = stored_media_batch(
        state,
        StoredMediaBatchRequest {
            stored_media_id: &seek.stored_media_id,
            source_id: &source_id,
            stream_id: &stream_id,
            recording_stream_id: &recording_stream_id,
            requested_time_ms,
            end_time_ms,
            mode,
            media_target,
            max_buffer_ms,
            generation,
        },
    )?;
    let mut cursors = state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cursor = cursors
        .get_mut(&(session_id, seek.stored_media_id.clone()))
        .filter(|cursor| cursor.generation == previous_generation)
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "stored media cursor changed during seek",
            )
        })?;
    cursor.requested_time_ms = requested_time_ms;
    cursor.generation = generation;
    cursor.content_type = batch.content_type;
    cursor.fragment_time_ms = batch.fragment_time_ms;
    cursor.delivered_through_ms = batch.delivered_through_ms;
    cursor.status = stored_media_status(cursor.end_time_ms, cursor.delivered_through_ms);
    let cursor_state = proto_stored_media_state(&seek.stored_media_id, cursor);
    Ok((cursor_state, batch.messages))
}

fn refill_stored_media(
    state: &ServerState,
    session_id: SessionId,
    refill: proto::RefillStoredMedia,
) -> Result<(proto::StoredMediaState, Vec<OutboundDataMessage>), ControlCommandError> {
    let playback_time_ms =
        required_timestamp_ms(refill.playback_time.as_ref(), "stored media playback time")?;
    let (
        recording_stream_id,
        end_time_ms,
        media_target,
        max_buffer_ms,
        previous_generation,
        delivered_through_ms,
        content_type,
        can_refill,
    ) = {
        let cursors = state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursor = cursors
            .get(&(session_id, refill.stored_media_id.clone()))
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::NotFound,
                    404,
                    "stored media cursor was not found",
                )
            })?;
        (
            cursor.recording_stream_id.clone(),
            cursor.end_time_ms,
            cursor.media_target,
            cursor.max_buffer_ms,
            cursor.generation,
            cursor.delivered_through_ms,
            cursor.content_type.clone(),
            cursor.status == proto::StoredMediaStatus::Active
                && cursor.mode == proto::StoredMediaMode::Playback
                && cursor.playing,
        )
    };
    if !can_refill {
        let cursors = state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursor = cursors
            .get(&(session_id, refill.stored_media_id.clone()))
            .expect("stored media cursor was validated above");
        return Ok((
            proto_stored_media_state(&refill.stored_media_id, cursor),
            Vec::new(),
        ));
    }
    let target_buffer_ms = max_buffer_ms.min(STORED_MEDIA_TARGET_BUFFER_MS);
    let buffer_end =
        playback_time_ms.saturating_add(i64::try_from(target_buffer_ms).unwrap_or(i64::MAX));
    let delivery_end = end_time_ms.map_or(buffer_end, |end_time| end_time.min(buffer_end));
    if delivery_end <= delivered_through_ms {
        let cursors = state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursor = cursors
            .get(&(session_id, refill.stored_media_id.clone()))
            .expect("stored media cursor was validated above");
        return Ok((
            proto_stored_media_state(&refill.stored_media_id, cursor),
            Vec::new(),
        ));
    }
    let Some(batch) = stored_media_continuation_batch(
        state,
        &refill.stored_media_id,
        &recording_stream_id,
        delivered_through_ms,
        delivery_end,
        media_target,
        previous_generation,
    )?
    else {
        let cursors = state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursor = cursors
            .get(&(session_id, refill.stored_media_id.clone()))
            .expect("stored media cursor was validated above");
        return Ok((
            proto_stored_media_state(&refill.stored_media_id, cursor),
            Vec::new(),
        ));
    };
    if batch.content_type != content_type {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "stored media codec changed during refill",
        ));
    }
    let mut cursors = state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cursor = cursors
        .get_mut(&(session_id, refill.stored_media_id.clone()))
        .filter(|cursor| cursor.generation == previous_generation)
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "stored media cursor changed during refill",
            )
        })?;
    cursor.delivered_through_ms = batch.delivered_through_ms;
    cursor.status = stored_media_status(cursor.end_time_ms, cursor.delivered_through_ms);
    let cursor_state = proto_stored_media_state(&refill.stored_media_id, cursor);
    Ok((cursor_state, batch.messages))
}

fn set_stored_media_playback(
    state: &ServerState,
    session_id: SessionId,
    update: proto::SetStoredMediaPlayback,
) -> Result<(proto::StoredMediaState, Vec<OutboundDataMessage>), ControlCommandError> {
    let playback_rate = update
        .playback_rate
        .map(|playback_rate| {
            if !playback_rate.is_finite() || playback_rate <= 0.0 {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "stored media playback rate must be finite and positive",
                ));
            }
            Ok(playback_rate)
        })
        .transpose()?;
    let mode = update
        .mode
        .map(|mode| {
            let mode = proto::StoredMediaMode::try_from(mode).map_err(|_| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "stored media mode is invalid",
                )
            })?;
            if mode == proto::StoredMediaMode::Unspecified {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "stored media mode is required",
                ));
            }
            Ok(mode)
        })
        .transpose()?;
    let (
        source_id,
        stream_id,
        recording_stream_id,
        requested_time_ms,
        end_time_ms,
        media_target,
        max_buffer_ms,
        generation,
        next_mode,
        next_playing,
        starts_playback,
    ) = {
        let cursors = state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursor = cursors
            .get(&(session_id, update.stored_media_id.clone()))
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::NotFound,
                    404,
                    "stored media cursor was not found",
                )
            })?;
        let next_mode = mode.unwrap_or(cursor.mode);
        let next_playing = update.playing.unwrap_or(cursor.playing);
        if next_mode == proto::StoredMediaMode::Scrub && next_playing {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "stored media scrub mode must be paused",
            ));
        }
        (
            cursor.source_id.clone(),
            cursor.stream_id.clone(),
            cursor.recording_stream_id.clone(),
            cursor.requested_time_ms,
            cursor.end_time_ms,
            cursor.media_target,
            cursor.max_buffer_ms,
            cursor.generation,
            next_mode,
            next_playing,
            cursor.mode == proto::StoredMediaMode::Scrub
                && next_mode == proto::StoredMediaMode::Playback,
        )
    };
    let batch = starts_playback
        .then(|| {
            stored_media_batch(
                state,
                StoredMediaBatchRequest {
                    stored_media_id: &update.stored_media_id,
                    source_id: &source_id,
                    stream_id: &stream_id,
                    recording_stream_id: &recording_stream_id,
                    requested_time_ms,
                    end_time_ms,
                    mode: next_mode,
                    media_target,
                    max_buffer_ms,
                    generation,
                },
            )
        })
        .transpose()?;
    let mut cursors = state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cursor = cursors
        .get_mut(&(session_id, update.stored_media_id.clone()))
        .filter(|cursor| cursor.generation == generation)
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "stored media cursor changed during playback update",
            )
        })?;
    cursor.mode = next_mode;
    cursor.playing = next_playing;
    if let Some(playback_rate) = playback_rate {
        cursor.playback_rate = playback_rate;
    }
    let messages = if let Some(batch) = batch {
        cursor.content_type = batch.content_type;
        cursor.fragment_time_ms = batch.fragment_time_ms;
        cursor.delivered_through_ms = batch.delivered_through_ms;
        cursor.status = stored_media_status(cursor.end_time_ms, cursor.delivered_through_ms);
        batch.messages
    } else {
        Vec::new()
    };
    Ok((
        proto_stored_media_state(&update.stored_media_id, cursor),
        messages,
    ))
}

fn stored_media_batch(
    state: &ServerState,
    request: StoredMediaBatchRequest<'_>,
) -> Result<StoredMediaBatch, ControlCommandError> {
    let Some(catalog) = &state.catalog else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        ));
    };
    let lookup_end = request.requested_time_ms.checked_add(1).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media time is out of range",
        )
    })?;
    let selected = catalog
        .media_fragments_in_range(
            request.recording_stream_id,
            request.requested_time_ms,
            lookup_end,
        )
        .map_err(|error| stored_catalog_error("locate stored media fragment", error))?
        .into_iter()
        .max_by_key(|fragment| fragment.start_ms)
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "stored media timestamp is unavailable",
            )
        })?;
    if request.mode == proto::StoredMediaMode::Scrub {
        return encode_stored_media_keyframe(state, request, &selected);
    }
    let target_buffer_ms = request.max_buffer_ms.min(STORED_MEDIA_TARGET_BUFFER_MS);
    let buffer_end = request
        .requested_time_ms
        .saturating_add(i64::try_from(target_buffer_ms).unwrap_or(i64::MAX));
    let delivery_end = request
        .end_time_ms
        .map_or(buffer_end, |end_time_ms| end_time_ms.min(buffer_end));
    let mut fragments = catalog
        .media_fragments_in_range(request.recording_stream_id, selected.start_ms, delivery_end)
        .map_err(|error| stored_catalog_error("query stored media fragments", error))?;
    if fragments.is_empty() {
        fragments.push(selected.clone());
    }
    encode_stored_media_fragments(
        request.stored_media_id,
        request.generation,
        selected.start_ms,
        request.media_target,
        fragments,
    )
}

fn encode_stored_media_keyframe(
    state: &ServerState,
    request: StoredMediaBatchRequest<'_>,
    fragment: &CatalogMediaFragment,
) -> Result<StoredMediaBatch, ControlCommandError> {
    let catalog = state.catalog.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        )
    })?;
    let location = catalog
        .resolve_media_object(
            request.source_id,
            request.stream_id,
            Some(request.recording_stream_id),
            &fragment.recording_id,
            fragment.sequence,
        )
        .map_err(|error| stored_catalog_error("resolve stored media keyframe", error))?
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "stored media keyframe was not found",
            )
        })?;
    if location.keyframe_len > MAX_STORED_KEYFRAME_BYTES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            413,
            "stored media keyframe exceeds 4 MiB",
        ));
    }
    let initialization = read_stored_range(
        Path::new(&location.path),
        location.initialization_offset,
        location.initialization_len,
    )?;
    let fragment_bytes = read_stored_range(
        Path::new(&location.path),
        location.fragment_offset,
        location.fragment_len,
    )?;
    let format = indexed_video_format(&initialization, Some(&fragment_bytes))?;
    let payload = read_stored_range(
        Path::new(&location.path),
        location.keyframe_offset,
        location.keyframe_len,
    )?;
    let stream_binding_id = format!("stored:{}:video", request.stored_media_id);
    let configuration = proto::MediaDataConfiguration {
        stream_binding_id: stream_binding_id.clone(),
        codec: Some(proto::CodecDescriptor {
            name: format.decoder.codec,
            parameters: HashMap::new(),
        }),
        format: Some(proto::MediaDataFormat {
            format: Some(proto::media_data_format::Format::Video(
                proto::VideoDataFormat {
                    width: u32::from(format.decoder.width),
                    height: u32::from(format.decoder.height),
                    decoder_config: format.decoder.description,
                },
            )),
        }),
        configuration_revision: request.generation,
    };
    let chunk_count = protobuf_chunk_count(payload.len())?;
    let messages = payload
        .chunks(DATA_MESSAGE_CHUNK_BYTES)
        .enumerate()
        .map(|(chunk_index, chunk)| OutboundDataMessage {
            target: DataChannelTarget::Reliable,
            group: format!("stored:{}", request.stored_media_id),
            message: proto::Message {
                message: Some(proto::message::Message::StoredMedia(
                    proto::StoredMediaMessage {
                        message: Some(proto::stored_media_message::Message::KeyFrame(
                            proto::StoredMediaKeyFrame {
                                stored_media_id: request.stored_media_id.to_owned(),
                                generation: request.generation,
                                configuration: Some(configuration.clone()),
                                frame: Some(proto::VideoDataFrame {
                                    stream_binding_id: stream_binding_id.clone(),
                                    frame_id: request.generation,
                                    timestamp: Some(millis_timestamp(fragment.start_ms)),
                                    fragment_index: u32::try_from(chunk_index).unwrap_or(u32::MAX),
                                    fragment_count: chunk_count,
                                    key_frame: true,
                                    payload: chunk.to_vec(),
                                    decode_time: None,
                                    configuration_revision: request.generation,
                                }),
                            },
                        )),
                    },
                )),
            },
        })
        .collect();
    Ok(StoredMediaBatch {
        content_type: format.keyframe_content_type,
        fragment_time_ms: fragment.start_ms,
        delivered_through_ms: fragment.start_ms,
        messages,
    })
}

fn stored_media_continuation_batch(
    state: &ServerState,
    stored_media_id: &str,
    recording_stream_id: &str,
    start_time_ms: i64,
    end_time_ms: i64,
    media_target: DataChannelTarget,
    generation: u64,
) -> Result<Option<StoredMediaBatch>, ControlCommandError> {
    let Some(catalog) = &state.catalog else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        ));
    };
    let fragments = catalog
        .media_fragments_in_range(recording_stream_id, start_time_ms, end_time_ms)
        .map_err(|error| stored_catalog_error("refill stored media fragments", error))?;
    let Some(fragment_time_ms) = fragments.first().map(|fragment| fragment.start_ms) else {
        return Ok(None);
    };
    encode_stored_media_fragments(
        stored_media_id,
        generation,
        fragment_time_ms,
        media_target,
        fragments,
    )
    .map(Some)
}

fn encode_stored_media_fragments(
    stored_media_id: &str,
    generation: u64,
    fragment_time_ms: i64,
    media_target: DataChannelTarget,
    fragments: Vec<CatalogMediaFragment>,
) -> Result<StoredMediaBatch, ControlCommandError> {
    let delivered_through_ms = fragments.iter().fold(fragment_time_ms, |end, fragment| {
        end.max(
            fragment
                .start_ms
                .saturating_add(i64::try_from(fragment.duration_ms).unwrap_or(i64::MAX)),
        )
    });
    let mut last_period = None::<StoredMediaPeriodKey>;
    let mut initialization_id = 0u64;
    let mut content_type = None;
    let mut messages = Vec::new();
    let mut sequence = 0u64;
    for fragment in fragments {
        let initialization = read_stored_range(
            Path::new(&fragment.path),
            fragment.init_offset,
            fragment.init_len,
        )?;
        let payload = read_stored_range(
            Path::new(&fragment.path),
            fragment.byte_offset,
            fragment.byte_len,
        )?;
        let period = stored_media_period(&initialization, &payload)?;
        let period_key = StoredMediaPeriodKey {
            recording_id: fragment.recording_id.clone(),
            sample_descriptions: period.sample_descriptions,
        };
        if last_period.as_ref() != Some(&period_key) {
            initialization_id = initialization_id.saturating_add(1);
            content_type.get_or_insert_with(|| period.content_type.clone());
            append_initialization_messages(
                &mut messages,
                stored_media_id,
                generation,
                initialization_id,
                &period.content_type,
                &period.initialization,
            )?;
            last_period = Some(period_key);
        }
        sequence = sequence.saturating_add(1);
        append_fragment_messages(
            &mut messages,
            stored_media_id,
            generation,
            initialization_id,
            sequence,
            fragment.start_ms,
            fragment.duration_ms,
            media_target,
            &period.fragment,
        )?;
    }
    Ok(StoredMediaBatch {
        content_type: content_type.unwrap_or_else(|| "video/mp4".to_owned()),
        fragment_time_ms,
        delivered_through_ms,
        messages,
    })
}

fn stored_media_status(
    end_time_ms: Option<i64>,
    delivered_through_ms: i64,
) -> proto::StoredMediaStatus {
    if end_time_ms.is_some_and(|end_time_ms| delivered_through_ms >= end_time_ms) {
        proto::StoredMediaStatus::Ended
    } else {
        proto::StoredMediaStatus::Active
    }
}

fn terminal_stored_media_notification(state: &proto::StoredMediaState) -> Vec<proto::Notification> {
    if state.status != proto::StoredMediaStatus::Ended as i32 {
        return Vec::new();
    }
    vec![proto::Notification {
        event: Some(proto::notification::Event::StoredMediaState(state.clone())),
    }]
}

fn proto_stored_media_state(
    stored_media_id: &str,
    cursor: &StoredMediaCursor,
) -> proto::StoredMediaState {
    proto::StoredMediaState {
        stored_media_id: stored_media_id.to_owned(),
        status: cursor.status as i32,
        generation: cursor.generation,
        requested_time: Some(millis_timestamp(cursor.requested_time_ms)),
        fragment_time: Some(millis_timestamp(cursor.fragment_time_ms)),
        end_time: cursor.end_time_ms.map(millis_timestamp),
        mode: cursor.mode as i32,
        playing: cursor.playing,
        playback_rate: cursor.playback_rate,
        delivery: Some(proto::StoredMediaDelivery {
            media_channel: cursor.media_channel as i32,
            content_type: cursor.content_type.clone(),
            data_payload_routes: Vec::new(),
            max_buffer_duration: Some(millis_duration(cursor.max_buffer_ms)),
        }),
    }
}

fn read_stored_range(
    path: &Path,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, ControlCommandError> {
    if length == 0 || length > MAX_STORED_OBJECT_BYTES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "stored media byte range is invalid",
        ));
    }
    let mut file = File::open(path).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to open indexed recording: {error}"),
        )
    })?;
    let file_len = file
        .metadata()
        .map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to inspect indexed recording: {error}"),
            )
        })?
        .len();
    if offset.checked_add(length).is_none_or(|end| end > file_len) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "stored media byte range exceeds its indexed recording",
        ));
    }
    file.seek(SeekFrom::Start(offset)).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to seek indexed recording: {error}"),
        )
    })?;
    let capacity = usize::try_from(length).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "stored media byte range does not fit memory",
        )
    })?;
    let mut payload = Vec::with_capacity(capacity);
    file.take(length)
        .read_to_end(&mut payload)
        .map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to read indexed recording: {error}"),
            )
        })?;
    if payload.len() != capacity {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "indexed recording byte range was truncated",
        ));
    }
    Ok(payload)
}

fn fragmented_mp4_content_type(initialization: &[u8]) -> Result<String, ControlCommandError> {
    let reader = mp4::Mp4Reader::read_header(
        Cursor::new(initialization),
        initialization.len().try_into().unwrap_or(u64::MAX),
    )
    .map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to parse indexed MP4 initialization: {error}"),
        )
    })?;
    let mut codecs = Vec::new();
    let mut has_video = false;
    for track in reader.tracks().values() {
        let track_type = track.track_type().map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to read indexed MP4 track type: {error}"),
            )
        })?;
        match track_type {
            mp4::TrackType::Video => {
                for index in 1..=track.sample_description_count() {
                    let decoder = track
                        .video_decoder_config_for_description(
                            u32::try_from(index).unwrap_or(u32::MAX),
                        )
                        .map_err(|error| {
                            ControlCommandError::new(
                                proto::ErrorCode::Internal,
                                500,
                                format!("unable to read indexed MP4 video codec: {error}"),
                            )
                        })?;
                    if let Some(decoder) = decoder {
                        has_video = true;
                        if !codecs.contains(&decoder.codec) {
                            codecs.push(decoder.codec);
                        }
                    }
                }
            }
            mp4::TrackType::Audio => {
                if matches!(track.media_type(), Ok(mp4::MediaType::AAC)) {
                    codecs.push("mp4a.40.2".to_owned());
                }
            }
            mp4::TrackType::Subtitle => {}
        }
    }
    if !has_video {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "indexed MP4 initialization has no supported video track",
        ));
    }
    Ok(format!("video/mp4; codecs=\"{}\"", codecs.join(", ")))
}

fn stored_media_period(
    initialization: &[u8],
    fragment: &[u8],
) -> Result<StoredMediaPeriod, ControlCommandError> {
    let mut media = Vec::with_capacity(initialization.len() + fragment.len());
    media.extend_from_slice(initialization);
    media.extend_from_slice(fragment);
    let reader = mp4::Mp4Reader::read_header(
        Cursor::new(media),
        (initialization.len() + fragment.len())
            .try_into()
            .unwrap_or(u64::MAX),
    )
    .map_err(|error| stored_mp4_period_error("parse", error))?;
    let mut tracks = reader.tracks().iter().collect::<Vec<_>>();
    tracks.sort_unstable_by_key(|(track_id, _)| **track_id);
    let mut sample_descriptions = Vec::with_capacity(tracks.len());
    let mut track_configs = Vec::with_capacity(tracks.len());
    for (_, track) in tracks {
        let sample_description = if track.sample_count() == 0 {
            1
        } else {
            track
                .sample_description_index(1)
                .map_err(|error| stored_mp4_period_error("resolve sample description", error))?
        };
        sample_descriptions.push(sample_description);
        track_configs.push(mp4::FragmentedTrackConfig {
            track_type: track
                .track_type()
                .map_err(|error| stored_mp4_period_error("read track type", error))?,
            timescale: track.timescale(),
            language: track.language().to_owned(),
            sample_descriptions: vec![
                track
                    .media_config_for_description(sample_description)
                    .map_err(|error| stored_mp4_period_error("read sample description", error))?,
            ],
        });
    }
    let config = mp4::Mp4Config {
        major_brand: *reader.major_brand(),
        minor_version: reader.minor_version(),
        compatible_brands: reader.compatible_brands().to_vec(),
        timescale: reader.timescale(),
    };
    let writer = mp4::FragmentedMp4Writer::write_start_with_sample_descriptions(
        Cursor::new(Vec::new()),
        &config,
        &track_configs,
    )
    .map_err(|error| stored_mp4_period_error("write initialization", error))?;
    let range = writer.initialization();
    let bytes = writer.into_writer().into_inner();
    let initialization =
        bytes[range.offset as usize..(range.offset + range.size) as usize].to_vec();
    let fragment = mp4::normalize_fragment_sample_description_indices(fragment)
        .map_err(|error| stored_mp4_period_error("normalize fragment", error))?;
    let content_type = fragmented_mp4_content_type(&initialization)?;
    Ok(StoredMediaPeriod {
        sample_descriptions,
        initialization,
        fragment,
        content_type,
    })
}

fn stored_mp4_period_error(context: &str, error: mp4::Error) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Internal,
        500,
        format!("unable to {context} stored MP4 period: {error}"),
    )
}

fn append_initialization_messages(
    messages: &mut Vec<OutboundDataMessage>,
    stored_media_id: &str,
    generation: u64,
    initialization_id: u64,
    content_type: &str,
    payload: &[u8],
) -> Result<(), ControlCommandError> {
    let chunk_count = protobuf_chunk_count(payload.len())?;
    for (chunk_index, chunk) in payload.chunks(DATA_MESSAGE_CHUNK_BYTES).enumerate() {
        messages.push(OutboundDataMessage {
            target: DataChannelTarget::Reliable,
            group: format!("stored:{stored_media_id}"),
            message: proto::Message {
                message: Some(proto::message::Message::StoredMedia(
                    proto::StoredMediaMessage {
                        message: Some(proto::stored_media_message::Message::Initialization(
                            proto::StoredMediaInitialization {
                                stored_media_id: stored_media_id.to_owned(),
                                generation,
                                initialization_id,
                                content_type: content_type.to_owned(),
                                chunk_index: u32::try_from(chunk_index).unwrap_or(u32::MAX),
                                chunk_count,
                                payload: chunk.to_vec(),
                            },
                        )),
                    },
                )),
            },
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_fragment_messages(
    messages: &mut Vec<OutboundDataMessage>,
    stored_media_id: &str,
    generation: u64,
    initialization_id: u64,
    sequence: u64,
    start_time_ms: i64,
    duration_ms: u64,
    target: DataChannelTarget,
    payload: &[u8],
) -> Result<(), ControlCommandError> {
    let chunk_count = protobuf_chunk_count(payload.len())?;
    for (chunk_index, chunk) in payload.chunks(DATA_MESSAGE_CHUNK_BYTES).enumerate() {
        messages.push(OutboundDataMessage {
            target,
            group: format!("stored:{stored_media_id}"),
            message: proto::Message {
                message: Some(proto::message::Message::StoredMedia(
                    proto::StoredMediaMessage {
                        message: Some(proto::stored_media_message::Message::Fragment(
                            proto::StoredMediaFragment {
                                stored_media_id: stored_media_id.to_owned(),
                                generation,
                                initialization_id,
                                sequence,
                                start_time: Some(millis_timestamp(start_time_ms)),
                                duration: Some(millis_duration(duration_ms)),
                                chunk_index: u32::try_from(chunk_index).unwrap_or(u32::MAX),
                                chunk_count,
                                payload: chunk.to_vec(),
                            },
                        )),
                    },
                )),
            },
        });
    }
    Ok(())
}

fn protobuf_chunk_count(payload_len: usize) -> Result<u32, ControlCommandError> {
    u32::try_from(payload_len.div_ceil(DATA_MESSAGE_CHUNK_BYTES)).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "stored media object requires too many protobuf chunks",
        )
    })
}

fn millis_duration(milliseconds: u64) -> prost_types::Duration {
    prost_types::Duration {
        seconds: i64::try_from(milliseconds / 1_000).unwrap_or(i64::MAX),
        nanos: i32::try_from((milliseconds % 1_000) * 1_000_000).unwrap_or(0),
    }
}

fn stored_catalog_error(context: &str, error: anyhow::Error) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Internal,
        500,
        format!("unable to {context}: {error}"),
    )
}

fn start_event_search_query(
    state: &ServerState,
    session_id: SessionId,
    request: proto::QueryEvents,
) -> Result<proto::EventSearchDelivery, ControlCommandError> {
    validate_client_id(&request.query_id, "event search query ID")?;
    let (_, channel) = data_channel_target(request.channel)?;
    if channel != proto::DataChannelKind::ReliableData {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search results require reliable-data",
        ));
    }
    event_search_catalog(state)?;
    let group = format!("event-search-query:{}", request.query_id);
    let cancelled = register_event_search_task(state, session_id, &group)?;
    let worker_state = state.clone();
    let query_id = request.query_id.clone();
    let delivery = proto::EventSearchDelivery {
        query_id: query_id.clone(),
        channel: channel as i32,
    };
    let worker_group = group.clone();
    let worker_cancelled = cancelled.clone();
    let spawn = std::thread::Builder::new()
        .name("event-search-query".to_owned())
        .spawn(move || {
            let result = query_events(&worker_state, request).map(|(_, messages)| messages);
            deliver_event_search_task(
                &worker_state,
                session_id,
                &worker_group,
                &worker_cancelled,
                result,
                Some(query_id),
                None,
            );
        });
    if let Err(error) = spawn {
        finish_event_search_task(state, session_id, &group, &cancelled);
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to start event search query: {error}"),
        ));
    }
    Ok(delivery)
}

fn start_event_search_media(
    state: &ServerState,
    session_id: SessionId,
    request: proto::FetchEventSearchMedia,
) -> Result<proto::EventSearchMediaDelivery, ControlCommandError> {
    validate_client_id(&request.transfer_id, "event search transfer ID")?;
    if request.objects.is_empty() || request.objects.len() > MAX_EVENT_SEARCH_MEDIA_OBJECTS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search media transfer requires 1 to 64 objects",
        ));
    }
    let (_, channel) = data_channel_target(request.channel)?;
    event_search_catalog(state)?;
    let object_count = u32::try_from(request.objects.len()).unwrap_or(u32::MAX);
    let group = format!("event-search-media:{}", request.transfer_id);
    let cancelled = register_event_search_task(state, session_id, &group)?;
    let worker_state = state.clone();
    let transfer_id = request.transfer_id.clone();
    let delivery = proto::EventSearchMediaDelivery {
        transfer_id: transfer_id.clone(),
        channel: channel as i32,
        object_count,
    };
    let worker_group = group.clone();
    let worker_cancelled = cancelled.clone();
    let spawn = std::thread::Builder::new()
        .name("event-search-media".to_owned())
        .spawn(move || {
            let result =
                stream_event_search_media(&worker_state, &request, &worker_cancelled, |message| {
                    worker_state
                        .webrtc
                        .enqueue_api_data(session_id, message, worker_cancelled.clone())
                        .map_err(|error| {
                            ControlCommandError::new(
                                proto::ErrorCode::Unavailable,
                                503,
                                format!("event search media delivery stopped: {error}"),
                            )
                        })
                });
            if let Err(error) = result
                && !worker_cancelled.load(Ordering::Acquire)
            {
                let message = event_search_message(
                    DataChannelTarget::Reliable,
                    &worker_group,
                    proto::event_search_message::Message::Error(proto::EventSearchError {
                        context: Some(proto::event_search_error::Context::TransferId(transfer_id)),
                        code: error.code as i32,
                        message: error.message,
                    }),
                );
                let _ = worker_state.webrtc.enqueue_api_data(
                    session_id,
                    message,
                    worker_cancelled.clone(),
                );
            }
            let completion_state = worker_state.clone();
            let completion_group = worker_group.clone();
            let completion_cancelled = worker_cancelled.clone();
            let _ = worker_state.webrtc.complete_api_data_group(
                session_id,
                worker_group,
                Box::new(move || {
                    finish_event_search_task(
                        &completion_state,
                        session_id,
                        &completion_group,
                        &completion_cancelled,
                    );
                }),
            );
        });
    if let Err(error) = spawn {
        finish_event_search_task(state, session_id, &group, &cancelled);
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to start event search media transfer: {error}"),
        ));
    }
    Ok(delivery)
}

fn register_event_search_task(
    state: &ServerState,
    session_id: SessionId,
    group: &str,
) -> Result<Arc<AtomicBool>, ControlCommandError> {
    let mut tasks = state
        .event_search_tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (session_id, group.to_owned());
    if tasks.contains_key(&key) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "event search query or transfer ID is already active",
        ));
    }
    if tasks.len() >= MAX_EVENT_SEARCH_TASKS
        || tasks
            .keys()
            .filter(|(owner_session_id, _)| *owner_session_id == session_id)
            .count()
            >= MAX_EVENT_SEARCH_TASKS_PER_SESSION
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "event search task limit is reached",
        ));
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    tasks.insert(key, cancelled.clone());
    Ok(cancelled)
}

fn cancel_event_search_task(state: &ServerState, session_id: SessionId, group: &str) {
    let cancelled = state
        .event_search_tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(session_id, group.to_owned()))
        .cloned();
    if let Some(cancelled) = cancelled {
        cancelled.store(true, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
fn deliver_event_search_task(
    state: &ServerState,
    session_id: SessionId,
    group: &str,
    cancelled: &Arc<AtomicBool>,
    result: Result<Vec<OutboundDataMessage>, ControlCommandError>,
    query_id: Option<String>,
    transfer_id: Option<String>,
) {
    let messages = match result {
        Ok(messages) => messages,
        Err(error) if !cancelled.load(Ordering::Acquire) => vec![event_search_message(
            DataChannelTarget::Reliable,
            group,
            proto::event_search_message::Message::Error(proto::EventSearchError {
                context: query_id
                    .map(proto::event_search_error::Context::QueryId)
                    .or_else(|| transfer_id.map(proto::event_search_error::Context::TransferId)),
                code: error.code as i32,
                message: error.message,
            }),
        )],
        Err(_) => Vec::new(),
    };
    for message in messages {
        if cancelled.load(Ordering::Acquire)
            || state
                .webrtc
                .enqueue_api_data(session_id, message, cancelled.clone())
                .is_err()
        {
            break;
        }
    }
    let completion_state = state.clone();
    let completion_group = group.to_owned();
    let completion_cancelled = cancelled.clone();
    let _ = state.webrtc.complete_api_data_group(
        session_id,
        group.to_owned(),
        Box::new(move || {
            finish_event_search_task(
                &completion_state,
                session_id,
                &completion_group,
                &completion_cancelled,
            );
        }),
    );
}

fn finish_event_search_task(
    state: &ServerState,
    session_id: SessionId,
    group: &str,
    cancelled: &Arc<AtomicBool>,
) {
    let mut tasks = state
        .event_search_tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (session_id, group.to_owned());
    if tasks
        .get(&key)
        .is_some_and(|active| Arc::ptr_eq(active, cancelled))
    {
        tasks.remove(&key);
    }
}

fn replace_event_search_terms(
    state: &ServerState,
    request: &proto::ReplaceEventSearchTerms,
) -> Result<(), ControlCommandError> {
    validate_client_id(&request.event_id, "event ID")?;
    validate_client_id(&request.source_id, "event source ID")?;
    if request.terms.len() > 64 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search term count exceeds 64",
        ));
    }
    let catalog = event_search_catalog(state)?;
    require_stored_event(catalog, &request.event_id, &request.source_id)?;
    let terms = request
        .terms
        .iter()
        .map(|term| {
            let field = storage_event_search_field(term.field)?;
            if field == EventSearchField::EventType {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "event type search terms are catalog-owned",
                ));
            }
            let value = term.value.split_whitespace().collect::<Vec<_>>().join(" ");
            if value.is_empty() || value.len() > 256 {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "event search terms must contain 1 to 256 UTF-8 bytes",
                ));
            }
            Ok(EventSearchTerm { field, value })
        })
        .collect::<Result<Vec<_>, _>>()?;
    EventSearch::new(catalog.clone())
        .replace_terms(&request.event_id, &terms)
        .map_err(|error| stored_catalog_error("replace event search terms", error))
}

fn set_event_search_embedding(
    state: &ServerState,
    request: &proto::SetEventSearchEmbedding,
) -> Result<(), ControlCommandError> {
    validate_client_id(&request.event_id, "event ID")?;
    validate_client_id(&request.source_id, "event source ID")?;
    let catalog = event_search_catalog(state)?;
    require_stored_event(catalog, &request.event_id, &request.source_id)?;
    let embedding = proto_event_embedding(request.embedding.as_ref())?;
    EventSearch::new(catalog.clone())
        .set_embedding(&request.event_id, embedding)
        .map_err(|error| stored_catalog_error("set event search embedding", error))
}

fn query_events(
    state: &ServerState,
    request: proto::QueryEvents,
) -> Result<(proto::EventSearchDelivery, Vec<OutboundDataMessage>), ControlCommandError> {
    validate_client_id(&request.query_id, "event search query ID")?;
    let (target, channel) = data_channel_target(request.channel)?;
    if target != DataChannelTarget::Reliable {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search results require reliable-data",
        ));
    }
    validate_event_search_stream(&request.stream_id)?;
    if let Some(source_id) = request.source_id.as_deref() {
        validate_client_id(source_id, "event search source ID")?;
        let camera = state.camera(source_id).ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "event search source was not found",
            )
        })?;
        if !camera
            .info
            .profiles
            .iter()
            .any(|profile| profile.stream == request.stream_id)
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "event search stream was not found",
            ));
        }
    }
    let start_ms = required_timestamp_ms(request.start_time.as_ref(), "event search start time")?;
    let end_ms = required_timestamp_ms(request.end_time.as_ref(), "event search end time")?;
    if start_ms >= end_ms {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search start must precede its end",
        ));
    }
    if end_ms.saturating_sub(start_ms) > 31 * 86_400_000 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search window exceeds 31 days",
        ));
    }
    let preview_before_ms =
        event_search_preview_ms(request.preview_before.as_ref(), DEFAULT_PREVIEW_BEFORE_MS)?;
    let preview_after_ms =
        event_search_preview_ms(request.preview_after.as_ref(), DEFAULT_PREVIEW_AFTER_MS)?;
    if preview_before_ms.saturating_add(preview_after_ms) > 60_000 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event preview window exceeds 60 seconds",
        ));
    }
    let page_size = if request.page_size == 0 {
        50
    } else {
        request.page_size
    };
    if page_size > 128 || request.offset != 0 || request.page_token.len() > 4_096 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search page size, offset, or page token is invalid",
        ));
    }

    let catalog = event_search_catalog(state)?;
    let search = EventSearch::new(catalog.clone());
    let mut page = match request.search {
        Some(proto::query_events::Search::Text(text)) => {
            let query_text = text.query.trim();
            if query_text.is_empty() || query_text.len() > 256 {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "event text search must contain 1 to 256 UTF-8 bytes",
                ));
            }
            let field = text.field.map(storage_event_search_field).transpose()?;
            search
                .search_text(EventTextSearchQuery {
                    query: query_text.to_owned(),
                    field,
                    source_id: request.source_id.clone(),
                    stream_id: request.stream_id.clone(),
                    start_time_ms: start_ms,
                    end_time_ms: end_ms,
                    preview_before_ms,
                    preview_after_ms,
                    page_size,
                    page_token: (!request.page_token.is_empty())
                        .then(|| request.page_token.clone()),
                })
                .map_err(|error| event_search_error("search event metadata", error))?
        }
        Some(proto::query_events::Search::Semantic(semantic)) => search
            .search_semantic(EventSemanticSearchQuery {
                embedding: proto_event_embedding(semantic.embedding.as_ref())?,
                source_id: request.source_id.clone(),
                stream_id: request.stream_id.clone(),
                start_time_ms: start_ms,
                end_time_ms: end_ms,
                preview_before_ms,
                preview_after_ms,
                page_size,
                page_token: (!request.page_token.is_empty()).then(|| request.page_token.clone()),
            })
            .map_err(|error| event_search_error("search event embeddings", error))?,
        None => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event search criterion is required",
            ));
        }
    };
    remap_event_search_keyframes(state, catalog, &request.stream_id, &mut page.hits)?;

    let group = format!("event-search-query:{}", request.query_id);
    let mut messages = page
        .hits
        .into_iter()
        .enumerate()
        .map(|(index, hit)| {
            event_search_message(
                DataChannelTarget::Reliable,
                &group,
                proto::event_search_message::Message::Result(proto::EventSearchResult {
                    query_id: request.query_id.clone(),
                    sequence: u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1),
                    hit: Some(proto_event_search_hit(hit)),
                }),
            )
        })
        .collect::<Vec<_>>();
    let result_count = u64::try_from(messages.len()).unwrap_or(u64::MAX);
    messages.push(event_search_message(
        DataChannelTarget::Reliable,
        &group,
        proto::event_search_message::Message::QueryEnd(proto::EventSearchQueryEnd {
            query_id: request.query_id.clone(),
            result_count,
            next_offset: None,
            next_page_token: page.next_page_token.unwrap_or_default(),
            candidates_truncated: page.candidates_truncated,
        }),
    ));
    Ok((
        proto::EventSearchDelivery {
            query_id: request.query_id,
            channel: channel as i32,
        },
        messages,
    ))
}

fn fetch_event_search_media(
    state: &ServerState,
    request: proto::FetchEventSearchMedia,
) -> Result<(proto::EventSearchMediaDelivery, Vec<OutboundDataMessage>), ControlCommandError> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut messages = Vec::new();
    let delivery = stream_event_search_media(state, &request, &cancelled, |message| {
        messages.push(message);
        Ok(())
    })?;
    Ok((delivery, messages))
}

struct ResolvedEventSearchMediaObject {
    object_id: String,
    recording_id: String,
    fragment_sequence: u64,
    representation: proto::StoredMediaObjectRepresentation,
    content_type: String,
    path: PathBuf,
    offset: u64,
    length: u64,
    codec: String,
    width: u32,
    height: u32,
    decoder_config: Vec<u8>,
    nal_length_size: u32,
}

fn stream_event_search_media(
    state: &ServerState,
    request: &proto::FetchEventSearchMedia,
    cancelled: &AtomicBool,
    mut emit: impl FnMut(OutboundDataMessage) -> Result<(), ControlCommandError>,
) -> Result<proto::EventSearchMediaDelivery, ControlCommandError> {
    validate_client_id(&request.transfer_id, "event search transfer ID")?;
    if request.objects.is_empty() || request.objects.len() > MAX_EVENT_SEARCH_MEDIA_OBJECTS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search media transfer requires 1 to 64 objects",
        ));
    }
    let (target, channel) = data_channel_target(request.channel)?;
    let catalog = event_search_catalog(state)?;
    let mut object_ids = HashSet::new();
    let mut total_bytes = 0u64;
    let group = format!("event-search-media:{}", request.transfer_id);
    let mut objects = Vec::with_capacity(request.objects.len());
    for object in &request.objects {
        validate_client_id(&object.object_id, "event search object ID")?;
        validate_client_id(&object.source_id, "event search media source ID")?;
        validate_client_id(&object.recording_id, "event search recording ID")?;
        if object.fragment_sequence == 0 {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event search fragment sequence must be positive",
            ));
        }
        if !object_ids.insert(object.object_id.clone()) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event search object IDs must be unique within a transfer",
            ));
        }
        validate_event_search_stream(&object.stream_id)?;
        let camera = state.camera(&object.source_id);
        let stored_stream_id = camera.as_ref().map_or(object.stream_id.as_str(), |camera| {
            recording_stream(camera, &object.stream_id)
        });
        let legacy_recording_stream_id = camera
            .as_ref()
            .map(|camera| format!("{}/{}", camera.recording_label, stored_stream_id));
        let location = catalog
            .resolve_media_object(
                &object.source_id,
                stored_stream_id,
                legacy_recording_stream_id.as_deref(),
                &object.recording_id,
                object.fragment_sequence,
            )
            .map_err(|error| stored_catalog_error("resolve event search media", error))?
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::NotFound,
                    404,
                    "event search media object was not found",
                )
            })?;
        let initialization = read_stored_range(
            Path::new(&location.path),
            location.initialization_offset,
            location.initialization_len,
        )?;
        let fragment = read_stored_range(
            Path::new(&location.path),
            location.fragment_offset,
            location.fragment_len,
        )?;
        let video_format = indexed_video_format(&initialization, Some(&fragment))?;
        let representation = proto::StoredMediaObjectRepresentation::try_from(
            object.representation,
        )
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "stored media object representation is invalid",
            )
        })?;
        let (offset, length, content_type) = match representation {
            proto::StoredMediaObjectRepresentation::EncodedKeyframe => (
                location.keyframe_offset,
                location.keyframe_len,
                video_format.keyframe_content_type.clone(),
            ),
            proto::StoredMediaObjectRepresentation::Fmp4Initialization => (
                location.initialization_offset,
                location.initialization_len,
                video_format.mp4_content_type.clone(),
            ),
            proto::StoredMediaObjectRepresentation::Fmp4Gop => (
                location.fragment_offset,
                location.fragment_len,
                video_format.mp4_content_type.clone(),
            ),
            proto::StoredMediaObjectRepresentation::Unspecified => {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "stored media object representation is required",
                ));
            }
        };
        total_bytes = total_bytes.checked_add(length).ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                413,
                "event search media transfer size overflowed",
            )
        })?;
        if total_bytes > MAX_EVENT_SEARCH_MEDIA_BYTES {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                413,
                "event search media transfer exceeds 32 MiB",
            ));
        }
        objects.push(ResolvedEventSearchMediaObject {
            object_id: object.object_id.clone(),
            recording_id: location.recording_id,
            fragment_sequence: location.fragment_sequence,
            representation,
            content_type,
            path: PathBuf::from(location.path),
            offset,
            length,
            codec: video_format.decoder.codec,
            width: u32::from(video_format.decoder.width),
            height: u32::from(video_format.decoder.height),
            decoder_config: video_format.decoder.description,
            nal_length_size: u32::from(video_format.decoder.nal_length_size),
        });
    }
    objects.sort_by_key(|object| match object.representation {
        proto::StoredMediaObjectRepresentation::Fmp4Initialization => 0,
        proto::StoredMediaObjectRepresentation::EncodedKeyframe => 1,
        proto::StoredMediaObjectRepresentation::Fmp4Gop => 2,
        proto::StoredMediaObjectRepresentation::Unspecified => 3,
    });
    for object in objects {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        let object_target = if object.representation
            == proto::StoredMediaObjectRepresentation::Fmp4Initialization
        {
            DataChannelTarget::Reliable
        } else {
            target
        };
        stream_event_search_object(
            request,
            object_target,
            &group,
            &object,
            cancelled,
            &mut emit,
        )?;
    }
    if !cancelled.load(Ordering::Acquire) {
        emit(event_search_message(
            DataChannelTarget::Reliable,
            &group,
            proto::event_search_message::Message::MediaEnd(proto::EventSearchMediaEnd {
                transfer_id: request.transfer_id.clone(),
                object_count: u32::try_from(request.objects.len()).unwrap_or(u32::MAX),
            }),
        ))?;
    }
    Ok(proto::EventSearchMediaDelivery {
        transfer_id: request.transfer_id.clone(),
        channel: channel as i32,
        object_count: u32::try_from(request.objects.len()).unwrap_or(u32::MAX),
    })
}

fn stream_event_search_object(
    request: &proto::FetchEventSearchMedia,
    target: DataChannelTarget,
    group: &str,
    object: &ResolvedEventSearchMediaObject,
    cancelled: &AtomicBool,
    emit: &mut impl FnMut(OutboundDataMessage) -> Result<(), ControlCommandError>,
) -> Result<(), ControlCommandError> {
    let chunk_count = protobuf_chunk_count(usize::try_from(object.length).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "event search media length does not fit this platform",
        )
    })?)?;
    let mut file = File::open(&object.path).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("event search media file is unavailable: {error}"),
        )
    })?;
    let file_len = file
        .metadata()
        .map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                format!("event search media file cannot be inspected: {error}"),
            )
        })?
        .len();
    if object
        .offset
        .checked_add(object.length)
        .is_none_or(|end| end > file_len)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "event search media range is no longer available",
        ));
    }
    file.seek(SeekFrom::Start(object.offset)).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("event search media range cannot be opened: {error}"),
        )
    })?;
    let mut remaining = object.length;
    let mut chunk_index = 0u32;
    let mut buffer = vec![0; DATA_MESSAGE_CHUNK_BYTES];
    while remaining > 0 && !cancelled.load(Ordering::Acquire) {
        let length = usize::try_from(remaining.min(DATA_MESSAGE_CHUNK_BYTES as u64)).unwrap();
        file.read_exact(&mut buffer[..length]).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                format!("event search media range was truncated: {error}"),
            )
        })?;
        emit(event_search_message(
            target,
            group,
            proto::event_search_message::Message::MediaChunk(proto::EventSearchMediaChunk {
                transfer_id: request.transfer_id.clone(),
                object_id: object.object_id.clone(),
                representation: object.representation as i32,
                content_type: object.content_type.clone(),
                byte_len: object.length,
                chunk_index,
                chunk_count,
                payload: buffer[..length].to_vec(),
                recording_id: object.recording_id.clone(),
                fragment_sequence: object.fragment_sequence,
                codec: object.codec.clone(),
                width: object.width,
                height: object.height,
                decoder_config: object.decoder_config.clone(),
                nal_length_size: object.nal_length_size,
            }),
        ))?;
        remaining -= length as u64;
        chunk_index = chunk_index.saturating_add(1);
    }
    Ok(())
}

fn event_search_catalog(
    state: &ServerState,
) -> Result<&RecordingCatalogHandle, ControlCommandError> {
    state.catalog.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        )
    })
}

fn event_search_error(context: &str, error: anyhow::Error) -> ControlCommandError {
    if error.to_string().contains("event search page token") {
        return ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string());
    }
    if error.to_string().contains("event search snapshot changed") {
        return ControlCommandError::new(proto::ErrorCode::Rejected, 409, error.to_string());
    }
    stored_catalog_error(context, error)
}

fn require_stored_event(
    catalog: &RecordingCatalogHandle,
    event_id: &str,
    source_id: &str,
) -> Result<(), ControlCommandError> {
    let event = catalog
        .event_by_id(event_id)
        .map_err(|error| stored_catalog_error("locate stored event", error))?;
    if event
        .as_ref()
        .is_none_or(|event| event.camera_id != source_id)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "stored event was not found for the requested source",
        ));
    }
    Ok(())
}

fn storage_event_search_field(value: i32) -> Result<EventSearchField, ControlCommandError> {
    match proto::EventSearchField::try_from(value) {
        Ok(proto::EventSearchField::EventType) => Ok(EventSearchField::EventType),
        Ok(proto::EventSearchField::FaceName) => Ok(EventSearchField::FaceName),
        Ok(proto::EventSearchField::ObjectClass) => Ok(EventSearchField::ObjectClass),
        Ok(proto::EventSearchField::Text) => Ok(EventSearchField::Text),
        _ => Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search field is invalid",
        )),
    }
}

fn proto_event_embedding(
    embedding: Option<&proto::EventSearchEmbedding>,
) -> Result<EventEmbedding, ControlCommandError> {
    let embedding = embedding.ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search embedding is required",
        )
    })?;
    let model_id = embedding.model_id.trim();
    if model_id.is_empty() || model_id.len() > 128 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "embedding model ID must contain 1 to 128 UTF-8 bytes",
        ));
    }
    if embedding.values.is_empty()
        || embedding.values.len() > 4_096
        || embedding.values.iter().any(|value| !value.is_finite())
        || embedding
            .values
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            <= f64::EPSILON
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "embedding must contain 1 to 4096 finite, non-zero dimensions",
        ));
    }
    Ok(EventEmbedding {
        model_id: model_id.to_owned(),
        values: embedding.values.clone(),
    })
}

fn event_search_preview_ms(
    duration: Option<&prost_types::Duration>,
    default_ms: u64,
) -> Result<u64, ControlCommandError> {
    duration.map_or(Ok(default_ms), |duration| {
        optional_duration_ms(Some(duration))
    })
}

fn validate_event_search_stream(stream_id: &str) -> Result<(), ControlCommandError> {
    if !matches!(stream_id, "main" | "sub") {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search stream must be main or sub",
        ));
    }
    Ok(())
}

fn remap_event_search_keyframes(
    state: &ServerState,
    catalog: &RecordingCatalogHandle,
    stream_id: &str,
    hits: &mut [crate::storage::EventSearchHit],
) -> Result<(), ControlCommandError> {
    let mut hit_indexes = Vec::new();
    let mut requests = Vec::new();
    for (hit_index, hit) in hits.iter().enumerate() {
        if !hit.keyframes.is_empty() {
            continue;
        }
        let Some(camera) = state.camera(&hit.source_id) else {
            continue;
        };
        hit_indexes.push(hit_index);
        let stored_stream_id = recording_stream(&camera, stream_id);
        requests.push(crate::storage::catalog::EventPreviewRequest {
            event_id: hit.event_id.clone(),
            source_id: hit.source_id.clone(),
            stream_id: stored_stream_id.to_owned(),
            recording_stream_id: format!("{}/{}", camera.recording_label, stored_stream_id),
            event_time_ms: hit.start_time_ms,
            start_time_ms: hit.preview_start_ms,
            end_time_ms: hit.preview_end_ms,
        });
    }
    let resolutions = catalog
        .resolve_event_preview_keyframes(requests)
        .map_err(|error| stored_catalog_error("resolve event preview keyframes", error))?;
    for (hit_index, mut resolution) in hit_indexes.into_iter().zip(resolutions) {
        let hit = &mut hits[hit_index];
        if hit.event_id != resolution.event_id {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                "event preview resolution order changed",
            ));
        }
        for keyframe in &mut resolution.keyframes {
            keyframe.stream_id = stream_id.to_owned();
        }
        hit.keyframes = resolution.keyframes;
        hit.keyframes_truncated |= resolution.truncated;
    }
    Ok(())
}

fn proto_event_search_hit(hit: crate::storage::EventSearchHit) -> proto::EventSearchHit {
    let source_id = hit.source_id.clone();
    proto::EventSearchHit {
        event_id: hit.event_id,
        source_id: source_id.clone(),
        event_type: hit.event_type,
        start_time: Some(millis_timestamp(hit.start_time_ms)),
        end_time: hit.end_time_ms.map(millis_timestamp),
        score: hit.score,
        preview_start_time: Some(millis_timestamp(hit.preview_start_ms)),
        preview_end_time: Some(millis_timestamp(hit.preview_end_ms)),
        keyframes: hit
            .keyframes
            .into_iter()
            .map(|keyframe| proto::EventSearchKeyframe {
                source_id: source_id.clone(),
                stream_id: keyframe.stream_id,
                recording_id: keyframe.recording_id,
                fragment_sequence: keyframe.fragment_sequence,
                event_time: Some(millis_timestamp(keyframe.event_time_ms)),
                fragment_start_time: Some(millis_timestamp(keyframe.fragment_start_ms)),
                byte_len: keyframe.byte_len,
            })
            .collect(),
        keyframes_truncated: hit.keyframes_truncated,
    }
}

fn event_search_message(
    target: DataChannelTarget,
    group: &str,
    message: proto::event_search_message::Message,
) -> OutboundDataMessage {
    OutboundDataMessage {
        target,
        group: group.to_owned(),
        message: proto::Message {
            message: Some(proto::message::Message::EventSearch(
                proto::EventSearchMessage {
                    message: Some(message),
                },
            )),
        },
    }
}

struct IndexedVideoFormat {
    mp4_content_type: String,
    keyframe_content_type: String,
    decoder: mp4::Mp4VideoDecoderConfig,
}

fn indexed_video_format(
    initialization: &[u8],
    fragment: Option<&[u8]>,
) -> Result<IndexedVideoFormat, ControlCommandError> {
    let mut media = Vec::with_capacity(initialization.len() + fragment.map_or(0, <[u8]>::len));
    media.extend_from_slice(initialization);
    if let Some(fragment) = fragment {
        media.extend_from_slice(fragment);
    }
    let reader = mp4::Mp4Reader::read_header(
        Cursor::new(media.as_slice()),
        media.len().try_into().unwrap_or(u64::MAX),
    )
    .map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to parse indexed MP4 initialization: {error}"),
        )
    })?;
    for track in reader.tracks().values() {
        if track.track_type().ok() != Some(mp4::TrackType::Video) {
            continue;
        }
        let description_index = if track.sample_count() > 0 {
            track.sample_description_index(1).map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::Internal,
                    500,
                    format!("unable to resolve indexed video sample description: {error}"),
                )
            })?
        } else {
            1
        };
        let decoder = track
            .video_decoder_config_for_description(description_index)
            .map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::Internal,
                    500,
                    format!("unable to read indexed video decoder configuration: {error}"),
                )
            })?;
        if let Some(mut decoder) = decoder {
            let (coded_width, coded_height) =
                indexed_video_coded_dimensions(track, description_index)?;
            decoder.width = coded_width;
            decoder.height = coded_height;
            let keyframe_content_type = if decoder.codec.starts_with("avc1") {
                "video/h264; format=avcc"
            } else {
                "video/h265; format=hvcc"
            };
            return Ok(IndexedVideoFormat {
                mp4_content_type: fragmented_mp4_content_type(initialization)?,
                keyframe_content_type: keyframe_content_type.to_owned(),
                decoder,
            });
        }
    }
    Err(ControlCommandError::new(
        proto::ErrorCode::Internal,
        500,
        "indexed MP4 initialization has no supported video track",
    ))
}

fn indexed_video_coded_dimensions(
    track: &mp4::Mp4Track,
    description_index: u32,
) -> Result<(u16, u16), ControlCommandError> {
    let dimensions = match track
        .media_config_for_description(description_index)
        .map_err(indexed_video_config_error)?
    {
        mp4::MediaConfig::AvcConfig(config) => {
            let parameters = retina::codec::h264::parameters_from_sps_and_pps(
                &config.seq_param_set,
                &config.pic_param_set,
                retina::codec::h26x::Framing::FourByteLength,
            )
            .map_err(indexed_video_config_error)?;
            parameters.coded_pixel_dimensions()
        }
        mp4::MediaConfig::HevcConfig(config) => {
            let parameters = retina::codec::h265::parameters_from_vps_sps_pps(
                &config.vps,
                &config.sps,
                &config.pps,
                retina::codec::h26x::Framing::FourByteLength,
            )
            .map_err(indexed_video_config_error)?;
            parameters.coded_pixel_dimensions()
        }
        _ => return Err(indexed_video_config_error("unsupported video codec")),
    };
    Ok((
        u16::try_from(dimensions.0)
            .map_err(|_| indexed_video_config_error("coded video width exceeds u16"))?,
        u16::try_from(dimensions.1)
            .map_err(|_| indexed_video_config_error("coded video height exceeds u16"))?,
    ))
}

fn indexed_video_config_error(error: impl std::fmt::Display) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Internal,
        500,
        format!("indexed video decoder configuration is invalid: {error}"),
    )
}

fn query_stored_media_timeline(
    state: &ServerState,
    query: proto::QueryStoredMediaTimeline,
) -> Result<(proto::StoredMediaQueryDelivery, Vec<OutboundDataMessage>), ControlCommandError> {
    validate_client_id(&query.query_id, "stored media query ID")?;
    if !query.payload_types.is_empty() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::UnsupportedRequest,
            501,
            "stored timed-data payload queries are unavailable",
        ));
    }
    let start_ms = required_timestamp_ms(query.start_time.as_ref(), "query start time")?;
    let end_ms = required_timestamp_ms(query.end_time.as_ref(), "query end time")?;
    if start_ms >= end_ms {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media query start must precede its end",
        ));
    }
    let bucket_ms = optional_duration_ms(query.availability_bucket.as_ref())?;
    let (target, channel) = data_channel_target(query.channel)?;
    let Some(catalog) = &state.catalog else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        ));
    };

    let mut cameras = state.camera_entries();
    if !query.source_ids.is_empty() {
        let requested = query.source_ids.into_iter().collect::<HashSet<_>>();
        if requested.len() > cameras.len()
            || requested
                .iter()
                .any(|source_id| !cameras.iter().any(|camera| camera.info.id == *source_id))
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "stored media source was not found",
            ));
        }
        cameras.retain(|camera| requested.contains(&camera.info.id));
    }

    let mut ranges = Vec::new();
    if !query.omit_availability {
        for camera in &cameras {
            let mut streams = camera
                .info
                .profiles
                .iter()
                .map(|profile| profile.stream.as_str())
                .filter(|stream| matches!(*stream, "main" | "sub"))
                .collect::<Vec<_>>();
            streams.sort_unstable();
            streams.dedup();
            for stream in streams {
                let stream_id = recording_stream_id(camera, stream);
                let fragments = catalog
                    .media_fragments_in_range(&stream_id, start_ms, end_ms)
                    .map_err(|error| {
                        ControlCommandError::new(
                            proto::ErrorCode::Internal,
                            500,
                            format!("unable to query recording availability: {error}"),
                        )
                    })?;
                ranges.extend(fragments.into_iter().map(|fragment| {
                    let fragment_end = fragment
                        .start_ms
                        .saturating_add(i64::try_from(fragment.duration_ms).unwrap_or(i64::MAX));
                    let (range_start, range_end) = bucket_range(
                        fragment.start_ms.max(start_ms),
                        fragment_end.min(end_ms),
                        bucket_ms,
                    );
                    StoredTimelineRange {
                        source_id: camera.info.id.clone(),
                        stream_id: stream.to_owned(),
                        start_ms: range_start,
                        end_ms: range_end,
                    }
                }));
            }
        }
    }
    ranges.sort_unstable_by(|left, right| {
        (&left.source_id, &left.stream_id, left.start_ms).cmp(&(
            &right.source_id,
            &right.stream_id,
            right.start_ms,
        ))
    });
    let ranges = coalesce_timeline_ranges(ranges);

    let mut events = if let Some(selection) = query.events {
        query_stored_events(state, &cameras, start_ms, end_ms, selection)?
    } else {
        Vec::new()
    };
    events.sort_unstable_by(|left, right| {
        let left_time = left
            .event
            .start_time
            .as_ref()
            .and_then(timestamp_ms)
            .unwrap_or(i64::MIN);
        let right_time = right
            .event
            .start_time
            .as_ref()
            .and_then(timestamp_ms)
            .unwrap_or(i64::MIN);
        (left_time, &left.event.event_id).cmp(&(right_time, &right.event.event_id))
    });

    let mut data_messages = Vec::new();
    let mut range_offset = 0;
    let mut event_offset = 0;
    let mut page_count = 0u64;
    while range_offset < ranges.len() || event_offset < events.len() {
        let range_count = (ranges.len() - range_offset).min(STORED_QUERY_PAGE_ITEMS);
        let remaining = STORED_QUERY_PAGE_ITEMS - range_count;
        let event_count = (events.len() - event_offset).min(remaining);
        let availability = ranges[range_offset..range_offset + range_count]
            .iter()
            .map(|range| proto::StoredMediaRange {
                source_id: range.source_id.clone(),
                stream_id: range.stream_id.clone(),
                start_time: Some(millis_timestamp(range.start_ms)),
                end_time: Some(millis_timestamp(range.end_ms)),
            })
            .collect();
        let page_events = events[event_offset..event_offset + event_count]
            .iter()
            .map(|event| event.event.clone())
            .collect();
        page_count = page_count.saturating_add(1);
        data_messages.push(stored_query_message(
            target,
            proto::stored_media_query_message::Message::Page(proto::StoredMediaQueryPage {
                query_id: query.query_id.clone(),
                sequence: page_count,
                availability,
                data: Vec::new(),
                events: page_events,
            }),
        ));
        range_offset += range_count;
        event_offset += event_count;
    }

    let mut attachment_count = 0u64;
    for event in &events {
        let Some(attachment) = &event.attachment else {
            continue;
        };
        let payload = std::fs::read(&attachment.path).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to read stored event attachment: {error}"),
            )
        })?;
        attachment_count = attachment_count.saturating_add(1);
        let chunk_count = payload.len().div_ceil(DATA_MESSAGE_CHUNK_BYTES).max(1);
        let chunk_count = u32::try_from(chunk_count).map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                "stored event attachment requires too many chunks",
            )
        })?;
        for (chunk_index, chunk) in payload.chunks(DATA_MESSAGE_CHUNK_BYTES).enumerate() {
            data_messages.push(OutboundDataMessage {
                target,
                group: format!("query:{}", query.query_id),
                message: proto::Message {
                    message: Some(proto::message::Message::Event(proto::EventMessage {
                        message: Some(proto::event_message::Message::Attachment(
                            proto::EventAttachmentChunk {
                                context: Some(proto::event_attachment_chunk::Context::QueryId(
                                    query.query_id.clone(),
                                )),
                                event_id: attachment.event_id.clone(),
                                revision: 1,
                                attachment_id: "thumbnail".to_owned(),
                                attachment_type: "thumbnail".to_owned(),
                                content_type: "image/jpeg".to_owned(),
                                ordinal: 0,
                                timestamp: Some(millis_timestamp(attachment.timestamp_ms)),
                                sequence: attachment_count,
                                chunk_index: u32::try_from(chunk_index).unwrap_or(u32::MAX),
                                chunk_count,
                                payload: chunk.to_vec(),
                            },
                        )),
                    })),
                },
            });
        }
    }
    data_messages.push(stored_query_message(
        target,
        proto::stored_media_query_message::Message::End(proto::StoredMediaQueryEnd {
            query_id: query.query_id.clone(),
            page_count,
            attachment_count,
        }),
    ));

    Ok((
        proto::StoredMediaQueryDelivery {
            query_id: query.query_id,
            channel: channel as i32,
        },
        data_messages,
    ))
}

fn query_stored_events(
    state: &ServerState,
    cameras: &[CameraEntry],
    start_ms: i64,
    end_ms: i64,
    selection: proto::StoredMediaEventQuery,
) -> Result<Vec<StoredTimelineEvent>, ControlCommandError> {
    let Some(store) = &state.events else {
        return Ok(Vec::new());
    };
    let event_types = selection.event_types.into_iter().collect::<HashSet<_>>();
    let mut results = Vec::new();
    for camera in cameras {
        let events = store
            .events_in_range(&camera.info.id, start_ms, end_ms)
            .map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::Internal,
                    500,
                    format!("unable to query stored events: {error}"),
                )
            })?;
        for event in events {
            if !event_types.is_empty() && !event_types.contains(&event.kind) {
                continue;
            }
            let attachment = event
                .thumbnail_filename
                .as_ref()
                .and_then(|_| {
                    store
                        .thumbnail_path(&camera.info.id, &event.id)
                        .ok()
                        .flatten()
                })
                .and_then(|path| {
                    let byte_len = path.metadata().ok()?.len();
                    Some((path, byte_len))
                });
            let descriptors = attachment
                .as_ref()
                .map(|(_, byte_len)| {
                    vec![proto::EventAttachmentDescriptor {
                        attachment_id: "thumbnail".to_owned(),
                        attachment_type: "thumbnail".to_owned(),
                        content_type: "image/jpeg".to_owned(),
                        byte_len: Some(*byte_len),
                        ordinal: 0,
                        timestamp: Some(millis_timestamp(event.start_time_ms)),
                        text: None,
                    }]
                })
                .unwrap_or_default();
            let stored_attachment = selection
                .include_attachments
                .then(|| {
                    attachment.map(|(path, _)| StoredTimelineAttachment {
                        event_id: event.id.clone(),
                        timestamp_ms: event.start_time_ms,
                        path,
                    })
                })
                .flatten();
            results.push(StoredTimelineEvent {
                event: proto::Event {
                    event_id: event.id,
                    revision: 1,
                    source_id: event.camera_id,
                    media_kind: event
                        .stream
                        .as_ref()
                        .map(|_| proto::MediaKind::Video as i32),
                    origin: match event.source {
                        crate::storage::metadata::EventSource::Camera => {
                            proto::EventOrigin::Camera as i32
                        }
                        crate::storage::metadata::EventSource::KeepPeek => {
                            proto::EventOrigin::Keeppeek as i32
                        }
                    },
                    event_type: event.kind,
                    start_time: Some(millis_timestamp(event.start_time_ms)),
                    end_time: event.end_time_ms.map(millis_timestamp),
                    confidence: event.confidence,
                    bounding_box: event
                        .bbox
                        .map(|[x, y, width, height]| proto::EventBoundingBox {
                            x,
                            y,
                            width,
                            height,
                        }),
                    zone: event.zone,
                    text: None,
                    payload: None,
                    attachments: descriptors,
                    source_session_id: None,
                    subscription_id: None,
                },
                attachment: stored_attachment,
            });
        }
    }
    Ok(results)
}

fn validate_client_id(value: &str, name: &str) -> Result<(), ControlCommandError> {
    if value.is_empty() || value.len() > 64 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{name} must contain 1 to 64 visible ASCII characters"),
        ));
    }
    Ok(())
}

fn required_timestamp_ms(
    timestamp: Option<&prost_types::Timestamp>,
    name: &str,
) -> Result<i64, ControlCommandError> {
    timestamp.and_then(timestamp_ms).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{name} is missing or invalid"),
        )
    })
}

fn timestamp_ms(timestamp: &prost_types::Timestamp) -> Option<i64> {
    if !(0..1_000_000_000).contains(&timestamp.nanos) {
        return None;
    }
    timestamp
        .seconds
        .checked_mul(1_000)?
        .checked_add(i64::from(timestamp.nanos / 1_000_000))
}

fn millis_timestamp(milliseconds: i64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: milliseconds.div_euclid(1_000),
        nanos: i32::try_from(milliseconds.rem_euclid(1_000) * 1_000_000).unwrap_or(0),
    }
}

fn optional_duration_ms(
    duration: Option<&prost_types::Duration>,
) -> Result<u64, ControlCommandError> {
    let Some(duration) = duration else {
        return Ok(0);
    };
    if duration.seconds < 0 || !(0..1_000_000_000).contains(&duration.nanos) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "availability bucket duration is invalid",
        ));
    }
    u64::try_from(duration.seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000))
        .and_then(|milliseconds| {
            milliseconds.checked_add(u64::try_from(duration.nanos / 1_000_000).ok()?)
        })
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "availability bucket duration is too large",
            )
        })
}

fn data_channel_target(
    value: i32,
) -> Result<(DataChannelTarget, proto::DataChannelKind), ControlCommandError> {
    match proto::DataChannelKind::try_from(value) {
        Ok(proto::DataChannelKind::ReliableData) => Ok((
            DataChannelTarget::Reliable,
            proto::DataChannelKind::ReliableData,
        )),
        Ok(proto::DataChannelKind::UnreliableData) => Ok((
            DataChannelTarget::Unreliable,
            proto::DataChannelKind::UnreliableData,
        )),
        _ => Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media query requires a negotiated data channel",
        )),
    }
}

fn bucket_range(start_ms: i64, end_ms: i64, bucket_ms: u64) -> (i64, i64) {
    let Ok(bucket_ms) = i64::try_from(bucket_ms) else {
        return (start_ms, end_ms);
    };
    if bucket_ms == 0 {
        return (start_ms, end_ms);
    }
    let start = start_ms.div_euclid(bucket_ms).saturating_mul(bucket_ms);
    let end_bucket = end_ms.div_euclid(bucket_ms);
    let end = if end_ms.rem_euclid(bucket_ms) == 0 {
        end_bucket.saturating_mul(bucket_ms)
    } else {
        end_bucket.saturating_add(1).saturating_mul(bucket_ms)
    };
    (start, end)
}

fn coalesce_timeline_ranges(ranges: Vec<StoredTimelineRange>) -> Vec<StoredTimelineRange> {
    let mut merged: Vec<StoredTimelineRange> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && previous.source_id == range.source_id
            && previous.stream_id == range.stream_id
            && range.start_ms <= previous.end_ms
        {
            previous.end_ms = previous.end_ms.max(range.end_ms);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn stored_query_message(
    target: DataChannelTarget,
    message: proto::stored_media_query_message::Message,
) -> OutboundDataMessage {
    OutboundDataMessage {
        target,
        group: format!(
            "query:{}",
            match &message {
                proto::stored_media_query_message::Message::Page(page) => &page.query_id,
                proto::stored_media_query_message::Message::End(end) => &end.query_id,
            }
        ),
        message: proto::Message {
            message: Some(proto::message::Message::StoredMediaQuery(
                proto::StoredMediaQueryMessage {
                    message: Some(message),
                },
            )),
        },
    }
}

fn proto_runtime_configuration_result(
    result: RuntimeSettingsUpdateResponse,
) -> proto::RuntimeConfigurationResult {
    let config = result.config;
    proto::RuntimeConfigurationResult {
        config: Some(proto::SanitizedRuntimeConfiguration {
            host: config.host,
            port: u32::from(config.port),
            storage: Some(proto::RuntimeStorageConfiguration {
                medium_term_path: config.storage.medium_term_path,
                long_term_path: config.storage.long_term_path,
                recording_catalog_path: config.storage.recording_catalog_path,
                event_thumbnail_path: config.storage.event_thumbnail_path,
                event_thumbnail_max_mb: config.storage.event_thumbnail_max_mb,
                short_term_secs: config.storage.short_term_secs,
                medium_term_secs: config.storage.medium_term_secs,
                flush_interval_secs: config.storage.flush_interval_secs,
                write_buffer_bytes: config
                    .storage
                    .write_buffer_bytes
                    .try_into()
                    .unwrap_or(u64::MAX),
                long_term_max_gb: config.storage.long_term_max_gb,
            }),
            camera_count: config.camera_count.try_into().unwrap_or(u64::MAX),
            recording_estimate: Some(proto::RecordingCapacityEstimate {
                estimated_bitrate_bps: config.recording_estimate.estimated_bitrate_bps,
                bytes_per_day: config.recording_estimate.bytes_per_day,
                known_streams: config
                    .recording_estimate
                    .known_streams
                    .try_into()
                    .unwrap_or(u64::MAX),
                unknown_streams: config
                    .recording_estimate
                    .unknown_streams
                    .try_into()
                    .unwrap_or(u64::MAX),
                estimated_retention_days: config.recording_estimate.estimated_retention_days,
            }),
        }),
        restart_required: result.restart_required,
    }
}

fn proto_health_snapshot(health: ServerHealthResponse) -> proto::ServerHealthSnapshot {
    proto::ServerHealthSnapshot {
        status: health.status,
        generated_at_ms: health.generated_at_ms,
        uptime_seconds: health.uptime_seconds,
        version: health.version.to_owned(),
        totals: Some(proto_health_totals(health.totals)),
        system: Some(proto_system_health(health.system)),
        storage: Some(proto_storage_health(health.storage)),
        webrtc: Some(proto_webrtc_health(health.webrtc)),
        cameras: health
            .cameras
            .into_iter()
            .map(proto_camera_health)
            .collect(),
        issues: health
            .issues
            .into_iter()
            .map(|issue| proto::HealthIssueSnapshot {
                severity: issue.severity,
                scope: issue.scope,
                message: issue.message,
            })
            .collect(),
        health_contract_version: health.health_contract_version,
    }
}

fn proto_health_totals(totals: HealthTotals) -> proto::HealthTotalsSnapshot {
    proto::HealthTotalsSnapshot {
        configured_cameras: usize_u64(totals.configured_cameras),
        configured_video_streams: usize_u64(totals.configured_video_streams),
        ingress_fps: totals.ingress_fps,
        ingress_bitrate_bps: totals.ingress_bitrate_bps,
        frames: totals.frames,
        keyframes: totals.keyframes,
        drops: totals.drops,
        errors: totals.errors,
        reconnects: totals.reconnects,
        connected_cameras: usize_u64(totals.connected_cameras),
        fresh_cameras: usize_u64(totals.fresh_cameras),
        decodable_cameras: usize_u64(totals.decodable_cameras),
        recording_requested_cameras: usize_u64(totals.recording_requested_cameras),
        recording_cameras: usize_u64(totals.recording_cameras),
        unknown_cameras: usize_u64(totals.unknown_cameras),
        connected_video_streams: usize_u64(totals.connected_video_streams),
        fresh_video_streams: usize_u64(totals.fresh_video_streams),
        decodable_video_streams: usize_u64(totals.decodable_video_streams),
        recording_requested_video_streams: usize_u64(totals.recording_requested_video_streams),
        recording_video_streams: usize_u64(totals.recording_video_streams),
    }
}

fn proto_camera_health(camera: CameraHealth) -> proto::CameraHealthSnapshot {
    proto::CameraHealthSnapshot {
        id: camera.id,
        ip: camera.ip,
        name: camera.name,
        manufacturer: camera.manufacturer,
        model: camera.model,
        firmware_version: camera.firmware_version,
        backend: camera.backend,
        transport: camera.transport,
        state: camera.state.as_str().to_owned(),
        lifecycle: camera.lifecycle,
        last_error: camera.last_error,
        configured_profiles: camera
            .configured_profiles
            .into_iter()
            .map(proto_health_profile)
            .collect(),
        streams: camera
            .streams
            .into_iter()
            .map(proto_stream_health)
            .collect(),
        reason: camera.reason.as_str().to_owned(),
        reason_codes: camera
            .reason_codes
            .into_iter()
            .map(|reason| reason.as_str().to_owned())
            .collect(),
        detail: camera.detail,
        dimensions: Some(proto_camera_health_dimensions(camera.dimensions)),
    }
}

fn proto_camera_health_dimensions(
    dimensions: CameraHealthDimensions,
) -> proto::CameraHealthDimensionsSnapshot {
    let connected_video_stream_ids_known = dimensions.connected_video_stream_ids.is_some();
    proto::CameraHealthDimensionsSnapshot {
        configured: dimensions.configured,
        expected: dimensions.expected,
        configured_video_streams: usize_u64(dimensions.configured_video_streams),
        connected_video_streams: dimensions.connected_video_streams.map(usize_u64),
        reporting_video_streams: usize_u64(dimensions.reporting_video_streams),
        fresh_video_streams: usize_u64(dimensions.fresh_video_streams),
        decodable_video_streams: usize_u64(dimensions.decodable_video_streams),
        configured_video_stream_ids: dimensions.configured_video_stream_ids,
        connected_video_stream_ids: dimensions.connected_video_stream_ids.unwrap_or_default(),
        connected_video_stream_ids_known,
        reporting_video_stream_ids: dimensions.reporting_video_stream_ids,
        fresh_video_stream_ids: dimensions.fresh_video_stream_ids,
        decodable_video_stream_ids: dimensions.decodable_video_stream_ids,
        transport_connected: dimensions.transport_connected,
        latest_report_at_ms: dimensions.latest_report_at_ms,
        report_age_ms: dimensions.report_age_ms,
        frames_fresh: dimensions.frames_fresh,
        decodable: dimensions.decodable,
        recent_reconnects: dimensions.recent_reconnects,
        recent_drops: dimensions.recent_drops,
        recent_errors: dimensions.recent_errors,
        recording_requested: dimensions.recording_requested,
        recording_video_streams: usize_u64(dimensions.recording_video_streams),
        recording_streams_progressing: usize_u64(dimensions.recording_streams_progressing),
        recording_video_stream_ids: dimensions.recording_video_stream_ids,
        recording_progressing_stream_ids: dimensions.recording_progressing_stream_ids,
        recording_progressing: dimensions.recording_progressing,
        recording_progress_age_ms: dimensions.recording_progress_age_ms,
        battery_configured: dimensions.battery_configured,
        battery_registered: dimensions.battery_registered,
        battery_last_seen_age_ms: dimensions.battery_last_seen_age_ms,
        battery_sleeping: dimensions.battery_sleeping,
        battery_wake_pending_age_ms: dimensions.battery_wake_pending_age_ms,
    }
}

fn proto_health_profile(profile: ProfileSummary) -> proto::HealthProfileSummary {
    proto::HealthProfileSummary {
        name: profile.name,
        stream: profile.stream,
        encoding: profile.encoding,
        resolution: profile.resolution,
        framerate: profile.framerate,
        bitrate_kbps: profile.bitrate_kbps,
        gop: profile.gop,
        h264_profile: profile.h264_profile,
        audio: profile.audio.map(|audio| proto::HealthAudioProfileSummary {
            encoding: audio.encoding,
            sample_rate: audio.sample_rate,
            bitrate_kbps: audio.bitrate_kbps,
        }),
    }
}

fn proto_camera_stream_verification(
    stream: &str,
    rtsp_url: Option<&str>,
    username: &str,
    password: &str,
    transport: RtspTransport,
) -> proto::CameraStreamVerification {
    let Some(rtsp_url) = rtsp_url else {
        return proto::CameraStreamVerification {
            stream: stream.to_owned(),
            verified: false,
            codec: None,
            resolution: None,
            declared_fps: None,
            frames_received: 0,
            keyframe_received: false,
            elapsed_ms: 0,
            error: Some("No RTSP endpoint is available for this stream.".to_owned()),
        };
    };
    match probe_rtsp_video(
        rtsp_url,
        username,
        password,
        transport,
        CAMERA_STREAM_VERIFICATION_TIMEOUT,
    ) {
        Ok(evidence) => proto::CameraStreamVerification {
            stream: stream.to_owned(),
            verified: true,
            codec: Some(evidence.codec),
            resolution: Some(format!("{}x{}", evidence.width, evidence.height)),
            declared_fps: evidence.declared_fps,
            frames_received: evidence.frames_received,
            keyframe_received: evidence.keyframe_received,
            elapsed_ms: evidence.elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            error: None,
        },
        Err(error) => {
            tracing::debug!(%error, stream, "candidate RTSP media verification failed");
            let detail = error.to_string().to_ascii_lowercase();
            let message = if detail.contains("unauthorized")
                || detail.contains("authentication")
                || detail.contains("401")
            {
                "RTSP authentication was rejected."
            } else if detail.contains("keyframe") || detail.contains("no media progress") {
                "No video keyframe arrived before the verification deadline."
            } else if detail.contains("did not describe a video stream") {
                "The RTSP endpoint did not describe a video stream."
            } else {
                "RTSP connection or media verification failed."
            };
            proto::CameraStreamVerification {
                stream: stream.to_owned(),
                verified: false,
                codec: None,
                resolution: None,
                declared_fps: None,
                frames_received: 0,
                keyframe_received: false,
                elapsed_ms: 0,
                error: Some(message.to_owned()),
            }
        }
    }
}

fn proto_stream_health(stream: StreamHealth) -> proto::StreamHealthSnapshot {
    let dimensions = stream.dimensions;
    let ingress = stream.ingress;
    let report = ingress.report;
    proto::StreamHealthSnapshot {
        r#type: report.kind,
        codec: report.codec,
        resolution: report.resolution,
        fps: nonzero_f64(report.fps),
        expected_fps: nonzero_f64(report.expected_fps),
        kf_fps: nonzero_f64(report.kf_fps),
        kbps: nonzero_f64(report.kbps),
        max_frame_kb: nonzero_f64(report.max_frame_kb),
        gap_min_ms: nonzero_f64(report.gap_min_ms),
        gap_avg_ms: nonzero_f64(report.gap_avg_ms),
        gap_max_ms: nonzero_f64(report.gap_max_ms),
        jitter_samples: nonzero_u64(report.jitter_samples),
        jitter_p50_ms: nonzero_f64(report.jitter_p50_ms),
        jitter_p99_ms: nonzero_f64(report.jitter_p99_ms),
        frames: report.frames.and_then(nonzero_u64),
        bytes: report.bytes.and_then(nonzero_u64),
        keyframes: report.keyframes.and_then(nonzero_u64),
        reconnects: report.reconnects.and_then(nonzero_u64),
        drops: report.drops.and_then(nonzero_u64),
        errors: report.errors.and_then(nonzero_u64),
        updated_at_ms: ingress.updated_at_ms,
        report_age_ms: ingress.report_age_ms,
        frame_updated_at_ms: ingress.frame_updated_at_ms,
        frame_age_ms: ingress.frame_age_ms,
        keyframe_updated_at_ms: ingress.keyframe_updated_at_ms,
        keyframe_age_ms: ingress.keyframe_age_ms,
        recent_reconnects: ingress.recent_reconnects,
        recent_drops: ingress.recent_drops,
        recent_errors: ingress.recent_errors,
        state: stream.state.as_str().to_owned(),
        reason: stream.reason.as_str().to_owned(),
        reason_codes: stream
            .reason_codes
            .into_iter()
            .map(|reason| reason.as_str().to_owned())
            .collect(),
        detail: stream.detail,
        dimensions: Some(proto::StreamHealthDimensionsSnapshot {
            expected: dimensions.expected,
            transport_connected: dimensions.transport_connected,
            report_fresh: dimensions.report_fresh,
            report_freshness_threshold_ms: dimensions.report_freshness_threshold_ms,
            frames_fresh: dimensions.frames_fresh,
            frame_freshness_threshold_ms: dimensions.frame_freshness_threshold_ms,
            decodable: dimensions.decodable,
            keyframe_freshness_threshold_ms: dimensions.keyframe_freshness_threshold_ms,
            recent_reconnects: dimensions.recent_reconnects,
            recent_drops: dimensions.recent_drops,
            recent_errors: dimensions.recent_errors,
            recording_requested: dimensions.recording_requested,
            recording_progressing: dimensions.recording_progressing,
            recording_progress_age_ms: dimensions.recording_progress_age_ms,
        }),
    }
}

fn proto_system_health(system: crate::health::SystemHealth) -> proto::SystemHealthSnapshot {
    proto::SystemHealthSnapshot {
        host_name: system.host_name,
        os_name: system.os_name,
        os_version: system.os_version,
        kernel_version: system.kernel_version,
        architecture: system.architecture.to_owned(),
        system_uptime_seconds: system.system_uptime_seconds,
        boot_time_seconds: system.boot_time_seconds,
        logical_cores: usize_u64(system.logical_cores),
        physical_cores: system.physical_cores.map(usize_u64),
        cpu_brand: system.cpu_brand,
        system_cpu_percent: system.system_cpu_percent,
        process: Some(proto_process_health(system.process)),
        memory: Some(proto::MemoryHealthSnapshot {
            total_bytes: system.memory.total_bytes,
            used_bytes: system.memory.used_bytes,
            available_bytes: system.memory.available_bytes,
            total_swap_bytes: system.memory.total_swap_bytes,
            used_swap_bytes: system.memory.used_swap_bytes,
        }),
        load: Some(proto::LoadHealthSnapshot {
            one_minute: system.load.one_minute,
            five_minutes: system.load.five_minutes,
            fifteen_minutes: system.load.fifteen_minutes,
        }),
        cpus: system
            .cpus
            .into_iter()
            .map(|cpu| proto::CpuHealthSnapshot {
                name: cpu.name,
                usage_percent: cpu.usage_percent,
                frequency_mhz: cpu.frequency_mhz,
            })
            .collect(),
        network_egress_bps: system.network_egress_bps,
        networks: system
            .networks
            .into_iter()
            .map(|network| proto::NetworkHealthSnapshot {
                name: network.name,
                received_bytes_per_second: network.received_bytes_per_second,
                transmitted_bytes_per_second: network.transmitted_bytes_per_second,
                received_packets_per_second: network.received_packets_per_second,
                transmitted_packets_per_second: network.transmitted_packets_per_second,
                receive_errors: network.receive_errors,
                transmit_errors: network.transmit_errors,
                total_received_bytes: network.total_received_bytes,
                total_transmitted_bytes: network.total_transmitted_bytes,
            })
            .collect(),
        disks: system
            .disks
            .into_iter()
            .map(|disk| proto::DiskHealthSnapshot {
                name: disk.name,
                kind: disk.kind,
                file_system: disk.file_system,
                mount_point: disk.mount_point,
                total_bytes: disk.total_bytes,
                available_bytes: disk.available_bytes,
                used_bytes: disk.used_bytes,
                removable: disk.removable,
                stores_recordings: disk.stores_recordings,
            })
            .collect(),
        temperatures: system
            .temperatures
            .into_iter()
            .map(|temperature| proto::TemperatureHealthSnapshot {
                label: temperature.label,
                current_celsius: temperature.current_celsius,
                max_celsius: temperature.max_celsius,
                critical_celsius: temperature.critical_celsius,
            })
            .collect(),
    }
}

fn proto_process_health(process: crate::health::ProcessHealth) -> proto::ProcessHealthSnapshot {
    proto::ProcessHealthSnapshot {
        pid: process.pid,
        name: process.name,
        executable: process.executable,
        working_directory: process.working_directory,
        cpu_percent: process.cpu_percent,
        cpu_capacity_percent: process.cpu_capacity_percent,
        cpu_core_equivalents: process.cpu_core_equivalents,
        resident_memory_bytes: process.resident_memory_bytes,
        memory_capacity_percent: process.memory_capacity_percent,
        virtual_memory_bytes: process.virtual_memory_bytes,
        started_at_seconds: process.started_at_seconds,
        uptime_seconds: process.uptime_seconds,
        tasks: process.tasks.map(usize_u64),
        read_bytes_per_second: process.read_bytes_per_second,
        write_bytes_per_second: process.write_bytes_per_second,
        total_read_bytes: process.total_read_bytes,
        total_written_bytes: process.total_written_bytes,
    }
}

fn proto_storage_health(storage: StorageHealth) -> proto::StorageHealthSnapshot {
    proto::StorageHealthSnapshot {
        medium_term_path: storage.medium_term_path,
        long_term_path: storage.long_term_path,
        paths_are_same: storage.paths_are_same,
        short_term_seconds: storage.short_term_seconds,
        medium_term_seconds: storage.medium_term_seconds,
        flush_interval_seconds: storage.flush_interval_seconds,
        write_buffer_bytes: usize_u64(storage.write_buffer_bytes),
        long_term_max_bytes: storage.long_term_max_bytes,
        catalog_bytes: storage.catalog_bytes,
        catalog: storage.catalog.map(|catalog| proto::CatalogHealthSnapshot {
            recording_files: catalog.recording_files,
            finalized_files: catalog.finalized_files,
            active_files: catalog.active_files,
            fragments: catalog.fragments,
            fragment_bytes: catalog.fragment_bytes,
            events: catalog.events,
            open_events: catalog.open_events,
            event_thumbnails: catalog.event_thumbnails,
            oldest_recording_at_ms: catalog.oldest_recording_at_ms,
            newest_recording_at_ms: catalog.newest_recording_at_ms,
        }),
        demand: Some(proto::RecordingDemandHealthSnapshot {
            active_streams: usize_u64(storage.demand.active_streams),
            total_viewers: usize_u64(storage.demand.total_viewers),
            leased_streams: usize_u64(storage.demand.leased_streams),
            streams: storage
                .demand
                .streams
                .into_iter()
                .map(|stream| proto::RecordingDemandStreamHealthSnapshot {
                    stream_id: stream.stream_id,
                    viewers: usize_u64(stream.viewers),
                    lease_remaining_ms: stream.lease_remaining_ms,
                })
                .collect(),
        }),
    }
}

fn proto_webrtc_health(health: crate::webrtc::WebRtcHealth) -> proto::WebRtcHealthSnapshot {
    proto::WebRtcHealthSnapshot {
        active_sessions: usize_u64(health.active_sessions),
        adaptive_sessions: usize_u64(health.adaptive_sessions),
        multi_track_sessions: usize_u64(health.multi_track_sessions),
        multi_tracks: usize_u64(health.multi_tracks),
        fixed_sessions: usize_u64(health.fixed_sessions),
        active_main: usize_u64(health.active_main),
        active_sub: usize_u64(health.active_sub),
        requested_auto: usize_u64(health.requested_auto),
        requested_high: usize_u64(health.requested_high),
        requested_low: usize_u64(health.requested_low),
        estimated_bitrate_min_bps: health.estimated_bitrate_min_bps,
        estimated_bitrate_avg_bps: health.estimated_bitrate_avg_bps,
        estimated_bitrate_max_bps: health.estimated_bitrate_max_bps,
        source_bitrate_bps: health.source_bitrate_bps,
        published_frames: health.published_frames,
        published_bytes: health.published_bytes,
        delivered_frames: health.delivered_frames,
        written_frames: health.written_frames,
        queue_capacity: usize_u64(health.queue_capacity),
        queued_frames: usize_u64(health.queued_frames),
        queue_depth_max: usize_u64(health.queue_depth_max),
        queue_high_water: usize_u64(health.queue_high_water),
        queue_drops: health.queue_drops,
        queue_discarded_frames: health.queue_discarded_frames,
        queue_recovery_drops: health.queue_recovery_drops,
        session_queues: health
            .session_queues
            .into_iter()
            .map(|queue| proto::WebRtcSessionQueueHealthSnapshot {
                session_id: queue.session_id.as_u64(),
                track_id: queue.track_id.map(|track_id| track_id.to_string()),
                camera_ip: queue.camera_ip.to_string(),
                stream: queue.stream.to_string(),
                depth: usize_u64(queue.depth),
                high_water: usize_u64(queue.high_water),
                written_frames: queue.written_frames,
                full_drops: queue.full_drops,
                discarded_frames: queue.discarded_frames,
                recovery_drops: queue.recovery_drops,
            })
            .collect(),
        sources: health
            .sources
            .into_iter()
            .map(|source| proto::WebRtcSourceHealthSnapshot {
                camera_ip: source.camera_ip.to_string(),
                stream: source.stream.to_string(),
                subscribers: usize_u64(source.subscribers),
                bitrate_bps: source.bitrate_bps,
                has_keyframe: source.has_keyframe,
                keyframe_age_ms: source.keyframe_age_ms,
            })
            .collect(),
    }
}

fn usize_u64(value: usize) -> u64 {
    value.try_into().unwrap_or(u64::MAX)
}

fn nonzero_u64(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn nonzero_f64(value: f64) -> Option<f64> {
    (value != 0.0).then_some(value)
}

fn proto_discovered_camera(camera: DiscoveredCameraSettings) -> proto::DiscoveredCamera {
    proto::DiscoveredCamera {
        ip: camera.ip,
        brand: camera.brand,
        name: camera.name,
        model: camera.model,
        onvif_port: camera.onvif_port.map(u32::from),
        sources: camera.sources,
        configured: camera.configured,
        health: camera.health,
        catalog: camera
            .catalog
            .map(|catalog| proto_camera_catalog_camera(catalog.camera, catalog.stream_hints)),
    }
}

fn proto_camera_discovery_result(
    discovery_id: String,
    cameras: Vec<DiscoveredCameraSettings>,
    complete: bool,
    cancelled: bool,
) -> proto::CameraDiscoveryResult {
    proto::CameraDiscoveryResult {
        cameras: cameras.into_iter().map(proto_discovered_camera).collect(),
        discovery_id,
        complete,
        cancelled,
    }
}

fn proto_camera_catalog_info(database: &CameraDatabase) -> proto::CameraCatalogInfo {
    let metadata = database.metadata();
    proto::CameraCatalogInfo {
        version: metadata.version.clone(),
        tag: metadata.tag.clone(),
        generated_at: metadata.generated_at.clone(),
        camera_count: u32::try_from(metadata.camera_count).unwrap_or(u32::MAX),
        website_url: CAMERA_CATALOG_WEBSITE.to_owned(),
    }
}

fn proto_camera_catalog_camera(
    camera: CatalogCamera,
    stream_hints: Option<StreamHints>,
) -> proto::CameraCatalogCamera {
    proto::CameraCatalogCamera {
        id: camera.id,
        brand: camera.brand,
        model: camera.model,
        aliases: camera.aliases.into_vec(),
        camera_type: camera.camera_type,
        resolution_label: camera.resolution_label,
        megapixels: camera.megapixels,
        sensor: camera.sensor,
        field_of_view: camera.field_of_view,
        night_vision: camera.night_vision,
        ip_rating: camera.ip_rating,
        ik_rating: camera.ik_rating,
        two_way_audio: camera.two_way_audio,
        release_year: camera.release_year.map(u32::from),
        community_notes_count: camera.community_notes_count,
        protocols: camera.protocols.into_vec(),
        codecs: camera.codecs.into_vec(),
        streams: camera
            .streams
            .into_vec()
            .into_iter()
            .map(|stream| proto::CameraCatalogStream {
                name: stream.name,
                resolution: stream.resolution,
                fps: stream.fps.map(u32::from),
                codec: stream.codec,
            })
            .collect(),
        sources: camera.sources.into_vec(),
        stream_hints: stream_hints.map(|hints| proto::CameraCatalogStreamHints {
            main_rtsp_url: hints.main,
            sub_rtsp_url: hints.sub,
        }),
    }
}

fn proto_motion_detection(motion: MotionDetection) -> proto::MotionDetectionResult {
    proto::MotionDetectionResult {
        supported: motion.supported,
        controllable: motion.controllable,
        enabled: motion.enabled,
        error: motion.error,
    }
}

fn ptz_continuous_operation(
    movement: &proto::PtzContinuous,
) -> Result<(PtzOp, u32), ControlCommandError> {
    for (axis, value) in [
        ("pan", movement.pan),
        ("tilt", movement.tilt),
        ("zoom", movement.zoom),
    ] {
        if !value.is_finite() || !(-1.0..=1.0).contains(&value) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("PTZ {axis} must be finite and between -1 and 1"),
            ));
        }
    }
    let has_pan_tilt = movement.pan != 0.0 || movement.tilt != 0.0;
    if has_pan_tilt && movement.zoom != 0.0 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::UnsupportedRequest,
            501,
            "simultaneous PTZ steering and zoom is unavailable",
        ));
    }
    let operation = match (
        movement.pan.partial_cmp(&0.0).unwrap(),
        movement.tilt.partial_cmp(&0.0).unwrap(),
        movement.zoom.partial_cmp(&0.0).unwrap(),
    ) {
        (std::cmp::Ordering::Less, std::cmp::Ordering::Greater, _) => PtzOp::LeftUp,
        (std::cmp::Ordering::Less, std::cmp::Ordering::Less, _) => PtzOp::LeftDown,
        (std::cmp::Ordering::Greater, std::cmp::Ordering::Greater, _) => PtzOp::RightUp,
        (std::cmp::Ordering::Greater, std::cmp::Ordering::Less, _) => PtzOp::RightDown,
        (std::cmp::Ordering::Less, std::cmp::Ordering::Equal, _) => PtzOp::Left,
        (std::cmp::Ordering::Greater, std::cmp::Ordering::Equal, _) => PtzOp::Right,
        (std::cmp::Ordering::Equal, std::cmp::Ordering::Greater, _) => PtzOp::Up,
        (std::cmp::Ordering::Equal, std::cmp::Ordering::Less, _) => PtzOp::Down,
        (_, _, std::cmp::Ordering::Greater) => PtzOp::ZoomIn,
        (_, _, std::cmp::Ordering::Less) => PtzOp::ZoomOut,
        _ => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "PTZ movement must include a nonzero axis",
            ));
        }
    };
    let magnitude = f64::from(
        movement
            .pan
            .abs()
            .max(movement.tilt.abs())
            .max(movement.zoom.abs()),
    );
    let speed = (magnitude * 64.0).ceil().clamp(1.0, 64.0) as u32;
    Ok((operation, speed))
}

fn reolink_ptz(control: &CameraControl, operation: PtzOp, speed: u32) -> anyhow::Result<()> {
    let client = logged_in_reolink(control)?;
    client.ptz_ctrl(0, operation, speed)
}

fn reolink_ptz_presets(
    control: &CameraControl,
) -> Result<Vec<crate::cameras::PtzPreset>, ControlCommandError> {
    let client = logged_in_reolink(control).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            format!("camera PTZ login failed: {error}"),
        )
    })?;
    client.get_ptz_presets(0).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            format!("camera PTZ preset list failed: {error}"),
        )
    })
}

fn reolink_goto_preset(control: &CameraControl, preset_id: u32) -> anyhow::Result<()> {
    let client = logged_in_reolink(control)?;
    client.goto_preset(0, preset_id, 32)
}

fn logged_in_reolink(control: &CameraControl) -> anyhow::Result<ReolinkClient> {
    let mut client = ReolinkClient::new_with_http_port(control.ip, control.http_port);
    client.login(&control.username, &control.password)?;
    Ok(client)
}

fn proto_ptz_preset(
    preset: crate::cameras::PtzPreset,
) -> Result<proto::PtzPreset, ControlCommandError> {
    let preset_id = preset.token.parse().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "camera returned an invalid PTZ preset ID",
        )
    })?;
    Ok(proto::PtzPreset {
        preset_id,
        name: preset.name.unwrap_or_else(|| format!("Preset {preset_id}")),
    })
}
fn proto_camera_settings(camera: CameraSettings) -> proto::CameraSettings {
    proto::CameraSettings {
        id: camera.id,
        ip: camera.ip,
        display_name: camera.display_name,
        manufacturer_override: camera.manufacturer_override,
        username_configured: camera.username_configured,
        password_configured: camera.password_configured,
        onvif_port: camera.onvif_port.map(u32::from),
        http_port: camera.http_port.map(u32::from),
        main_rtsp_url: camera.main_rtsp_url,
        sub_rtsp_url: camera.sub_rtsp_url,
        uid_configured: camera.uid_configured,
        backend: match camera.backend.as_str() {
            "retina" => proto::CameraBackend::Retina as i32,
            "reo-proto" => proto::CameraBackend::ReoProto as i32,
            _ => proto::CameraBackend::Auto as i32,
        },
        transport: match camera.transport.as_str() {
            "udp" => proto::CameraTransport::Udp as i32,
            _ => proto::CameraTransport::Tcp as i32,
        },
        record_generic_motion_events: camera.record_generic_motion_events,
        recording_mode: match camera.recording_mode.as_str() {
            "off" => proto::CameraRecordingMode::Off as i32,
            "main" => proto::CameraRecordingMode::Main as i32,
            "both" => proto::CameraRecordingMode::Both as i32,
            "event-boost" => proto::CameraRecordingMode::EventBoost as i32,
            _ => proto::CameraRecordingMode::Sub as i32,
        },
        event_recording_duration_secs: u32::try_from(camera.event_recording_duration_secs)
            .unwrap_or(u32::MAX),
        health: camera.health,
        model: camera.model,
    }
}

fn camera_settings_update_from_proto(
    update: proto::UpdateCameraConfiguration,
) -> Result<CameraSettingsUpdate, ControlCommandError> {
    Ok(CameraSettingsUpdate {
        display_name: optional_string_update(update.display_name, "display name")?,
        manufacturer: optional_string_update(update.manufacturer, "manufacturer")?,
        username: update.username,
        password: update.password,
        onvif_port: optional_port_update(update.onvif_port, "ONVIF port")?,
        http_port: optional_port_update(update.http_port, "HTTP port")?,
        main_rtsp_url: optional_string_update(update.main_rtsp_url, "main RTSP URL")?,
        sub_rtsp_url: optional_string_update(update.sub_rtsp_url, "sub RTSP URL")?,
        uid: optional_string_update(update.uid, "UID")?,
        backend: optional_camera_backend(update.backend)?,
        transport: optional_camera_transport(update.transport)?,
        record_generic_motion_events: update.record_generic_motion_events,
        recording_mode: optional_camera_recording_mode(update.recording_mode)?,
        event_recording_duration_secs: update.event_recording_duration_secs.map(u64::from),
    })
}

fn optional_string_update(
    update: Option<proto::OptionalStringUpdate>,
    field: &str,
) -> Result<Option<Option<String>>, ControlCommandError> {
    update
        .map(|update| match update.value {
            Some(optional_string_update::Value::Set(value)) => Ok(Some(value)),
            Some(optional_string_update::Value::Clear(true)) => Ok(None),
            Some(optional_string_update::Value::Clear(false)) | None => {
                Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    format!("{field} update must set or clear the value"),
                ))
            }
        })
        .transpose()
}

fn optional_port_update(
    update: Option<proto::OptionalUint32Update>,
    field: &str,
) -> Result<Option<Option<u16>>, ControlCommandError> {
    update
        .map(|update| match update.value {
            Some(proto::optional_uint32_update::Value::Set(value)) => {
                u16::try_from(value).map(Some).map_err(|_| {
                    ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        format!("{field} must be between 1 and 65535"),
                    )
                })
            }
            Some(proto::optional_uint32_update::Value::Clear(true)) => Ok(None),
            Some(proto::optional_uint32_update::Value::Clear(false)) | None => {
                Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    format!("{field} update must set or clear the value"),
                ))
            }
        })
        .transpose()
}

fn optional_camera_backend(
    value: Option<i32>,
) -> Result<Option<CameraBackend>, ControlCommandError> {
    value
        .map(|value| match proto::CameraBackend::try_from(value) {
            Ok(proto::CameraBackend::Auto) => Ok(CameraBackend::Auto),
            Ok(proto::CameraBackend::Retina) => Ok(CameraBackend::Retina),
            Ok(proto::CameraBackend::ReoProto) => Ok(CameraBackend::ReoProto),
            Ok(proto::CameraBackend::Unspecified) | Err(_) => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "camera backend is invalid",
            )),
        })
        .transpose()
}

fn optional_camera_transport(
    value: Option<i32>,
) -> Result<Option<CameraTransport>, ControlCommandError> {
    value
        .map(|value| match proto::CameraTransport::try_from(value) {
            Ok(proto::CameraTransport::Tcp) => Ok(CameraTransport::Tcp),
            Ok(proto::CameraTransport::Udp) => Ok(CameraTransport::Udp),
            Ok(proto::CameraTransport::Unspecified) | Err(_) => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "camera transport is invalid",
            )),
        })
        .transpose()
}

fn optional_camera_recording_mode(
    value: Option<i32>,
) -> Result<Option<CameraRecordingMode>, ControlCommandError> {
    value
        .map(|value| match proto::CameraRecordingMode::try_from(value) {
            Ok(proto::CameraRecordingMode::Off) => Ok(CameraRecordingMode::Off),
            Ok(proto::CameraRecordingMode::Sub) => Ok(CameraRecordingMode::Sub),
            Ok(proto::CameraRecordingMode::Main) => Ok(CameraRecordingMode::Main),
            Ok(proto::CameraRecordingMode::Both) => Ok(CameraRecordingMode::Both),
            Ok(proto::CameraRecordingMode::EventBoost) => Ok(CameraRecordingMode::EventBoost),
            Ok(proto::CameraRecordingMode::Unspecified) | Err(_) => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "camera recording mode is invalid",
            )),
        })
        .transpose()
}

fn proto_logging_settings(settings: LoggingSettings) -> proto::LoggingSettingsResult {
    proto::LoggingSettingsResult {
        active_filter: settings.active_filter,
        default_filter: settings.default_filter.to_owned(),
        filter_error: settings.filter_error,
        version: settings.version.to_owned(),
        buffer: Some(proto::LogBufferStats {
            entry_count: settings.buffer.entry_count.try_into().unwrap_or(u64::MAX),
            byte_count: settings.buffer.byte_count.try_into().unwrap_or(u64::MAX),
            evicted_entries: settings.buffer.evicted_entries,
            max_entries: settings.buffer.max_entries.try_into().unwrap_or(u64::MAX),
            max_bytes: settings.buffer.max_bytes.try_into().unwrap_or(u64::MAX),
            active_streams: settings
                .buffer
                .active_streams
                .try_into()
                .unwrap_or(u64::MAX),
            max_streams: settings.buffer.max_streams.try_into().unwrap_or(u64::MAX),
        }),
    }
}

fn profile_summaries(profiles: &[MediaProfile]) -> Vec<ProfileSummary> {
    profiles
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
        |camera| profile_summaries(&camera.profiles),
    );
    let mut capabilities = camera
        .map(|camera| camera.capabilities.clone())
        .unwrap_or_default();
    capabilities.ptz |= camera.is_some_and(|camera| camera.ptz.is_some());
    let battery_uid = camera_config
        .uid
        .clone()
        .or_else(|| camera.and_then(|camera| camera.device.p2p_uid.clone()));

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
            capabilities,
            profiles,
        },
        reported_manufacturer,
        configuration: camera_config.clone(),
        battery_uid,
        recording_label: camera_config
            .name
            .clone()
            .unwrap_or_else(|| camera_config.ip.to_string()),
        control,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApiPrincipal {
    LoopbackAdministrator,
    AccessKey(AccessKeyFingerprint),
}

struct StoredMediaCursor {
    source_id: String,
    stream_id: String,
    recording_stream_id: String,
    requested_time_ms: i64,
    end_time_ms: Option<i64>,
    mode: proto::StoredMediaMode,
    playing: bool,
    playback_rate: f64,
    media_target: DataChannelTarget,
    media_channel: proto::DataChannelKind,
    max_buffer_ms: u64,
    generation: u64,
    content_type: String,
    fragment_time_ms: i64,
    delivered_through_ms: i64,
    status: proto::StoredMediaStatus,
    _demand: RecordingDemandGuard,
}

#[derive(Clone)]
struct ExportJobRecord {
    request: proto::CreateExportJob,
    job: proto::ExportJob,
    path: Option<PathBuf>,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct ServerState {
    host: String,
    port: u16,
    access_key: Arc<RwLock<AccessKey>>,
    allowed_origins: Arc<HashSet<String>>,
    api_session_owners: Arc<Mutex<HashMap<SessionId, ApiPrincipal>>>,
    stored_media_cursors: Arc<Mutex<HashMap<(SessionId, String), StoredMediaCursor>>>,
    ptz_owners: Arc<Mutex<HashMap<String, SessionId>>>,
    export_jobs: Arc<Mutex<HashMap<String, ExportJobRecord>>>,
    event_search_tasks: EventSearchTasks,
    camera_discovery_tasks: CameraDiscoveryTasks,
    cameras: Arc<RwLock<Vec<CameraEntry>>>,
    events: Option<EventStore>,
    recording_demand: RecordingDemand,
    recording_health: RecordingHealthRegistry,
    battery_wake: Option<BatteryWakeHandle>,
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
    camera_database: Option<Arc<CameraDatabase>>,
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
            access_key: Arc::new(RwLock::new(config.access_key)),
            allowed_origins: Arc::new(config.direct_card.allowed_origins.iter().cloned().collect()),
            api_session_owners: Arc::new(Mutex::new(HashMap::new())),
            stored_media_cursors: Arc::new(Mutex::new(HashMap::new())),
            ptz_owners: Arc::new(Mutex::new(HashMap::new())),
            export_jobs: Arc::new(Mutex::new(HashMap::new())),
            event_search_tasks: Arc::new(Mutex::new(HashMap::new())),
            camera_discovery_tasks: Arc::new(Mutex::new(HashMap::new())),
            cameras: Arc::new(RwLock::new(entries)),
            events: None,
            recording_demand,
            recording_health: RecordingHealthRegistry::default(),
            battery_wake: None,
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
            camera_database: None,
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
            RecordingDemand::new(TEST_RECORDING_DEMAND_GRACE),
            WebRtc::new(),
        )
    }

    #[doc(hidden)]
    pub fn for_test() -> Self {
        Self::empty()
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

    pub(crate) fn enrich_camera_metadata_in_background(&self, configs: Vec<CameraConfig>) {
        let pending = Arc::new(Mutex::new(VecDeque::from(configs)));
        let worker_count = pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
            .min(MAX_CAMERA_METADATA_WORKERS);
        for worker_index in 0..worker_count {
            let state = self.clone();
            let pending = pending.clone();
            let spawn = std::thread::Builder::new()
                .name(format!("camera-metadata-{worker_index}"))
                .spawn(move || {
                    loop {
                        let config = pending
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .pop_front();
                        let Some(config) = config else {
                            break;
                        };
                        match probe_onvif_camera(&config) {
                            Ok(probe) => state.apply_camera_metadata(config.ip, &probe),
                            Err(_) => tracing::debug!(
                                ip = %config.ip,
                                "configured camera did not provide ONVIF metadata"
                            ),
                        }
                    }
                });
            if let Err(error) = spawn {
                tracing::warn!(%error, "camera metadata worker could not start");
            }
        }
    }

    fn apply_camera_metadata(&self, ip: IpAddr, probe: &crate::cameras::ProbedOnvifCamera) {
        let catalog_brand = self.camera_database.as_ref().and_then(|database| {
            let model = probe.device.model.as_deref()?;
            let manufacturer = probe.device.manufacturer.as_deref().unwrap_or_default();
            match database.match_camera(manufacturer, model) {
                CameraMatch::Exact(camera) => Some(camera.brand),
                CameraMatch::Ambiguous | CameraMatch::Missing => None,
            }
        });
        let manufacturer = catalog_brand.or_else(|| probe.device.manufacturer.clone());
        let profiles = profile_summaries(&probe.profiles);
        let mut cameras = self
            .cameras
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(camera) = cameras
            .iter_mut()
            .find(|camera| camera.info.ip == ip.to_string())
        else {
            return;
        };
        camera.reported_manufacturer = manufacturer.clone();
        camera.info.manufacturer = manufacturer;
        camera.info.model.clone_from(&probe.device.model);
        camera
            .info
            .firmware_version
            .clone_from(&probe.device.firmware_version);
        camera
            .info
            .serial_number
            .clone_from(&probe.device.serial_number);
        camera
            .info
            .hardware_id
            .clone_from(&probe.device.hardware_id);
        camera.info.ports.onvif = Some(probe.onvif_port);
        if !profiles.is_empty() {
            camera.info.profiles = profiles;
        }
    }

    pub fn with_camera_config_path(mut self, config_path: PathBuf) -> Self {
        self.camera_config_path = Some(config_path);
        self
    }

    pub(crate) fn with_camera_database(mut self, camera_database: Arc<CameraDatabase>) -> Self {
        self.camera_database = Some(camera_database);
        self
    }

    #[doc(hidden)]
    pub fn with_test_camera_catalog(
        mut self,
        catalog: crate::test_support::TestCameraCatalog,
    ) -> Self {
        self.camera_database = Some(Arc::new(catalog.into_database()));
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

    pub(crate) fn with_recording_health(mut self, health: RecordingHealthRegistry) -> Self {
        self.recording_health = health;
        self
    }

    pub(crate) fn with_battery_wake(mut self, battery_wake: Option<BatteryWakeHandle>) -> Self {
        self.battery_wake = battery_wake;
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
    let resolved_medium_term_path = storage_config.medium_term_path.to_string_lossy();
    let resolved_long_term_path = storage_config.long_term_path.to_string_lossy();
    let medium_term_path =
        config.reference_or_value(&["storage", "medium_term_path"], &resolved_medium_term_path);
    let long_term_path =
        config.reference_or_value(&["storage", "long_term_path"], &resolved_long_term_path);
    let recording_catalog_path = if config.storage.recording_catalog_path.is_none()
        && long_term_path != resolved_long_term_path
    {
        format!(
            "{}/recordings.db",
            long_term_path.trim_end_matches(['/', '\\'])
        )
    } else {
        config.reference_or_value(
            &["storage", "recording_catalog_path"],
            &storage_config.recording_catalog_path.to_string_lossy(),
        )
    };
    let event_thumbnail_path = if config.storage.event_thumbnail_path.is_none()
        && long_term_path != resolved_long_term_path
    {
        format!(
            "{}/.event-thumbnails",
            long_term_path.trim_end_matches(['/', '\\'])
        )
    } else {
        config.reference_or_value(
            &["storage", "event_thumbnail_path"],
            &storage_config.event_thumbnail_path.to_string_lossy(),
        )
    };
    SanitizedConfig {
        host: config.reference_or_value(&["host"], &config.host),
        port: config.port,
        storage: SanitizedStorage {
            medium_term_path,
            long_term_path,
            recording_catalog_path,
            event_thumbnail_path,
            event_thumbnail_max_mb: config.storage.event_thumbnail_max_mb,
            short_term_secs: config.storage.short_term_secs,
            medium_term_secs: config.storage.medium_term_secs,
            flush_interval_secs: config.storage.flush_interval_secs,
            write_buffer_bytes: config.storage.write_buffer_bytes,
            long_term_max_gb: config.storage.long_term_max_gb,
        },
        camera_count,
        recording_estimate: recording_capacity_estimate(
            cameras.iter().flat_map(|camera| {
                camera.info.profiles.iter().filter(|profile| {
                    recording_mode_includes_stream(
                        camera.configuration.recording_mode,
                        &profile.stream,
                    )
                })
            }),
            storage_config.long_term_max_bytes,
        ),
    }
}

fn recording_mode_includes_stream(mode: CameraRecordingMode, stream: &str) -> bool {
    match mode {
        CameraRecordingMode::Off => false,
        CameraRecordingMode::Sub | CameraRecordingMode::EventBoost => stream == "sub",
        CameraRecordingMode::Main => stream == "main",
        CameraRecordingMode::Both => matches!(stream, "main" | "sub"),
    }
}

fn recording_stream<'a>(camera: &CameraEntry, requested_stream: &'a str) -> &'a str {
    if camera.configuration.recording_mode == CameraRecordingMode::EventBoost {
        "sub"
    } else {
        requested_stream
    }
}

fn recording_stream_id(camera: &CameraEntry, requested_stream: &str) -> String {
    format!(
        "{}/{}",
        camera.recording_label,
        recording_stream(camera, requested_stream)
    )
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
        let config = config::load_config(path)?;
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
    let control_handler: Arc<dyn ControlRequestHandler> =
        Arc::new(ServerControlHandler::new(state.clone(), router_tx.clone()));
    state
        .webrtc
        .set_control_handler(Arc::downgrade(&control_handler));
    let server = Server::from_tcp_listener(listener, move |request| {
        let _control_handler = &control_handler;
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
        (POST) (/create) => {
            authenticated_api_request(request, state, true, |principal| {
                create_api_session(request, state, principal)
            })
        },
        (POST) (/delete) => {
            authenticated_api_request(request, state, true, |principal| {
                delete_api_session(request, state, principal)
            })
        },
        (OPTIONS) (/create) => {
            api_preflight(request, state)
        },
        (OPTIONS) (/delete) => {
            api_preflight(request, state)
        },
        (GET) (/logs) => {
            authenticated_api_request(request, state, false, |_| log_stream(request, state))
        },
        (GET) (/logs/snapshot) => {
            authenticated_api_request(request, state, false, |_| log_snapshot(state))
        },
        (GET) (/metrics) => {
            authenticated_api_request(request, state, false, |_| {
                prometheus_metrics(router_tx, state)
            })
        },
        _ => serve_ui(request)
    )
}

fn authenticated_api_request(
    request: &Request,
    state: &ServerState,
    cors: bool,
    action: impl FnOnce(ApiPrincipal) -> Response,
) -> Response {
    let origin = if cors {
        match api_request_origin(request, state) {
            Ok(origin) => origin,
            Err(response) => return response,
        }
    } else {
        None
    };
    let response = match api_principal(request, state) {
        Ok(principal) => action(principal),
        Err(response) => response,
    };
    with_api_cors(response, origin)
}

fn api_preflight(request: &Request, state: &ServerState) -> Response {
    let Some(origin) = request.header("Origin") else {
        return api_status(403, "CORS origin is required");
    };
    if !state.allowed_origins.contains(origin) {
        return api_status(403, "CORS origin is not allowed");
    };
    with_api_cors(Response::empty_204(), Some(origin.to_owned()))
        .with_additional_header("Access-Control-Allow-Methods", "POST, OPTIONS")
        .with_additional_header(
            "Access-Control-Allow-Headers",
            "Authorization, Content-Type, Content-Encoding",
        )
}

fn api_request_origin(request: &Request, state: &ServerState) -> Result<Option<String>, Response> {
    let Some(origin) = request.header("Origin") else {
        return Ok(None);
    };
    if request_origin(request).as_deref() == Some(origin) || state.allowed_origins.contains(origin)
    {
        return Ok(Some(origin.to_owned()));
    }
    Err(api_status(403, "CORS origin is not allowed"))
}

fn request_origin(request: &Request) -> Option<String> {
    let host = request.header("Host")?;
    let scheme = if request.is_secure() { "https" } else { "http" };
    Url::parse(&format!("{scheme}://{host}"))
        .ok()
        .map(|url| url.origin().ascii_serialization())
}

fn with_api_cors(response: Response, origin: Option<String>) -> Response {
    let Some(origin) = origin else {
        return response;
    };
    response
        .with_additional_header("Access-Control-Allow-Origin", origin)
        .with_additional_header("Vary", "Origin")
}

fn api_principal(request: &Request, state: &ServerState) -> Result<ApiPrincipal, Response> {
    if is_trusted_loopback_request(request) {
        return Ok(ApiPrincipal::LoopbackAdministrator);
    }
    let Some(authorization) = request.header("Authorization") else {
        return Err(api_status(401, "Bearer access key is required"));
    };
    let mut parts = authorization.split_ascii_whitespace();
    let (Some(scheme), Some(value), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(api_status(401, "Bearer access key is invalid"));
    };
    let access_key = *state
        .access_key
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !scheme.eq_ignore_ascii_case("Bearer") || access_key.is_unset() {
        return Err(api_status(401, "Bearer access key is invalid"));
    }
    let Ok(candidate) = AccessKey::parse(value) else {
        return Err(api_status(401, "Bearer access key is invalid"));
    };
    if !candidate.fingerprint().matches(access_key.fingerprint()) {
        return Err(api_status(401, "Bearer access key is invalid"));
    }
    Ok(ApiPrincipal::AccessKey(candidate.fingerprint()))
}

fn is_trusted_loopback_request(request: &Request) -> bool {
    const FORWARDED_HEADERS: [&str; 5] = [
        "Forwarded",
        "X-Forwarded-For",
        "X-Forwarded-Host",
        "X-Forwarded-Proto",
        "X-Real-IP",
    ];
    if FORWARDED_HEADERS
        .iter()
        .any(|header| request.header(header).is_some())
    {
        return false;
    }
    is_loopback_address(request.remote_addr().ip())
}

const fn is_loopback_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_loopback(),
        IpAddr::V6(address) => {
            if let Some(address) = address.to_ipv4_mapped() {
                return address.is_loopback();
            }
            address.is_loopback()
        }
    }
}

fn create_api_session(request: &Request, state: &ServerState, principal: ApiPrincipal) -> Response {
    if request.header("Content-Encoding") != Some("gzip") {
        return api_status(415, "create request must use gzip content encoding");
    }
    let Some(body) = request.data() else {
        return api_status(400, "missing create request body");
    };
    let mut decoded = Vec::new();
    let decoded_result = GzDecoder::new(body)
        .take(MAX_CREATE_BODY_BYTES + 1)
        .read_to_end(&mut decoded);
    if decoded_result.is_err() {
        return api_status(400, "create request body is not valid gzip");
    }
    if decoded.len() as u64 > MAX_CREATE_BODY_BYTES {
        return api_status(400, "create request body exceeds 4 MiB");
    }
    let create: CreateRequest = match serde_json::from_slice(&decoded) {
        Ok(create) => create,
        Err(error) => return api_status(400, &format!("invalid create request JSON: {error}")),
    };
    if create.offer.sdp_type != "offer" || create.offer.sdp.is_empty() {
        return api_status(400, "create request must contain a nonempty SDP offer");
    }
    let offer = match str0m::change::SdpOffer::from_sdp_string(&create.offer.sdp) {
        Ok(offer) => offer,
        Err(error) => return api_status(400, &format!("invalid SDP offer: {error}")),
    };
    let session = match state.webrtc.accept_api_offer(offer) {
        Ok(session) => session,
        Err(error) => return api_status(400, &format!("unable to accept SDP offer: {error}")),
    };
    let response = CreateResponse {
        session_id: session.id.to_string(),
        answer: ApiSdpAnswer {
            sdp_type: "answer".to_owned(),
            sdp: session.answer.to_sdp_string(),
        },
    };
    let json = match serde_json::to_vec(&response) {
        Ok(json) => json,
        Err(error) => {
            state.webrtc.close_api_session(session.id);
            return api_status(500, &format!("unable to encode create response: {error}"));
        }
    };
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    if let Err(error) = encoder.write_all(&json) {
        state.webrtc.close_api_session(session.id);
        return api_status(500, &format!("unable to compress create response: {error}"));
    }
    let compressed = match encoder.finish() {
        Ok(compressed) => compressed,
        Err(error) => {
            state.webrtc.close_api_session(session.id);
            return api_status(500, &format!("unable to compress create response: {error}"));
        }
    };
    let active_sessions = state.webrtc.active_api_session_ids();
    let mut owners = state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    owners.retain(|session_id, _| active_sessions.contains(session_id));
    owners.insert(session.id, principal);
    Response::from_data("application/json", compressed)
        .with_status_code(201)
        .with_additional_header("Content-Encoding", "gzip")
}

fn delete_api_session(request: &Request, state: &ServerState, principal: ApiPrincipal) -> Response {
    let Some(body) = request.data() else {
        return api_status(400, "missing delete request body");
    };
    let delete: DeleteRequest = match serde_json::from_reader(body) {
        Ok(delete) => delete,
        Err(error) => return api_status(400, &format!("invalid delete request JSON: {error}")),
    };
    let Ok(session_id) = delete.session_id.parse::<u64>() else {
        return api_status(404, "WebRTC session not found");
    };
    let session_id = SessionId::from_u64(session_id);
    let owns_session = {
        let mut owners = state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match owners.get(&session_id) {
            None => return Response::empty_204(),
            Some(owner) if owner != &principal => false,
            Some(_) => {
                owners.remove(&session_id);
                true
            }
        }
    };
    if !owns_session {
        return api_status(404, "WebRTC session not found");
    }
    state.webrtc.close_api_session(session_id);
    Response::empty_204()
}

fn api_status(status: u16, message: &str) -> Response {
    Response::json(&Status {
        code: i32::from(status),
        message: message.to_owned(),
        details: Vec::new(),
    })
    .with_status_code(status)
}

fn get_logging_settings(state: &ServerState) -> Result<LoggingSettings, ControlCommandError> {
    state
        .logging
        .as_ref()
        .map(LoggingService::settings)
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "logging service is unavailable",
            )
        })
}

fn set_logging_filter(
    state: &ServerState,
    filter: &str,
) -> Result<LoggingSettings, ControlCommandError> {
    let Some(logging) = &state.logging else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "logging service is unavailable",
        ));
    };
    let filter = filter.trim();
    if filter.is_empty() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "log filter must not be empty",
        ));
    }
    if let Err(error) = tracing_subscriber::EnvFilter::try_new(filter) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("invalid log filter: {error}"),
        ));
    }
    logging.update_filter(filter).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to update log filter: {error}"),
        )
    })?;
    Ok(logging.settings())
}

fn log_snapshot(state: &ServerState) -> Response {
    let Some(logging) = &state.logging else {
        return service_error(503, "logging service is unavailable");
    };
    let limit = logging.settings().buffer.max_entries;
    Response::json(&logging.snapshot(None, limit))
        .with_additional_header("Cache-Control", "no-store")
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

fn normalized_video_stream_id(value: &str) -> Option<&str> {
    match value.strip_prefix("video_").unwrap_or(value) {
        "main" => Some("main"),
        "sub" => Some("sub"),
        _ => None,
    }
}

fn expected_video_stream_ids(
    camera: &CameraEntry,
    router_status: Option<&CameraStatus>,
    streams: &[StreamHealthReport],
) -> Vec<String> {
    let mut ids = router_status
        .into_iter()
        .flat_map(|status| status.expected_streams.iter())
        .filter_map(|stream| normalized_video_stream_id(stream))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if ids.is_empty() {
        ids.extend(
            camera
                .info
                .profiles
                .iter()
                .filter_map(|profile| normalized_video_stream_id(&profile.stream))
                .map(str::to_owned),
        );
    }
    if ids.is_empty() {
        ids.extend(
            streams
                .iter()
                .filter_map(|stream| normalized_video_stream_id(&stream.report.kind))
                .map(str::to_owned),
        );
    }
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn connected_video_stream_ids(router_status: Option<&CameraStatus>) -> Option<Vec<String>> {
    let status = router_status?;
    if status.expected_streams.is_empty() && status.connected_streams.is_empty() {
        return None;
    }
    let mut ids = status
        .connected_streams
        .iter()
        .filter_map(|stream| normalized_video_stream_id(stream))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    Some(ids)
}

fn stream_transport_connected(
    stream_id: &str,
    router_status: Option<&CameraStatus>,
) -> Option<bool> {
    let status = router_status?;
    if !status.expected_streams.is_empty() || !status.connected_streams.is_empty() {
        return Some(status.connected_streams.iter().any(|connected| {
            normalized_video_stream_id(connected).is_some_and(|connected| connected == stream_id)
        }));
    }
    match status.lifecycle {
        CameraLifecycle::Connected => Some(true),
        CameraLifecycle::Reconnecting | CameraLifecycle::Stopped => Some(false),
        CameraLifecycle::Starting | CameraLifecycle::Degraded | CameraLifecycle::ShuttingDown => {
            None
        }
    }
}

fn frame_freshness_threshold_ms(expected_fps: f64) -> u64 {
    let report_floor = REPORT_INTERVAL
        .as_millis()
        .saturating_mul(2)
        .try_into()
        .unwrap_or(u64::MAX);
    if !expected_fps.is_finite() || expected_fps <= 0.0 {
        return STREAM_REPORT_FRESHNESS_THRESHOLD_MS;
    }
    let frame_window = (3_000.0 / expected_fps).ceil().max(1.0) as u64;
    report_floor.max(frame_window).min(120_000)
}

fn keyframe_freshness_threshold_ms(profile: Option<&ProfileSummary>, observed_kf_fps: f64) -> u64 {
    let configured_interval_ms = profile.and_then(|profile| {
        let gop = f64::from(profile.gop?);
        let fps = profile.framerate?;
        (fps.is_finite() && fps > 0.0).then_some(gop / fps * 1_000.0)
    });
    let observed_interval_ms =
        (observed_kf_fps.is_finite() && observed_kf_fps > 0.0).then_some(1_000.0 / observed_kf_fps);
    configured_interval_ms.or(observed_interval_ms).map_or(
        STREAM_REPORT_FRESHNESS_THRESHOLD_MS,
        |interval| {
            (interval * 3.0)
                .ceil()
                .max(STREAM_REPORT_FRESHNESS_THRESHOLD_MS as f64)
                .min(120_000.0) as u64
        },
    )
}

fn recording_freshness_threshold_ms(config: &StorageConfig) -> u64 {
    config
        .short_term_duration
        .saturating_add(config.flush_interval)
        .saturating_add(Duration::from_millis(STREAM_REPORT_FRESHNESS_THRESHOLD_MS))
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn recording_progressing(
    health: Option<&RecordingStreamHealthSnapshot>,
    threshold_ms: u64,
    uptime_ms: u64,
    frames_fresh: bool,
) -> Option<bool> {
    if health
        .and_then(|health| health.last_error.as_ref())
        .is_some()
    {
        return Some(false);
    }
    if let Some(age_ms) = health.and_then(|health| health.progress_age_ms) {
        return Some(age_ms <= threshold_ms);
    }
    if health
        .and_then(|health| health.attempt_age_ms)
        .is_some_and(|age_ms| age_ms > threshold_ms)
        || (health.is_none() && frames_fresh && uptime_ms > threshold_ms)
    {
        return Some(false);
    }
    None
}

fn bounded_health_detail(value: &str) -> String {
    value.chars().take(240).collect()
}

struct StreamProjectionContext<'a> {
    camera: &'a CameraEntry,
    expected_streams: &'a [String],
    router_status: Option<&'a CameraStatus>,
    recording_health: &'a HashMap<String, RecordingStreamHealthSnapshot>,
    recording_threshold_ms: u64,
    uptime_ms: u64,
    startup_grace: bool,
    battery_sleeping: Option<bool>,
}

fn project_stream_snapshot(
    context: &StreamProjectionContext<'_>,
    stream: StreamHealthReport,
) -> StreamHealth {
    let stream_id = normalized_video_stream_id(&stream.report.kind);
    let is_video = stream_id.is_some();
    let expected = stream_id.is_none_or(|stream_id| {
        context
            .expected_streams
            .iter()
            .any(|expected| expected == stream_id)
    });
    let transport_connected = stream_id
        .and_then(|stream_id| stream_transport_connected(stream_id, context.router_status))
        .or_else(|| {
            (!is_video).then(|| {
                context
                    .router_status
                    .is_some_and(|status| status.lifecycle == CameraLifecycle::Connected)
            })
        });
    let report_fresh = stream.report_age_ms <= STREAM_REPORT_FRESHNESS_THRESHOLD_MS;
    let frame_threshold_ms = frame_freshness_threshold_ms(stream.report.expected_fps);
    let frames_fresh = report_fresh
        && stream
            .frame_age_ms
            .is_some_and(|age_ms| age_ms <= frame_threshold_ms);
    let profile = stream_id.and_then(|stream_id| {
        context
            .camera
            .info
            .profiles
            .iter()
            .find(|profile| normalized_video_stream_id(&profile.stream) == Some(stream_id))
    });
    let keyframe_threshold_ms = keyframe_freshness_threshold_ms(profile, stream.report.kf_fps);
    let decodable = if is_video {
        report_fresh
            && stream
                .keyframe_age_ms
                .is_some_and(|age_ms| age_ms <= keyframe_threshold_ms)
    } else {
        frames_fresh
    };
    let frame_rate_healthy =
        stream.report.expected_fps <= 0.0 || stream.report.fps >= stream.report.expected_fps * 0.7;
    let recording_requested = stream_id.is_some_and(|stream_id| {
        recording_mode_includes_stream(context.camera.configuration.recording_mode, stream_id)
    });
    let recording_key =
        stream_id.map(|stream_id| format!("{}/{stream_id}", context.camera.recording_label));
    let writer_health = recording_key
        .as_ref()
        .and_then(|stream_id| context.recording_health.get(stream_id));
    let recording_progress = recording_requested
        .then(|| {
            recording_progressing(
                writer_health,
                context.recording_threshold_ms,
                context.uptime_ms,
                frames_fresh,
            )
        })
        .flatten();
    let lifecycle = match transport_connected {
        Some(true) => Some(CameraLifecycle::Connected),
        Some(false) => Some(CameraLifecycle::Reconnecting),
        None => context.router_status.map(|status| status.lifecycle),
    };
    let projection = project_camera_health(&CameraHealthEvidence {
        expected,
        lifecycle,
        startup_grace: context.startup_grace,
        report_age_ms: Some(stream.report_age_ms),
        frames_fresh: Some(frames_fresh),
        decodable: Some(decodable),
        frame_rate_healthy: Some(frame_rate_healthy),
        recent_reconnects: stream.recent_reconnects,
        recent_drops: stream.recent_drops,
        recent_errors: stream.recent_errors,
        recording_requested,
        recording_progressing: recording_progress,
        battery_sleeping: context.battery_sleeping,
    });
    let detail = projection.reason.detail().to_owned();

    StreamHealth {
        state: projection.state,
        reason: projection.reason,
        reason_codes: projection.reasons,
        detail,
        dimensions: StreamHealthDimensions {
            expected,
            transport_connected,
            report_fresh,
            report_freshness_threshold_ms: STREAM_REPORT_FRESHNESS_THRESHOLD_MS,
            frames_fresh,
            frame_freshness_threshold_ms: frame_threshold_ms,
            decodable,
            keyframe_freshness_threshold_ms: keyframe_threshold_ms,
            recent_reconnects: stream.recent_reconnects,
            recent_drops: stream.recent_drops,
            recent_errors: stream.recent_errors,
            recording_requested,
            recording_progressing: recording_progress,
            recording_progress_age_ms: writer_health.and_then(|health| health.progress_age_ms),
        },
        ingress: stream,
    }
}

struct CameraProjectionContext<'a> {
    router_status: Option<&'a CameraStatus>,
    recording_health: &'a HashMap<String, RecordingStreamHealthSnapshot>,
    battery_wake: Option<&'a BatteryWakeHandle>,
    storage_config: &'a StorageConfig,
    uptime_seconds: u64,
}

fn project_camera_snapshot(
    camera: &CameraEntry,
    info: CameraInfo,
    streams: Vec<StreamHealthReport>,
    context: &CameraProjectionContext<'_>,
) -> CameraHealth {
    let expected_streams = expected_video_stream_ids(camera, context.router_status, &streams);
    let connected_streams = connected_video_stream_ids(context.router_status);
    let uptime_ms = context.uptime_seconds.saturating_mul(1_000);
    let startup_grace = context.uptime_seconds < REPORT_INTERVAL.as_secs() * 2;
    let battery_configured = camera.battery_uid.is_some();
    let battery_source = context.battery_wake.zip(camera.battery_uid.as_deref());
    let battery_health = battery_source.map(|(wake, uid)| wake.health(uid));
    let battery_sleeping = battery_health.map(|battery| {
        battery.sleeping
            && !context
                .router_status
                .is_some_and(|status| status.lifecycle == CameraLifecycle::Connected)
    });
    let recording_threshold_ms = recording_freshness_threshold_ms(context.storage_config);
    let stream_context = StreamProjectionContext {
        camera,
        expected_streams: &expected_streams,
        router_status: context.router_status,
        recording_health: context.recording_health,
        recording_threshold_ms,
        uptime_ms,
        startup_grace,
        battery_sleeping,
    };
    let projected_streams = streams
        .into_iter()
        .map(|stream| project_stream_snapshot(&stream_context, stream))
        .collect::<Vec<_>>();
    let video_streams = projected_streams
        .iter()
        .filter(|stream| normalized_video_stream_id(&stream.ingress.report.kind).is_some())
        .collect::<Vec<_>>();
    let mut reporting_stream_ids = video_streams
        .iter()
        .filter_map(|stream| normalized_video_stream_id(&stream.ingress.report.kind))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    reporting_stream_ids.sort_unstable();
    reporting_stream_ids.dedup();
    let mut fresh_stream_ids = video_streams
        .iter()
        .filter(|stream| stream.dimensions.frames_fresh)
        .filter_map(|stream| normalized_video_stream_id(&stream.ingress.report.kind))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    fresh_stream_ids.sort_unstable();
    fresh_stream_ids.dedup();
    let mut decodable_stream_ids = video_streams
        .iter()
        .filter(|stream| stream.dimensions.decodable)
        .filter_map(|stream| normalized_video_stream_id(&stream.ingress.report.kind))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    decodable_stream_ids.sort_unstable();
    decodable_stream_ids.dedup();
    let recording_stream_ids = expected_streams
        .iter()
        .filter(|stream| {
            recording_mode_includes_stream(camera.configuration.recording_mode, stream)
        })
        .cloned()
        .collect::<Vec<_>>();
    let recording_states = recording_stream_ids
        .iter()
        .map(|stream_id| {
            let key = format!("{}/{stream_id}", camera.recording_label);
            let frames_fresh = fresh_stream_ids.iter().any(|fresh| fresh == stream_id);
            let health = context.recording_health.get(&key);
            (
                stream_id.clone(),
                recording_progressing(health, recording_threshold_ms, uptime_ms, frames_fresh),
                health,
            )
        })
        .collect::<Vec<_>>();
    let recording_progressing_stream_ids = recording_states
        .iter()
        .filter(|(_, progressing, _)| *progressing == Some(true))
        .map(|(stream_id, _, _)| stream_id.clone())
        .collect::<Vec<_>>();
    let recording_requested = !recording_stream_ids.is_empty();
    let recording_progress = if !recording_requested {
        None
    } else if recording_states
        .iter()
        .any(|(_, progressing, _)| *progressing == Some(false))
    {
        Some(false)
    } else if recording_states
        .iter()
        .all(|(_, progressing, _)| *progressing == Some(true))
    {
        Some(true)
    } else {
        None
    };
    let recording_progress_age_ms = recording_states
        .iter()
        .filter_map(|(_, _, health)| health.and_then(|health| health.progress_age_ms))
        .max();
    let reporting_video_streams = reporting_stream_ids.len();
    let fresh_video_streams = fresh_stream_ids.len();
    let decodable_video_streams = decodable_stream_ids.len();
    let expected_count = expected_streams.len();
    let frames_fresh = (expected_count > 0).then_some(fresh_video_streams == expected_count);
    let decodable = (expected_count > 0).then_some(decodable_video_streams == expected_count);
    let report_age_ms = video_streams
        .iter()
        .map(|stream| stream.ingress.report_age_ms)
        .max();
    let latest_report_at_ms = video_streams
        .iter()
        .map(|stream| stream.ingress.updated_at_ms)
        .max();
    let frame_rate_healthy = (expected_count > 0).then_some(
        video_streams.len() == expected_count
            && video_streams.iter().all(|stream| {
                stream.ingress.report.expected_fps <= 0.0
                    || stream.ingress.report.fps >= stream.ingress.report.expected_fps * 0.7
            }),
    );
    let recent_reconnects = video_streams
        .iter()
        .map(|stream| stream.ingress.recent_reconnects)
        .sum();
    let recent_drops = video_streams
        .iter()
        .map(|stream| stream.ingress.recent_drops)
        .sum();
    let recent_errors = video_streams
        .iter()
        .map(|stream| stream.ingress.recent_errors)
        .sum();
    let expected = !matches!(
        context.router_status.map(|status| status.lifecycle),
        Some(CameraLifecycle::Stopped | CameraLifecycle::ShuttingDown)
    );
    let projection = project_camera_health(&CameraHealthEvidence {
        expected,
        lifecycle: context.router_status.map(|status| status.lifecycle),
        startup_grace,
        report_age_ms,
        frames_fresh,
        decodable,
        frame_rate_healthy,
        recent_reconnects,
        recent_drops,
        recent_errors,
        recording_requested,
        recording_progressing: recording_progress,
        battery_sleeping,
    });
    let recording_error = recording_states
        .iter()
        .find_map(|(_, _, health)| health.and_then(|health| health.last_error.as_deref()));
    let last_error = context
        .router_status
        .and_then(|status| status.last_error.as_deref())
        .or(recording_error)
        .map(bounded_health_detail);
    let detail = last_error
        .clone()
        .unwrap_or_else(|| projection.reason.detail().to_owned());

    CameraHealth {
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
        state: projection.state,
        reason: projection.reason,
        reason_codes: projection.reasons,
        detail,
        dimensions: CameraHealthDimensions {
            configured: true,
            expected,
            configured_video_streams: expected_count,
            connected_video_streams: connected_streams.as_ref().map(Vec::len),
            reporting_video_streams,
            fresh_video_streams,
            decodable_video_streams,
            configured_video_stream_ids: expected_streams,
            connected_video_stream_ids: connected_streams,
            reporting_video_stream_ids: reporting_stream_ids,
            fresh_video_stream_ids: fresh_stream_ids,
            decodable_video_stream_ids: decodable_stream_ids,
            transport_connected: context
                .router_status
                .map(|status| status.lifecycle == CameraLifecycle::Connected),
            latest_report_at_ms,
            report_age_ms,
            frames_fresh,
            decodable,
            recent_reconnects,
            recent_drops,
            recent_errors,
            recording_requested,
            recording_video_streams: recording_stream_ids.len(),
            recording_streams_progressing: recording_progressing_stream_ids.len(),
            recording_video_stream_ids: recording_stream_ids,
            recording_progressing_stream_ids,
            recording_progressing: recording_progress,
            recording_progress_age_ms,
            battery_configured,
            battery_registered: battery_health.map(|health| health.registered),
            battery_last_seen_age_ms: battery_health.and_then(|health| health.last_seen_age_ms),
            battery_wake_pending_age_ms: battery_health
                .and_then(|health| health.wake_pending_age_ms),
            battery_sleeping,
        },
        lifecycle: context
            .router_status
            .map(|status| format!("{:?}", status.lifecycle).to_lowercase()),
        last_error,
        configured_profiles: camera.info.profiles.clone(),
        streams: projected_streams,
    }
}

fn server_health_status(issues: &[HealthIssue]) -> &'static str {
    if issues
        .iter()
        .any(|issue| matches!(issue.severity.as_str(), "critical" | "warning"))
    {
        "degraded"
    } else {
        "healthy"
    }
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
    let recording_health = state
        .recording_health
        .snapshot()
        .streams
        .into_iter()
        .map(|stream| (stream.stream_id.clone(), stream))
        .collect::<HashMap<_, _>>();
    let configured_cameras = state.camera_entries();
    let mut totals = HealthTotals {
        configured_cameras: configured_cameras.len(),
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
            if stream.recent_drops > 0 || stream.recent_errors > 0 {
                issues.push(HealthIssue {
                    severity: "info".to_owned(),
                    scope: camera
                        .info
                        .name
                        .clone()
                        .unwrap_or_else(|| camera.info.ip.clone()),
                    message: format!(
                        "{} recent drops {}, errors {}",
                        stream.report.kind, stream.recent_drops, stream.recent_errors
                    ),
                });
            }
        }

        let router_status = lifecycle.get(&camera.recording_label);
        let projection_context = CameraProjectionContext {
            router_status,
            recording_health: &recording_health,
            battery_wake: state.battery_wake.as_ref(),
            storage_config: &state.storage_config,
            uptime_seconds,
        };
        let camera_health = project_camera_snapshot(camera, info, streams, &projection_context);
        let dimensions = &camera_health.dimensions;
        totals.configured_video_streams += dimensions.configured_video_streams;
        totals.connected_video_streams += dimensions.connected_video_streams.unwrap_or(0);
        totals.fresh_video_streams += dimensions.fresh_video_streams;
        totals.decodable_video_streams += dimensions.decodable_video_streams;
        totals.recording_requested_video_streams += dimensions.recording_video_streams;
        totals.recording_video_streams += dimensions.recording_streams_progressing;
        totals.connected_cameras += usize::from(dimensions.transport_connected == Some(true));
        totals.fresh_cameras += usize::from(dimensions.frames_fresh == Some(true));
        totals.decodable_cameras += usize::from(dimensions.decodable == Some(true));
        totals.recording_requested_cameras += usize::from(dimensions.recording_requested);
        totals.recording_cameras += usize::from(dimensions.recording_progressing == Some(true));
        totals.unknown_cameras += usize::from(camera_health.state == CameraHealthState::Unknown);
        if !matches!(
            camera_health.state,
            CameraHealthState::Healthy | CameraHealthState::Starting | CameraHealthState::Stopped
        ) {
            issues.push(HealthIssue {
                severity: "warning".to_owned(),
                scope: camera_health.id.clone(),
                message: camera_health.detail.clone(),
            });
        }
        cameras.push(camera_health);
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
        medium_term_path: state.config.storage.medium_term_path.clone(),
        long_term_path: state.config.storage.long_term_path.clone(),
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
    let status = server_health_status(&issues);

    ServerHealthResponse {
        status: status.to_owned(),
        health_contract_version: CAMERA_HEALTH_CONTRACT_VERSION,
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

fn prometheus_metrics(router_tx: &FacadeSender<RouterMessage>, state: &ServerState) -> Response {
    match crate::metrics::encode_health(&server_health(router_tx, state)) {
        Ok(metrics) => Response::from_data(
            "text/plain; version=0.0.4; charset=utf-8",
            metrics.into_bytes(),
        ),
        Err(error) => {
            tracing::error!(%error, "unable to encode Prometheus metrics");
            api_status(503, "Prometheus metrics are unavailable")
        }
    }
}

fn camera_settings(
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> Vec<CameraSettings> {
    let entries = state.camera_entries();
    let health = server_health(router_tx, state)
        .cameras
        .into_iter()
        .map(|camera| (camera.id, camera.state.as_str().to_owned()))
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
                            state.camera_config_path.as_deref(),
                        )
                    },
                    |camera| {
                        camera_settings_entry(
                            config,
                            Some(camera),
                            health.get(&camera.info.id).cloned(),
                            state.camera_config_path.as_deref(),
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
    config_path: Option<&Path>,
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
        main_rtsp_url: camera_setting_for_output(
            config_path,
            configuration,
            "main_rtsp_url",
            configuration.main_rtsp_url.as_deref(),
        ),
        sub_rtsp_url: camera_setting_for_output(
            config_path,
            configuration,
            "sub_rtsp_url",
            configuration.sub_rtsp_url.as_deref(),
        ),
        uid_configured: configuration.uid.is_some(),
        backend: camera_backend_name(configuration.backend).to_owned(),
        transport: camera_transport_name(configuration.transport).to_owned(),
        record_generic_motion_events: configuration.record_generic_motion_events,
        recording_mode: match configuration.recording_mode {
            CameraRecordingMode::Off => "off",
            CameraRecordingMode::Sub => "sub",
            CameraRecordingMode::Main => "main",
            CameraRecordingMode::Both => "both",
            CameraRecordingMode::EventBoost => "event-boost",
        }
        .to_owned(),
        event_recording_duration_secs: configuration.event_recording_duration_secs,
        health,
        model: camera.and_then(|camera| camera.info.model.clone()),
    }
}

fn camera_setting_for_output(
    config_path: Option<&Path>,
    configuration: &CameraConfig,
    key: &str,
    resolved: Option<&str>,
) -> Option<String> {
    let resolved = resolved?;
    config_path
        .and_then(|path| {
            config::camera_reference_or_value(path, configuration.ip, key, resolved).ok()
        })
        .or_else(|| Some(resolved.to_owned()))
}

fn discover_camera_settings(
    networks: Vec<ipnet::Ipv4Net>,
    mut subnets: Vec<u8>,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
    task_key: Option<&CameraDiscoveryTaskKey>,
) -> Result<Vec<DiscoveredCameraSettings>, ControlCommandError> {
    subnets.sort_unstable();
    subnets.dedup();
    if subnets.len() > 32 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "at most 32 additional subnets may be scanned at once",
        ));
    }
    let discovered = match if networks.is_empty() {
        crate::cameras::discover(Some(Duration::from_secs(5)), &subnets)
    } else {
        let cancelled = task_key
            .and_then(|key| {
                state
                    .camera_discovery_tasks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(key)
                    .map(|task| task.cancelled.clone())
            })
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        crate::cameras::discover_on_networks_with_progress(
            Some(Duration::from_secs(5)),
            &networks,
            &cancelled,
            |cameras| {
                let Some(key) = task_key else {
                    return;
                };
                if let Some(task) = state
                    .camera_discovery_tasks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_mut(key)
                {
                    task.cameras = cameras.to_vec();
                }
            },
        )
    } {
        Ok(discovered) => discovered,
        Err(error) => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                502,
                format!("camera discovery failed: {error}"),
            ));
        }
    };
    Ok(present_discovered_cameras(discovered, router_tx, state))
}

fn present_discovered_cameras(
    discovered: Vec<crate::cameras::DiscoveredCamera>,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> Vec<DiscoveredCameraSettings> {
    let health = server_health(router_tx, state)
        .cameras
        .into_iter()
        .map(|camera| (camera.ip, camera.state.as_str().to_owned()))
        .collect::<HashMap<_, _>>();
    let configured = state
        .camera_entries()
        .into_iter()
        .map(|camera| camera.info.ip)
        .collect::<HashSet<_>>();
    discovered
        .into_iter()
        .map(|camera| {
            let catalog = camera.model.as_deref().and_then(|model| {
                let database = state.camera_database.as_deref()?;
                match database.match_camera(camera.brand, model) {
                    CameraMatch::Exact(catalog_camera) => {
                        let catalog_camera = *catalog_camera;
                        Some(DiscoveredCameraCatalog {
                            stream_hints: database.stream_hints(&catalog_camera.id, camera.ip),
                            camera: catalog_camera,
                        })
                    }
                    CameraMatch::Ambiguous | CameraMatch::Missing => None,
                }
            });
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
                catalog,
            }
        })
        .collect::<Vec<_>>()
}

fn save_runtime_settings(
    update: RuntimeSettingsUpdate,
    state: &ServerState,
) -> Result<RuntimeSettingsUpdateResponse, ControlCommandError> {
    let Some(config_path) = &state.camera_config_path else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "settings persistence is unavailable",
        ));
    };
    let resolve = |field: &str, value: &str| {
        config::resolve_secret_references(config_path, value).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("{field} secret reference is invalid: {error}"),
            )
        })
    };
    let resolved_host = resolve("host", &update.host)?;
    let Some(host) = normalize_server_host(&resolved_host) else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "host must be a nonempty address or hostname",
        ));
    };
    if update.port == 0 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "server port must be between 1 and 65535",
        ));
    }
    if server_bind_address(&host, update.port)
        .to_socket_addrs()
        .is_err()
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "host must resolve to an address",
        ));
    }
    if update.storage.write_buffer_bytes == 0
        || update.storage.write_buffer_bytes > MAX_WRITE_BUFFER_BYTES
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("write buffer size must be between 1 and {MAX_WRITE_BUFFER_BYTES} bytes"),
        ));
    }
    if update.storage.long_term_max_gb > u64::MAX / GIBIBYTE_BYTES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "long-term storage limit is too large",
        ));
    }
    if update.storage.event_thumbnail_max_mb > u64::MAX / MEBIBYTE_BYTES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event thumbnail storage limit is too large",
        ));
    }
    let medium_term_path_value =
        resolve("medium-term storage path", &update.storage.medium_term_path)?;
    let Some(medium_term_path) = normalize_storage_path(&medium_term_path_value) else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "medium-term storage path must be nonempty and cannot contain NUL",
        ));
    };
    let long_term_path_value = resolve("long-term storage path", &update.storage.long_term_path)?;
    let Some(long_term_path) = normalize_storage_path(&long_term_path_value) else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "long-term storage path must be nonempty and cannot contain NUL",
        ));
    };
    let recording_catalog_path_value = resolve(
        "recording metadata database path",
        &update.storage.recording_catalog_path,
    )?;
    let Some(recording_catalog_path) = normalize_storage_path(&recording_catalog_path_value) else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "recording metadata database path must be nonempty and cannot contain NUL",
        ));
    };
    let event_thumbnail_path_value = resolve(
        "event thumbnail storage path",
        &update.storage.event_thumbnail_path,
    )?;
    let Some(event_thumbnail_path) = normalize_storage_path(&event_thumbnail_path_value) else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event thumbnail storage path must be nonempty and cannot contain NUL",
        ));
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
        ..Config::default()
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
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    format!("invalid storage migration: {error}"),
                ));
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
            Err(error) => {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::Internal,
                    500,
                    format!("unable to save settings: {error}"),
                ));
            }
        };
    let camera_count = config::load_cameras(config_path)
        .map(|cameras| cameras.values().map(Vec::len).sum())
        .unwrap_or(state.config.camera_count);
    let storage = StorageConfig::from_toml(&saved.storage);
    Ok(RuntimeSettingsUpdateResponse {
        config: sanitized_config(&saved, &storage, camera_count, &state.camera_entries()),
        restart_required: true,
    })
}

fn save_camera_settings(
    mut update: CameraSettingsUpdate,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
    camera_id: &str,
) -> Result<CameraSettingsUpdateResponse, ControlCommandError> {
    let Ok(ip) = camera_id.parse::<IpAddr>() else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "camera ID must be an IP address",
        ));
    };
    if update.onvif_port == Some(Some(0)) || update.http_port == Some(Some(0)) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "camera ports must be between 1 and 65535",
        ));
    }
    let Some(config_path) = &state.camera_config_path else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "camera configuration persistence is unavailable",
        ));
    };
    let credential_defaults = config::load_camera_defaults(config_path).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to load camera credential defaults: {error}"),
        )
    })?;
    let submitted_username = update.username.clone();
    let submitted_password = update.password.clone();
    let submitted_main_rtsp_url = update.main_rtsp_url.clone().flatten();
    let submitted_sub_rtsp_url = update.sub_rtsp_url.clone().flatten();
    let submitted_uid = update.uid.clone().flatten();
    update.username = update
        .username
        .map(|value| resolve_setting_secret(config_path, "username", &value))
        .transpose()?;
    update.password = update
        .password
        .map(|value| resolve_setting_secret(config_path, "password", &value))
        .transpose()?;
    update.main_rtsp_url =
        resolve_optional_setting_secret(config_path, "main RTSP URL", update.main_rtsp_url)?;
    update.sub_rtsp_url =
        resolve_optional_setting_secret(config_path, "sub RTSP URL", update.sub_rtsp_url)?;
    update.uid = resolve_optional_setting_secret(config_path, "UID", update.uid)?;
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
            return Err(ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to load camera configuration: {error}"),
            ));
        }
    };
    let existing = state.camera(camera_id);
    let existing_config = existing
        .as_ref()
        .map(|camera| camera.configuration.clone())
        .or(persisted);
    let is_new_camera = existing_config.is_none();
    let username = nonempty_setting(update.username)
        .or_else(|| {
            existing_config
                .as_ref()
                .map(|camera| camera.username.clone())
        })
        .or_else(|| nonempty_setting(Some(credential_defaults.username.clone())));
    let password = nonempty_setting(update.password)
        .or_else(|| {
            existing_config
                .as_ref()
                .map(|camera| camera.password.clone())
        })
        .or_else(|| nonempty_setting(Some(credential_defaults.password.clone())));
    let (Some(username), Some(password)) = (username, password) else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "username and password are required for a new camera",
        ));
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
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "manufacturer must be at most 120 printable characters",
                ));
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
            Err(error) => {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    format!("main RTSP URL {error}"),
                ));
            }
        },
        None => existing_config
            .as_ref()
            .and_then(|camera| camera.main_rtsp_url.clone()),
    };
    let sub_rtsp_url = match update.sub_rtsp_url {
        Some(url) => match normalize_rtsp_url(url) {
            Ok(url) => url,
            Err(error) => {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    format!("sub RTSP URL {error}"),
                ));
            }
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
        record_generic_motion_events: update
            .record_generic_motion_events
            .or_else(|| {
                existing_config
                    .as_ref()
                    .map(|camera| camera.record_generic_motion_events)
            })
            .unwrap_or_default(),
        recording_mode: update
            .recording_mode
            .or_else(|| existing_config.as_ref().map(|camera| camera.recording_mode))
            .unwrap_or_default(),
        event_recording_duration_secs: update
            .event_recording_duration_secs
            .or_else(|| {
                existing_config
                    .as_ref()
                    .map(|camera| camera.event_recording_duration_secs)
            })
            .unwrap_or(60),
    };
    if config.event_recording_duration_secs == 0 || config.event_recording_duration_secs > 3_600 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event recording duration must be between 1 and 3600 seconds",
        ));
    }
    let mut persisted_config = config.clone();
    for (submitted, persisted) in [
        (submitted_username, &mut persisted_config.username),
        (submitted_password, &mut persisted_config.password),
    ] {
        if let Some(reference) = submitted.filter(|value| config::contains_secret_reference(value))
        {
            *persisted = reference;
        }
    }
    for (submitted, persisted) in [
        (submitted_main_rtsp_url, &mut persisted_config.main_rtsp_url),
        (submitted_sub_rtsp_url, &mut persisted_config.sub_rtsp_url),
        (submitted_uid, &mut persisted_config.uid),
    ] {
        if let Some(reference) = submitted.filter(|value| config::contains_secret_reference(value))
        {
            *persisted = Some(reference);
        }
    }
    let persisted_name =
        config::upsert_camera(config_path, &persisted_config).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to save camera configuration: {error}"),
            )
        })?;
    config.name = Some(persisted_name);
    let started_config = start_runtime_camera(state, &config, !is_new_camera);
    let dynamically_started = started_config.is_some();
    if let Some(started_config) = started_config {
        config = started_config;
    }
    let health = server_health(router_tx, state)
        .cameras
        .into_iter()
        .find(|camera| camera.ip == config.ip.to_string())
        .map(|camera| camera.state.as_str().to_owned());
    let health = if dynamically_started && health.as_deref().is_none_or(|state| state == "offline")
    {
        Some("starting".to_owned())
    } else {
        health
    };
    let camera = CameraSettings {
        id: config.ip.to_string(),
        ip: config.ip.to_string(),
        display_name: config.display_name.clone(),
        manufacturer_override: config.manufacturer_override().map(str::to_owned),
        username_configured: true,
        password_configured: true,
        onvif_port: config.onvif_port,
        http_port: config.http_port,
        main_rtsp_url: camera_setting_for_output(
            Some(config_path),
            &config,
            "main_rtsp_url",
            config.main_rtsp_url.as_deref(),
        ),
        sub_rtsp_url: camera_setting_for_output(
            Some(config_path),
            &config,
            "sub_rtsp_url",
            config.sub_rtsp_url.as_deref(),
        ),
        uid_configured: config.uid.is_some(),
        backend: camera_backend_name(config.backend).to_owned(),
        transport: camera_transport_name(config.transport).to_owned(),
        record_generic_motion_events: config.record_generic_motion_events,
        recording_mode: match config.recording_mode {
            CameraRecordingMode::Off => "off",
            CameraRecordingMode::Sub => "sub",
            CameraRecordingMode::Main => "main",
            CameraRecordingMode::Both => "both",
            CameraRecordingMode::EventBoost => "event-boost",
        }
        .to_owned(),
        event_recording_duration_secs: config.event_recording_duration_secs,
        health,
        model: existing
            .as_ref()
            .and_then(|camera| camera.info.model.clone()),
    };
    Ok(CameraSettingsUpdateResponse {
        camera,
        restart_required: !dynamically_started,
    })
}

fn resolve_optional_setting_secret(
    config_path: &Path,
    field: &str,
    value: Option<Option<String>>,
) -> Result<Option<Option<String>>, ControlCommandError> {
    value
        .map(|value| {
            value
                .map(|value| resolve_setting_secret(config_path, field, &value))
                .transpose()
        })
        .transpose()
}

fn resolve_setting_secret(
    config_path: &Path,
    field: &str,
    value: &str,
) -> Result<String, ControlCommandError> {
    config::resolve_secret_references(config_path, value).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{field} secret reference is invalid: {error}"),
        )
    })
}

fn start_runtime_camera(
    state: &ServerState,
    config: &CameraConfig,
    restart: bool,
) -> Option<CameraConfig> {
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
    let runtime_result = if restart {
        runtime.restart_camera(camera.clone())
    } else {
        runtime.start_camera(camera.clone())
    };
    if let Err(error) = runtime_result {
        tracing::warn!(ip = %config.ip, %error, "camera configuration could not be applied live");
        return None;
    }
    state.upsert_camera(camera_entry(&camera.config, Some(&camera)));
    Some(camera.config)
}

fn delete_camera_settings(state: &ServerState, camera_id: &str) -> Result<(), ControlCommandError> {
    let Ok(ip) = camera_id.parse::<IpAddr>() else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "camera ID must be an IP address",
        ));
    };
    let Some(config_path) = &state.camera_config_path else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "camera configuration persistence is unavailable",
        ));
    };
    let _config_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match config::remove_camera(config_path, ip) {
        Ok(()) => Ok(()),
        Err(error) => Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to remove camera configuration: {error}"),
        )),
    }
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

fn set_camera_manufacturer(
    state: &ServerState,
    camera_id: &str,
    manufacturer: Option<String>,
) -> Result<CameraInfo, ControlCommandError> {
    let Some(camera) = state.camera(camera_id) else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "camera not found",
        ));
    };
    let manufacturer = match manufacturer {
        Some(manufacturer) => {
            let normalized = normalize_manufacturer(&manufacturer);
            if normalized.is_none() && !manufacturer.trim().is_empty() {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "manufacturer must be at most 120 printable characters",
                ));
            }
            normalized
        }
        None => None,
    };
    let Some(config_path) = &state.camera_config_path else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "camera configuration persistence is unavailable",
        ));
    };
    let Ok(camera_ip) = camera.info.ip.parse::<IpAddr>() else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "camera has an invalid IP address",
        ));
    };

    let _config_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(error) =
        config::set_camera_manufacturer(config_path, camera_ip, manufacturer.as_deref())
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to save manufacturer override: {error}"),
        ));
    }
    {
        let mut overrides = state
            .manufacturer_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &manufacturer {
            Some(manufacturer) => {
                overrides.insert(camera.info.id.clone(), manufacturer.clone());
            }
            None => {
                overrides.remove(&camera.info.id);
            }
        }
    }

    Ok(state.camera_info(&camera))
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

fn set_camera_motion(
    state: &ServerState,
    camera_id: &str,
    enabled: bool,
) -> Result<MotionDetection, ControlCommandError> {
    let Some(camera) = state.camera(camera_id) else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "camera not found",
        ));
    };
    let Some(control) = &camera.control else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "motion detection control is unavailable for this camera",
        ));
    };
    let mut client = ReolinkClient::new_with_http_port(control.ip, control.http_port);
    if let Err(error) = client.login(&control.username, &control.password) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            format!("camera motion login failed: {error}"),
        ));
    }
    if let Err(error) = client.set_alarm(0, enabled) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            format!("camera motion update failed: {error}"),
        ));
    }
    match client.get_md_state(0) {
        Ok(actual) if actual == enabled => Ok(MotionDetection {
            supported: true,
            controllable: true,
            enabled: Some(actual),
            error: None,
        }),
        Ok(actual) => Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            502,
            format!("camera motion state was {actual} after requesting {enabled}"),
        )),
        Err(error) => Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            format!("camera motion verification failed: {error}"),
        )),
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

fn serve_ui(request: &Request) -> Response {
    if request.method() != "GET" {
        return service_error(404, "not found");
    }

    let path = request.url();
    UI_ASSETS
        .get_file(path.trim_start_matches('/'))
        .map_or_else(
            || {
                embedded_ui_file(
                    UI_ASSETS
                        .get_file("index.html")
                        .expect("the compiled UI must include index.html"),
                )
                .with_no_cache()
            },
            |file| embedded_ui_file(file).with_public_cache(3_600),
        )
}

fn embedded_ui_file(file: &EmbeddedFile<'_>) -> Response {
    let content_type = file
        .path()
        .extension()
        .and_then(|extension| extension.to_str())
        .map_or("application/octet-stream", rouille::extension_to_mime);
    Response::from_data(content_type, file.contents())
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

fn service_error(status: u16, message: &str) -> Response {
    Response::json(&ApiError {
        error: message.to_owned(),
    })
    .with_status_code(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::CameraHealthReason;

    #[test]
    fn indexed_video_format_reports_coded_dimensions() {
        let initialization = std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/test-camera/testdata/cc-4k-640x360-h264.mp4"),
        )
        .unwrap();

        let format = indexed_video_format(&initialization, None).unwrap();

        assert_eq!((format.decoder.width, format.decoder.height), (640, 368));

        let h265 = std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/test-camera/testdata/cc-4k-640x360-h265.mp4"),
        )
        .unwrap();
        let h265_format = indexed_video_format(&h265, None).unwrap();
        assert_eq!(h265_format.decoder.codec, "hvc1.1.6.L63.90");
        assert_eq!(
            h265_format.mp4_content_type,
            "video/mp4; codecs=\"hvc1.1.6.L63.90\""
        );
        assert_eq!(
            (h265_format.decoder.width, h265_format.decoder.height),
            (640, 360)
        );
    }

    #[test]
    fn indexed_video_format_follows_the_fragment_codec_description() {
        fn fixture(name: &str, media_type: mp4::MediaType) -> (mp4::MediaConfig, mp4::Mp4Sample) {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/test-camera/testdata")
                .join(name);
            let mut reader = mp4::read_mp4(std::fs::File::open(path).unwrap()).unwrap();
            let (&track_id, track) = reader
                .tracks()
                .iter()
                .find(|(_, track)| track.media_type().ok() == Some(media_type))
                .unwrap();
            let config = track.media_config_for_description(1).unwrap();
            let mut sample = (1..=track.sample_count())
                .find_map(|sample_id| {
                    let sample = reader.read_sample(track_id, sample_id).unwrap().unwrap();
                    sample.is_sync.then_some(sample)
                })
                .unwrap();
            sample.start_time = 0;
            sample.duration = 3_000;
            (config, sample)
        }

        let (h264, h264_sample) = fixture("cc-4k-640x360-h264.mp4", mp4::MediaType::H264);
        let (h265, mut h265_sample) = fixture("cc-4k-640x360-h265.mp4", mp4::MediaType::H265);
        h265_sample.start_time = 9_000;
        let config = mp4::Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
            timescale: 1_000,
        };
        let track = mp4::FragmentedTrackConfig {
            track_type: mp4::TrackType::Video,
            timescale: 90_000,
            language: "und".to_owned(),
            sample_descriptions: vec![h264, h265],
        };
        let mut writer = mp4::FragmentedMp4Writer::write_start_with_sample_descriptions(
            Cursor::new(Vec::new()),
            &config,
            &[track],
        )
        .unwrap();
        let initialization = writer.initialization();
        writer
            .write_sample_with_description(1, 1, h264_sample)
            .unwrap();
        writer.flush_fragment().unwrap();
        writer
            .write_sample_with_description(1, 2, h265_sample)
            .unwrap();
        let h265_fragment = writer.write_end().unwrap().unwrap();
        let buffer = writer.into_writer().into_inner();
        let initialization = &buffer[initialization.offset as usize
            ..(initialization.offset + initialization.size) as usize];
        let fragment = &buffer[h265_fragment.range.offset as usize
            ..(h265_fragment.range.offset + h265_fragment.range.size) as usize];

        let content_type = fragmented_mp4_content_type(initialization).unwrap();
        assert!(content_type.contains("avc1"));
        assert!(content_type.contains("hev1"));
        let format = indexed_video_format(initialization, Some(fragment)).unwrap();
        assert!(format.decoder.codec.starts_with("hev1"));
        assert_eq!((format.decoder.width, format.decoder.height), (640, 360));
        assert_eq!(format.keyframe_content_type, "video/h265; format=hvcc");
    }

    #[test]
    fn stored_media_periods_normalize_h264_h265_h264_archive_gops() {
        fn fixture(name: &str, media_type: mp4::MediaType) -> (mp4::MediaConfig, mp4::Mp4Sample) {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/test-camera/testdata")
                .join(name);
            let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
            let (&track_id, track) = reader
                .tracks()
                .iter()
                .find(|(_, track)| track.media_type().ok() == Some(media_type))
                .unwrap();
            let config = track.media_config_for_description(1).unwrap();
            let mut sample = (1..=track.sample_count())
                .find_map(|sample_id| {
                    let sample = reader.read_sample(track_id, sample_id).unwrap().unwrap();
                    sample.is_sync.then_some(sample)
                })
                .unwrap();
            sample.start_time = 0;
            sample.duration = 90_000;
            (config, sample)
        }

        let (h264, mut h264_sample) = fixture("cc-4k-640x360-h264.mp4", mp4::MediaType::H264);
        let (h265, mut h265_sample) = fixture("cc-4k-640x360-h265.mp4", mp4::MediaType::H265);
        let track = mp4::FragmentedTrackConfig {
            track_type: mp4::TrackType::Video,
            timescale: 90_000,
            language: "und".to_owned(),
            sample_descriptions: vec![h264, h265],
        };
        let mut writer = mp4::FragmentedMp4Writer::write_start_with_sample_descriptions(
            Cursor::new(Vec::new()),
            &mp4::Mp4Config {
                major_brand: "iso6".parse().unwrap(),
                minor_version: 1,
                compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
                timescale: 1_000,
            },
            &[track],
        )
        .unwrap();
        let initialization = writer.initialization();
        writer
            .write_sample_with_description(1, 1, h264_sample.clone())
            .unwrap();
        let first = writer.flush_fragment().unwrap().unwrap();
        h265_sample.start_time = 90_000;
        writer
            .write_sample_with_description(1, 2, h265_sample)
            .unwrap();
        let second = writer.flush_fragment().unwrap().unwrap();
        h264_sample.start_time = 180_000;
        writer
            .write_sample_with_description(1, 1, h264_sample)
            .unwrap();
        let third = writer.write_end().unwrap().unwrap();
        let bytes = writer.into_writer().into_inner();
        let initialization = &bytes[initialization.offset as usize
            ..(initialization.offset + initialization.size) as usize];
        let periods = [first, second, third]
            .into_iter()
            .map(|fragment| {
                stored_media_period(
                    initialization,
                    &bytes[fragment.range.offset as usize
                        ..(fragment.range.offset + fragment.range.size) as usize],
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            periods
                .iter()
                .map(|period| period.sample_descriptions.as_slice())
                .collect::<Vec<_>>(),
            vec![&[1][..], &[2][..], &[1][..]]
        );
        assert!(periods[0].content_type.contains("avc1"));
        assert!(periods[1].content_type.contains("hev1"));
        assert!(periods[2].content_type.contains("avc1"));
        for period in periods {
            let mut media = period.initialization;
            media.extend_from_slice(&period.fragment);
            let reader =
                mp4::Mp4Reader::read_header(Cursor::new(media.clone()), media.len() as u64)
                    .unwrap();
            let video = reader.tracks()[&1].sample_description_count();
            assert_eq!(video, 1);
            assert_eq!(reader.tracks()[&1].sample_description_index(1).unwrap(), 1);
        }
    }

    #[test]
    fn stored_media_batch_allows_codec_changes_between_recordings() {
        fn write_source(
            fixture_name: &str,
            media_type: mp4::MediaType,
            path: &Path,
            recording_id: &str,
            start_ms: i64,
        ) -> CatalogMediaFragment {
            let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/test-camera/testdata")
                .join(fixture_name);
            let mut reader = mp4::read_mp4(File::open(fixture).unwrap()).unwrap();
            let (&track_id, track) = reader
                .tracks()
                .iter()
                .find(|(_, track)| track.media_type().ok() == Some(media_type))
                .unwrap();
            let config = track.media_config_for_description(1).unwrap();
            let mut sample = (1..=track.sample_count())
                .find_map(|sample_id| {
                    let sample = reader.read_sample(track_id, sample_id).unwrap().unwrap();
                    sample.is_sync.then_some(sample)
                })
                .unwrap();
            sample.start_time = 0;
            sample.duration = 90_000;
            let track = mp4::TrackConfig {
                track_type: mp4::TrackType::Video,
                timescale: 90_000,
                language: "und".to_owned(),
                media_conf: config,
            };
            let mut writer = mp4::FragmentedMp4Writer::write_start(
                File::create(path).unwrap(),
                &mp4::Mp4Config {
                    major_brand: "iso6".parse().unwrap(),
                    minor_version: 1,
                    compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
                    timescale: 1_000,
                },
                &[track],
            )
            .unwrap();
            let initialization = writer.initialization();
            writer.write_sample(1, sample).unwrap();
            let fragment = writer.write_end().unwrap().unwrap();
            CatalogMediaFragment {
                recording_id: recording_id.to_owned(),
                recording_started_at_ms: start_ms,
                path: path.to_string_lossy().into_owned(),
                init_offset: initialization.offset,
                init_len: initialization.size,
                sequence: 1,
                start_ms,
                duration_ms: 1_000,
                byte_offset: fragment.range.offset,
                byte_len: fragment.range.size,
            }
        }

        let directory =
            std::env::temp_dir().join(format!("keeppeek-stored-periods-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&directory).unwrap();
        let fragments = vec![
            write_source(
                "cc-4k-640x360-h264.mp4",
                mp4::MediaType::H264,
                &directory.join("h264.mp4"),
                "h264",
                0,
            ),
            write_source(
                "cc-4k-640x360-h265.mp4",
                mp4::MediaType::H265,
                &directory.join("h265.mp4"),
                "h265",
                1_000,
            ),
        ];

        let batch = encode_stored_media_fragments(
            "mixed-recordings",
            1,
            0,
            DataChannelTarget::Reliable,
            fragments,
        )
        .unwrap();
        let content_types = batch
            .messages
            .iter()
            .filter_map(|message| match message.message.message.as_ref() {
                Some(proto::message::Message::StoredMedia(stored)) => {
                    match stored.message.as_ref() {
                        Some(proto::stored_media_message::Message::Initialization(init)) => {
                            Some(init.content_type.as_str())
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        assert_eq!(content_types.len(), 2);
        assert!(
            content_types
                .iter()
                .any(|content_type| content_type.contains("avc1"))
        );
        assert!(
            content_types
                .iter()
                .any(|content_type| content_type.contains("hev1"))
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    use crate::logging::LogFilterFile;
    use crate::storage::RecordingCatalog;
    use crate::test_support::TestCameraCatalog;
    use std::{io, net::SocketAddr};
    fn response_data(response: Response) -> Vec<u8> {
        let (mut reader, _) = response.data.into_reader_and_size();
        let mut data = Vec::new();
        reader.read_to_end(&mut data).unwrap();
        data
    }

    fn gzip_json(value: &impl Serialize) -> Vec<u8> {
        let json = serde_json::to_vec(value).unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&json).unwrap();
        encoder.finish().unwrap()
    }

    fn test_control_handler(state: ServerState) -> ServerControlHandler {
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        ServerControlHandler::new(state, router_tx)
    }

    fn secured_test_state() -> ServerState {
        let mut state = ServerState::empty();
        state.access_key = Arc::new(RwLock::new(
            AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap(),
        ));
        state.allowed_origins = Arc::new(HashSet::from(["https://home.example.net".to_owned()]));
        state
    }

    fn bearer_header() -> (String, String) {
        (
            "Authorization".to_owned(),
            "Bearer 550e8400-e29b-41d4-a716-446655440000".to_owned(),
        )
    }

    fn media_test_state() -> ServerState {
        let config = CameraConfig {
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            name: Some("front-door".to_owned()),
            display_name: Some("Front Door".to_owned()),
            manufacturer: None,
            username: String::new(),
            password: String::new(),
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
        };
        let mut state = ServerState::empty();
        state.cameras = Arc::new(RwLock::new(vec![camera_entry(&config, None)]));
        state
    }

    #[test]
    fn connected_stream_identities_remain_unknown_when_router_omits_them() {
        let aggregate_only = CameraStatus {
            id: crate::api::CameraId::new("front-door"),
            lifecycle: CameraLifecycle::Connected,
            expected_streams: Vec::new(),
            connected_streams: Vec::new(),
            last_error: None,
        };
        assert_eq!(connected_video_stream_ids(Some(&aggregate_only)), None);

        let known_empty = CameraStatus {
            expected_streams: vec!["main".to_owned()],
            ..aggregate_only
        };
        assert_eq!(
            connected_video_stream_ids(Some(&known_empty)),
            Some(Vec::new())
        );
    }

    #[test]
    fn detector_failure_does_not_demote_healthy_camera_recording() {
        let camera = project_camera_health(&CameraHealthEvidence {
            expected: true,
            lifecycle: Some(CameraLifecycle::Connected),
            startup_grace: false,
            report_age_ms: Some(100),
            frames_fresh: Some(true),
            decodable: Some(true),
            frame_rate_healthy: Some(true),
            recent_reconnects: 0,
            recent_drops: 0,
            recent_errors: 0,
            recording_requested: true,
            recording_progressing: Some(true),
            battery_sleeping: None,
        });
        let external_findings = [HealthIssue {
            severity: "warning".to_owned(),
            scope: "object-detector".to_owned(),
            message: "External detector is unavailable".to_owned(),
        }];

        assert_eq!(camera.state, CameraHealthState::Healthy);
        assert_eq!(camera.reason, CameraHealthReason::Healthy);
        assert_eq!(server_health_status(&external_findings), "degraded");
    }

    #[test]
    fn connected_camera_without_frame_progress_is_stale_in_api_counts_and_metrics() {
        let camera = CameraConfig {
            ip: "192.0.2.40".parse().unwrap(),
            name: Some("front-door".to_owned()),
            display_name: Some("Front Door".to_owned()),
            manufacturer: None,
            username: String::new(),
            password: String::new(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Retina,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: CameraRecordingMode::Off,
            event_recording_duration_secs: 60,
        };
        let config = Config::default();
        let storage = StorageConfig::default();
        let camera_configs = HashMap::from([("cameras".to_owned(), vec![camera])]);
        let registry = HealthRegistry::new();
        registry.publish(crate::stats::CameraReport {
            ip: "192.0.2.40".parse().unwrap(),
            name: Some("front-door".to_owned()),
            brand: None,
            port: 554,
            streams: vec![crate::stats::StreamReport {
                kind: "video_main".to_owned(),
                codec: Some("h264".to_owned()),
                resolution: Some("1920x1080".to_owned()),
                fps: 0.0,
                expected_fps: 15.0,
                kf_fps: 0.0,
                kbps: 0.0,
                max_frame_kb: 0.0,
                gap_min_ms: 0.0,
                gap_avg_ms: 0.0,
                gap_max_ms: 0.0,
                jitter_samples: 0,
                jitter_p50_ms: 0.0,
                jitter_p99_ms: 0.0,
                frames: None,
                bytes: None,
                keyframes: None,
                reconnects: None,
                drops: None,
                errors: None,
            }],
        });
        let state = ServerState::new(
            &config,
            &camera_configs,
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        )
        .with_health_registry(registry);
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        router_tx
            .send(RouterMessage::WorkerEvent(
                crate::runtime::WorkerEvent::StatusChanged(CameraStatus {
                    id: crate::api::CameraId::new("front-door"),
                    lifecycle: CameraLifecycle::Connected,
                    expected_streams: vec!["main".to_owned()],
                    connected_streams: vec!["main".to_owned()],
                    last_error: None,
                }),
            ))
            .unwrap();
        assert_eq!(router.wait_and_drain(Some(Duration::ZERO)).unwrap(), 1);
        let router_worker =
            std::thread::spawn(move || router.wait_and_drain(Some(Duration::from_secs(30))));

        let health = server_health(&router_tx, &state);
        assert_eq!(router_worker.join().unwrap().unwrap(), 1);
        assert_eq!(health.cameras[0].state, CameraHealthState::Stale);
        assert_eq!(
            health.cameras[0].reason,
            CameraHealthReason::FramesNotArriving
        );
        assert_eq!(health.totals.connected_cameras, 1);
        assert_eq!(health.totals.fresh_cameras, 0);
        assert_eq!(health.totals.decodable_cameras, 0);
        assert_eq!(health.totals.connected_video_streams, 1);
        assert_eq!(health.totals.fresh_video_streams, 0);
        assert_eq!(health.totals.decodable_video_streams, 0);

        let metrics = crate::metrics::encode_health(&health).unwrap();
        assert!(metrics.contains("state=\"stale\""));
        assert!(!metrics.contains("keeppeek_camera_online"));
        assert!(!metrics.contains("keeppeek_camera_degraded"));
        assert!(metrics.contains("dimension=\"frames_fresh\""));
        let proto = proto_health_snapshot(health);
        assert_eq!(
            proto.health_contract_version,
            CAMERA_HEALTH_CONTRACT_VERSION
        );
        assert_eq!(proto.cameras[0].state, "stale");
        assert_eq!(proto.cameras[0].reason, "frames_not_arriving");
        assert_eq!(proto.totals.unwrap().fresh_cameras, 0);
    }

    fn media_request(
        kind: proto::MediaKind,
        transport: proto::DeliveryTransport,
        quality: proto::VideoQuality,
        variant_id: &str,
    ) -> proto::SubscribeMedia {
        proto::SubscribeMedia {
            subscription_id: "front-door-live".to_owned(),
            source_session_id: camera_source_session_id("127.0.0.1"),
            kind: kind as i32,
            requested_delivery_transport: transport as i32,
            video_quality: quality as i32,
            variant_id: variant_id.to_owned(),
        }
    }

    #[test]
    fn canonical_create_and_delete_manage_one_api_session() {
        let state = ServerState::empty();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let offer = crate::webrtc::test_api_offer().to_sdp_string();
        let create_body = gzip_json(&CreateRequest {
            offer: crate::api::SdpOffer {
                sdp_type: "offer".to_owned(),
                sdp: offer,
            },
        });
        let create = handle_request(
            &Request::fake_http(
                "POST",
                "/create",
                vec![
                    ("Content-Type".to_owned(), "application/json".to_owned()),
                    ("Content-Encoding".to_owned(), "gzip".to_owned()),
                ],
                create_body,
            ),
            &router_tx,
            &state,
        );

        assert_eq!(create.status_code, 201);
        assert!(create.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Content-Encoding") && value == "gzip"
        }));
        let compressed = response_data(create);
        let mut decoder = GzDecoder::new(compressed.as_slice());
        let response: CreateResponse = serde_json::from_reader(&mut decoder).unwrap();
        assert_eq!(response.answer.sdp_type, "answer");
        assert!(response.answer.sdp.contains("a=ice-lite"));

        let delete_body = serde_json::to_vec(&DeleteRequest {
            session_id: response.session_id,
        })
        .unwrap();
        let delete = handle_request(
            &Request::fake_http(
                "POST",
                "/delete",
                vec![("Content-Type".to_owned(), "application/json".to_owned())],
                delete_body.clone(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(delete.status_code, 204);

        let repeated = handle_request(
            &Request::fake_http(
                "POST",
                "/delete",
                vec![("Content-Type".to_owned(), "application/json".to_owned())],
                delete_body,
            ),
            &router_tx,
            &state,
        );
        assert_eq!(repeated.status_code, 204);
        state.webrtc.shutdown();
    }

    #[test]
    fn canonical_create_rejects_an_uncompressed_offer() {
        let state = ServerState::empty();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let response = handle_request(
            &Request::fake_http("POST", "/create", Vec::new(), b"{}".to_vec()),
            &router_tx,
            &state,
        );

        assert_eq!(response.status_code, 415);
        let status: Status = serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(status.code, 415);
        assert!(status.message.contains("gzip"));
    }

    #[test]
    fn remote_and_forwarded_requests_require_the_configured_bearer_key() {
        let state = secured_test_state();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let remote = SocketAddr::from(([203, 0, 113, 7], 42_000));

        for path in ["/logs", "/logs/snapshot"] {
            let missing = handle_request(
                &Request::fake_http_from(remote, "GET", path, Vec::new(), Vec::new()),
                &router_tx,
                &state,
            );
            assert_eq!(missing.status_code, 401);

            let wrong = handle_request(
                &Request::fake_http_from(
                    remote,
                    "GET",
                    path,
                    vec![(
                        "Authorization".to_owned(),
                        "Bearer 123e4567-e89b-12d3-a456-426614174000".to_owned(),
                    )],
                    Vec::new(),
                ),
                &router_tx,
                &state,
            );
            assert_eq!(wrong.status_code, 401);

            let authenticated = handle_request(
                &Request::fake_http_from(remote, "GET", path, vec![bearer_header()], Vec::new()),
                &router_tx,
                &state,
            );
            assert_eq!(authenticated.status_code, 503);
        }

        let forwarded_local = handle_request(
            &Request::fake_http(
                "GET",
                "/logs",
                vec![("X-Forwarded-For".to_owned(), "203.0.113.7".to_owned())],
                Vec::new(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(forwarded_local.status_code, 401);

        for local_network in [
            SocketAddr::from(([192, 168, 1, 50], 42_000)),
            SocketAddr::from(([169, 254, 1, 50], 42_000)),
        ] {
            let response = handle_request(
                &Request::fake_http_from(local_network, "GET", "/logs", Vec::new(), Vec::new()),
                &router_tx,
                &state,
            );
            assert_eq!(response.status_code, 401);
        }
    }

    #[test]
    fn canonical_preflight_requires_an_exact_configured_origin() {
        let state = secured_test_state();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let allowed = handle_request(
            &Request::fake_http_from(
                SocketAddr::from(([203, 0, 113, 7], 42_000)),
                "OPTIONS",
                "/create",
                vec![("Origin".to_owned(), "https://home.example.net".to_owned())],
                Vec::new(),
            ),
            &router_tx,
            &state,
        );

        assert_eq!(allowed.status_code, 204);
        assert!(allowed.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Access-Control-Allow-Origin")
                && value == "https://home.example.net"
        }));
        assert!(allowed.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Access-Control-Allow-Methods") && value == "POST, OPTIONS"
        }));
        assert!(allowed.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Access-Control-Allow-Headers")
                && value == "Authorization, Content-Type, Content-Encoding"
        }));

        let unauthenticated = handle_request(
            &Request::fake_http_from(
                SocketAddr::from(([203, 0, 113, 7], 42_000)),
                "POST",
                "/create",
                vec![("Origin".to_owned(), "https://home.example.net".to_owned())],
                Vec::new(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(unauthenticated.status_code, 401);
        assert!(unauthenticated.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Access-Control-Allow-Origin")
                && value == "https://home.example.net"
        }));

        let rejected = handle_request(
            &Request::fake_http(
                "OPTIONS",
                "/delete",
                vec![(
                    "Origin".to_owned(),
                    "https://unexpected.example.net".to_owned(),
                )],
                Vec::new(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(rejected.status_code, 403);
    }

    #[test]
    fn only_the_session_creator_identity_can_delete_it() {
        let state = secured_test_state();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let remote = SocketAddr::from(([203, 0, 113, 7], 42_000));
        let create = handle_request(
            &Request::fake_http_from(
                remote,
                "POST",
                "/create",
                vec![
                    bearer_header(),
                    ("Content-Type".to_owned(), "application/json".to_owned()),
                    ("Content-Encoding".to_owned(), "gzip".to_owned()),
                ],
                gzip_json(&CreateRequest {
                    offer: crate::api::SdpOffer {
                        sdp_type: "offer".to_owned(),
                        sdp: crate::webrtc::test_api_offer().to_sdp_string(),
                    },
                }),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(create.status_code, 201);
        let compressed = response_data(create);
        let mut decoder = GzDecoder::new(compressed.as_slice());
        let response: CreateResponse = serde_json::from_reader(&mut decoder).unwrap();
        let delete_body = serde_json::to_vec(&DeleteRequest {
            session_id: response.session_id,
        })
        .unwrap();

        let local_delete = handle_request(
            &Request::fake_http("POST", "/delete", Vec::new(), delete_body.clone()),
            &router_tx,
            &state,
        );
        assert_eq!(local_delete.status_code, 404);

        let creator_delete = handle_request(
            &Request::fake_http_from(
                remote,
                "POST",
                "/delete",
                vec![bearer_header()],
                delete_body,
            ),
            &router_tx,
            &state,
        );
        assert_eq!(creator_delete.status_code, 204);
        state.webrtc.shutdown();
    }

    #[test]
    fn data_channel_motion_control_preserves_request_id_and_fails_closed() {
        let handler = test_control_handler(ServerState::empty());
        let response = handler
            .handle(proto::Request {
                request_id: 73,
                command: Some(control_request::Command::CameraControlCommand(
                    proto::CameraControlCommand {
                        action: Some(camera_control_command::Action::SetMotionDetection(
                            proto::SetMotionDetection {
                                source_id: "missing-camera".to_owned(),
                                enabled: false,
                            },
                        )),
                    },
                )),
            })
            .response;

        assert_eq!(response.request_id, 73);
        let Some(control_response::Result::Error(error)) = response.result else {
            panic!("unknown camera motion control must return an error");
        };
        assert_eq!(error.code, proto::ErrorCode::NotFound as i32);
        assert_eq!(error.message, "camera not found");
    }

    #[test]
    fn data_channel_motion_status_preserves_unavailable_camera_evidence() {
        let handler = test_control_handler(media_test_state());
        let response = handler
            .handle(proto::Request {
                request_id: 74,
                command: Some(control_request::Command::CameraControlCommand(
                    proto::CameraControlCommand {
                        action: Some(camera_control_command::Action::GetMotionDetection(
                            proto::GetMotionDetection {
                                source_id: "127.0.0.1".to_owned(),
                            },
                        )),
                    },
                )),
            })
            .response;

        assert_eq!(response.request_id, 74);
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("motion status must succeed for a configured camera");
        };
        let Some(control_ok::Result::MotionDetectionResult(result)) = ok.result else {
            panic!("motion status must return motion evidence");
        };
        assert!(!result.supported);
        assert!(!result.controllable);
        assert_eq!(result.enabled, None);
        assert_eq!(result.error, None);
    }

    #[test]
    fn data_channel_ptz_is_session_owned_and_stops_on_disconnect() {
        let commands = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let captured = commands.clone();
        let server = rouille::Server::new("127.0.0.1:0", move |request| {
            let command = request.get_param("cmd").unwrap_or_default();
            let payload = request.data().map_or(serde_json::Value::Null, |mut body| {
                let mut text = String::new();
                body.read_to_string(&mut text).unwrap();
                serde_json::from_str(&text).unwrap()
            });
            let value = match command.as_str() {
                "Login" => serde_json::json!({ "Token": { "name": "ptz-test-token" } }),
                "PtzCtrl" => {
                    captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(payload[0]["param"].clone());
                    serde_json::json!({})
                }
                "GetPtzPreset" => serde_json::json!({
                    "PtzPreset": [{ "id": 7, "enable": 1, "name": "Gate" }]
                }),
                _ => serde_json::json!({}),
            };
            Response::json(&serde_json::json!([{
                "cmd": command,
                "code": 0,
                "value": value
            }]))
        })
        .unwrap();
        let address = server.server_addr();
        let (worker, stop) = server.stoppable();

        let state = media_test_state();
        {
            let mut cameras = state
                .cameras
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cameras[0].info.capabilities.ptz = true;
            cameras[0].info.is_reolink = true;
            cameras[0].control = Some(CameraControl {
                ip: address.ip(),
                username: "operator".to_owned(),
                password: "secret".to_owned(),
                http_port: Some(address.port()),
            });
        }
        let handler = test_control_handler(state.clone());
        let capabilities = handler
            .initial_capabilities(SessionId::from_u64(40))
            .unwrap();
        let ptz = capabilities.cameras[0].ptz.as_ref().unwrap();
        assert!(ptz.supported);
        assert!(ptz.continuous);
        assert!(ptz.presets);
        assert!(ptz.zoom);
        assert!(!ptz.relative);

        let owner = SessionId::from_u64(41);
        let moving = handler.handle_for_session(
            owner,
            proto::Request {
                request_id: 111,
                command: Some(control_request::Command::CameraControlCommand(
                    proto::CameraControlCommand {
                        action: Some(camera_control_command::Action::Ptz(proto::PtzCommand {
                            source_id: "127.0.0.1".to_owned(),
                            action: Some(proto::ptz_command::Action::Continuous(
                                proto::PtzContinuous {
                                    pan: 0.5,
                                    tilt: 0.0,
                                    zoom: 0.0,
                                },
                            )),
                        })),
                    },
                )),
            },
        );
        assert!(matches!(
            moving.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::PtzResult(_))
            }))
        ));
        assert_eq!(
            state
                .ptz_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get("127.0.0.1"),
            Some(&owner)
        );
        let first = commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[0]
            .clone();
        assert_eq!(first["op"], "Right");
        assert_eq!(first["speed"], 32);

        let rejected = handler.handle_for_session(
            SessionId::from_u64(42),
            proto::Request {
                request_id: 113,
                command: Some(control_request::Command::CameraControlCommand(
                    proto::CameraControlCommand {
                        action: Some(camera_control_command::Action::Ptz(proto::PtzCommand {
                            source_id: "127.0.0.1".to_owned(),
                            action: Some(proto::ptz_command::Action::Continuous(
                                proto::PtzContinuous {
                                    pan: -1.0,
                                    tilt: 0.0,
                                    zoom: 0.0,
                                },
                            )),
                        })),
                    },
                )),
            },
        );
        let Some(control_response::Result::Error(error)) = rejected.response.result else {
            panic!("second PTZ owner must be rejected");
        };
        assert_eq!(error.code, proto::ErrorCode::Rejected as i32);

        let presets = handler.handle_for_session(
            owner,
            proto::Request {
                request_id: 115,
                command: Some(control_request::Command::CameraControlCommand(
                    proto::CameraControlCommand {
                        action: Some(camera_control_command::Action::Ptz(proto::PtzCommand {
                            source_id: "127.0.0.1".to_owned(),
                            action: Some(proto::ptz_command::Action::ListPresets(
                                proto::PtzPresetList {},
                            )),
                        })),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(ok)) = presets.response.result else {
            panic!("PTZ preset list must succeed");
        };
        let Some(control_ok::Result::PtzResult(result)) = ok.result else {
            panic!("PTZ preset list must return a PTZ result");
        };
        assert_eq!(result.presets[0].preset_id, 7);
        assert_eq!(result.presets[0].name, "Gate");

        handler.session_closed(owner);
        assert!(
            state
                .ptz_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        let captured_commands = commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(captured_commands.last().unwrap()["op"], "Stop");
        drop(captured_commands);

        let goto = handler.handle_for_session(
            SessionId::from_u64(42),
            proto::Request {
                request_id: 117,
                command: Some(control_request::Command::CameraControlCommand(
                    proto::CameraControlCommand {
                        action: Some(camera_control_command::Action::Ptz(proto::PtzCommand {
                            source_id: "127.0.0.1".to_owned(),
                            action: Some(proto::ptz_command::Action::GotoPreset(
                                proto::PtzPresetGoto { preset_id: 7 },
                            )),
                        })),
                    },
                )),
            },
        );
        assert!(matches!(
            goto.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::PtzResult(_))
            }))
        ));
        let captured_commands = commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(captured_commands.last().unwrap()["op"], "ToPos");
        assert_eq!(captured_commands.last().unwrap()["id"], 7);
        drop(captured_commands);

        stop.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn initial_capabilities_identify_the_connection_and_advertise_complete_contracts() {
        let handler = test_control_handler(ServerState::empty());
        let capabilities = handler
            .initial_capabilities(SessionId::from_u64(19))
            .expect("server handler must provide initial capabilities");

        assert_eq!(capabilities.revision, 1);
        assert_eq!(capabilities.self_source_session_id, "webrtc-client-19");
        assert_eq!(capabilities.source_sessions.len(), 1);
        assert_eq!(
            capabilities.source_sessions[0].source_session_id,
            capabilities.self_source_session_id
        );
        assert_eq!(
            capabilities.capability_ids,
            [
                "keeppeek.media-export.v1",
                "keeppeek.event-search",
                "keeppeek.event-publication.v1",
                "stored-media-keyframe-preview.v1"
            ]
        );
    }

    #[test]
    fn runtime_video_evidence_advertises_and_resolves_camera_media() {
        let state = media_test_state();
        for stream in [StreamKind::Main, StreamKind::Sub] {
            state.webrtc.live().publish(
                crate::webrtc::Source {
                    camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                    stream,
                },
                crate::storage::VideoCodec::H264,
                true,
                Instant::now(),
                None,
                bytes::Bytes::from_static(&[0, 0, 0, 1]),
            );
        }
        let handler = test_control_handler(state);

        let capabilities = handler
            .initial_capabilities(SessionId::from_u64(23))
            .unwrap();
        let source = capabilities
            .source_sessions
            .iter()
            .find(|source| source.source_id == "127.0.0.1")
            .expect("runtime camera source must be advertised");
        let video = source
            .video
            .as_ref()
            .expect("camera source must include video");
        assert_eq!(
            video
                .variants
                .iter()
                .map(|variant| variant.variant_id.as_str())
                .collect::<Vec<_>>(),
            ["main", "sub"]
        );
        assert!(video.variants.iter().all(|variant| {
            variant.codec.as_ref().map(|codec| codec.name.as_str()) == Some("h264")
        }));

        let plan = handler
            .resolve_media_subscription(&media_request(
                proto::MediaKind::Video,
                proto::DeliveryTransport::Rtp,
                proto::VideoQuality::Auto,
                "",
            ))
            .unwrap();
        assert_eq!(plan.selected_variant_id, "sub");
        assert_eq!(plan.quality, StreamQuality::Auto);
        assert!(plan.has_sub_stream);
        assert_eq!(plan.recording_label, "front-door");
    }

    #[test]
    fn media_subscription_falls_back_to_the_only_available_variant() {
        let state = media_test_state();
        state.webrtc.live().publish(
            crate::webrtc::Source {
                camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                stream: StreamKind::Sub,
            },
            crate::storage::VideoCodec::H264,
            true,
            Instant::now(),
            None,
            bytes::Bytes::from_static(&[0, 0, 0, 1]),
        );
        let handler = test_control_handler(state);

        let plan = handler
            .resolve_media_subscription(&media_request(
                proto::MediaKind::Video,
                proto::DeliveryTransport::Rtp,
                proto::VideoQuality::High,
                "",
            ))
            .unwrap();

        assert_eq!(plan.selected_variant_id, "sub");
        assert_eq!(plan.quality, StreamQuality::Low);
        assert_eq!(plan.delivery_transport, proto::DeliveryTransport::Rtp);
    }

    #[test]
    fn media_subscription_accepts_reliable_data_delivery() {
        let state = media_test_state();
        state.webrtc.live().publish(
            crate::webrtc::Source {
                camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                stream: StreamKind::Sub,
            },
            crate::storage::VideoCodec::H264,
            true,
            Instant::now(),
            None,
            bytes::Bytes::from_static(&[0, 0, 0, 1]),
        );
        let handler = test_control_handler(state);

        let plan = handler
            .resolve_media_subscription(&media_request(
                proto::MediaKind::Video,
                proto::DeliveryTransport::ReliableData,
                proto::VideoQuality::Low,
                "",
            ))
            .unwrap();

        assert_eq!(plan.selected_variant_id, "sub");
        assert_eq!(plan.quality, StreamQuality::Low);
        assert_eq!(
            plan.delivery_transport,
            proto::DeliveryTransport::ReliableData
        );
    }

    #[test]
    fn published_detection_is_persisted_and_retry_is_idempotent() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-published-event-{}",
            rand::random::<u64>()
        ));
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let events = EventStore::new(handle.clone(), &directory.join("thumbnails"), 0).unwrap();
        let mut state = media_test_state();
        state.events = Some(events);
        state.catalog = Some(handle.clone());
        state.webrtc.live().publish(
            crate::webrtc::Source {
                camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                stream: StreamKind::Sub,
            },
            crate::storage::VideoCodec::H264,
            true,
            Instant::now(),
            None,
            bytes::Bytes::from_static(&[0, 0, 0, 1]),
        );
        let handler = test_control_handler(state);
        let capabilities = handler
            .initial_capabilities(SessionId::from_u64(7))
            .unwrap();
        let source = capabilities
            .source_sessions
            .iter()
            .find(|source| source.source_id == "127.0.0.1")
            .unwrap();
        assert_eq!(
            source
                .event_types
                .iter()
                .map(|event_type| event_type.event_type.as_str())
                .collect::<Vec<_>>(),
            ["person", "vehicle"]
        );
        let event = proto::Event {
            event_id: "detector-event-1".to_owned(),
            revision: 1,
            source_id: "127.0.0.1".to_owned(),
            media_kind: Some(proto::MediaKind::Video as i32),
            origin: proto::EventOrigin::Camera as i32,
            event_type: "person".to_owned(),
            start_time: Some(millis_timestamp(12_345)),
            end_time: None,
            confidence: Some(0.91),
            bounding_box: Some(proto::EventBoundingBox {
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
            }),
            zone: Some("porch".to_owned()),
            text: None,
            payload: Some(prost_types::Struct {
                fields: std::collections::BTreeMap::from([(
                    "stream_id".to_owned(),
                    prost_types::Value {
                        kind: Some(prost_types::value::Kind::StringValue("sub".to_owned())),
                    },
                )]),
            }),
            attachments: Vec::new(),
            source_session_id: Some(camera_source_session_id("127.0.0.1")),
            subscription_id: None,
        };
        let request = proto::Request {
            request_id: 41,
            command: Some(control_request::Command::PublishEvent(
                proto::PublishEvent { event: Some(event) },
            )),
        };

        for request_id in [41, 43] {
            let dispatch = handler.handle_for_session(
                SessionId::from_u64(7),
                proto::Request {
                    request_id,
                    ..request.clone()
                },
            );
            assert!(matches!(
                dispatch.response.result,
                Some(control_response::Result::Ok(proto::Ok { result: None }))
            ));
        }

        let stored = handle.event_by_id("detector-event-1").unwrap().unwrap();
        assert_eq!(stored.source, EventSource::KeepPeek);
        assert_eq!(stored.camera_id, "127.0.0.1");
        assert_eq!(stored.stream.as_deref(), Some("sub"));
        assert_eq!(stored.kind, "person");
        assert_eq!(stored.start_time_ms, 12_345);
        assert_eq!(stored.confidence, Some(0.91));
        assert_eq!(stored.bbox, Some([0.1, 0.2, 0.3, 0.4]));

        drop(handler);
        drop(handle);
        drop(catalog);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn media_subscription_rejects_unsupported_requests() {
        let state = media_test_state();
        state.webrtc.live().publish(
            crate::webrtc::Source {
                camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                stream: StreamKind::Main,
            },
            crate::storage::VideoCodec::H264,
            true,
            Instant::now(),
            None,
            bytes::Bytes::from_static(&[0, 0, 0, 1]),
        );
        let handler = test_control_handler(state);
        let cases = [
            (
                media_request(
                    proto::MediaKind::Audio,
                    proto::DeliveryTransport::Rtp,
                    proto::VideoQuality::Auto,
                    "",
                ),
                proto::ErrorCode::InvalidRequest,
            ),
            (
                media_request(
                    proto::MediaKind::Video,
                    proto::DeliveryTransport::Rtp,
                    proto::VideoQuality::Auto,
                    "missing",
                ),
                proto::ErrorCode::NotFound,
            ),
            (
                media_request(
                    proto::MediaKind::Video,
                    proto::DeliveryTransport::Rtp,
                    proto::VideoQuality::High,
                    "main",
                ),
                proto::ErrorCode::InvalidRequest,
            ),
        ];

        for (request, expected_code) in cases {
            let error = handler.resolve_media_subscription(&request).unwrap_err();
            assert_eq!(error.code, expected_code);
        }
    }

    #[test]
    fn stored_timeline_query_emits_indexed_availability_events_and_end_marker() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-stored-query-{}", rand::random::<u64>()));
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(crate::storage::CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front-door/sub".to_owned(),
                source_id: Some("127.0.0.1".to_owned()),
                logical_stream_id: Some("sub".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(3_000),
                path: directory
                    .join("recording.mp4")
                    .to_string_lossy()
                    .into_owned(),
                init_offset: 0,
                init_len: 128,
                finalized: true,
            })
            .unwrap();
        handle
            .insert_fragment(crate::storage::CatalogFragment {
                recording_id: "recording-1".to_owned(),
                sequence: 1,
                start_ms: 1_000,
                duration_ms: 2_000,
                byte_offset: 128,
                byte_len: 512,
                random_access: true,
            })
            .unwrap();
        let event_store =
            EventStore::new(handle.clone(), &directory.join("thumbnails"), 0).unwrap();
        event_store
            .insert(TimelineEvent {
                id: "motion-1".to_owned(),
                camera_id: "127.0.0.1".to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::Camera,
                kind: "motion".to_owned(),
                start_time_ms: 1_500,
                end_time_ms: Some(1_700),
                confidence: Some(0.8),
                bbox: Some([0.1, 0.2, 0.3, 0.4]),
                zone: Some("porch".to_owned()),
                thumbnail_filename: None,
            })
            .unwrap();
        let mut state = media_test_state();
        state.catalog = Some(handle);
        state.events = Some(event_store);
        let handler = test_control_handler(state);

        let dispatch = handler.handle(proto::Request {
            request_id: 91,
            command: Some(control_request::Command::StoredMediaCommand(
                proto::StoredMediaCommand {
                    action: Some(stored_media_command::Action::QueryTimeline(
                        proto::QueryStoredMediaTimeline {
                            query_id: "timeline-1".to_owned(),
                            source_ids: vec!["127.0.0.1".to_owned()],
                            start_time: Some(millis_timestamp(0)),
                            end_time: Some(millis_timestamp(5_000)),
                            payload_types: Vec::new(),
                            availability_bucket: None,
                            channel: proto::DataChannelKind::ReliableData as i32,
                            events: Some(proto::StoredMediaEventQuery {
                                event_types: Vec::new(),
                                include_attachments: false,
                            }),
                            ..Default::default()
                        },
                    )),
                },
            )),
        });

        assert_eq!(dispatch.response.request_id, 91);
        let Some(control_response::Result::Ok(ok)) = dispatch.response.result else {
            panic!("stored timeline query must succeed");
        };
        let Some(control_ok::Result::StoredMediaQueryDelivery(delivery)) = ok.result else {
            panic!("stored timeline query must return its delivery channel");
        };
        assert_eq!(delivery.query_id, "timeline-1");
        assert_eq!(dispatch.data_messages.len(), 2);
        assert_eq!(
            dispatch.data_messages[0].target,
            DataChannelTarget::Reliable
        );
        let Some(proto::message::Message::StoredMediaQuery(query_message)) =
            &dispatch.data_messages[0].message.message
        else {
            panic!("first stored timeline data message must be a query page");
        };
        let Some(proto::stored_media_query_message::Message::Page(page)) = &query_message.message
        else {
            panic!("first stored timeline data message must be a page");
        };
        assert_eq!(page.sequence, 1);
        assert_eq!(page.availability.len(), 2);
        assert_eq!(page.availability[0].source_id, "127.0.0.1");
        assert_eq!(page.availability[0].stream_id, "main");
        assert_eq!(page.availability[1].source_id, "127.0.0.1");
        assert_eq!(page.availability[1].stream_id, "sub");
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event_id, "motion-1");
        let Some(proto::message::Message::StoredMediaQuery(query_message)) =
            &dispatch.data_messages[1].message.message
        else {
            panic!("last stored timeline data message must be a query end marker");
        };
        let Some(proto::stored_media_query_message::Message::End(end)) = &query_message.message
        else {
            panic!("last stored timeline data message must be an end marker");
        };
        assert_eq!(end.page_count, 1);
        assert_eq!(end.attachment_count, 0);

        let mut event_only_query = proto::QueryStoredMediaTimeline {
            query_id: "timeline-events".to_owned(),
            source_ids: vec!["127.0.0.1".to_owned()],
            start_time: Some(millis_timestamp(0)),
            end_time: Some(millis_timestamp(5_000)),
            payload_types: Vec::new(),
            availability_bucket: None,
            channel: proto::DataChannelKind::ReliableData as i32,
            events: Some(proto::StoredMediaEventQuery {
                event_types: Vec::new(),
                include_attachments: false,
            }),
            ..Default::default()
        };
        event_only_query.omit_availability = true;
        let event_only = handler.handle(proto::Request {
            request_id: 92,
            command: Some(control_request::Command::StoredMediaCommand(
                proto::StoredMediaCommand {
                    action: Some(stored_media_command::Action::QueryTimeline(
                        event_only_query,
                    )),
                },
            )),
        });
        let Some(proto::message::Message::StoredMediaQuery(query_message)) =
            &event_only.data_messages[0].message.message
        else {
            panic!("event-only timeline query must emit a query page");
        };
        let Some(proto::stored_media_query_message::Message::Page(page)) = &query_message.message
        else {
            panic!("event-only timeline query must emit a page");
        };
        assert!(page.availability.is_empty());
        assert_eq!(page.events.len(), 1);

        drop(handler);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn event_search_queries_and_fetches_encoded_media_objects() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-event-search-{}", rand::random::<u64>()));
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let started_at = Instant::now();
        let mut writer =
            crate::storage::medium_term::MediumTermWriter::create_with_catalog_identity(
                &directory,
                crate::storage::RecordingStreamIdentity::new(
                    "127.0.0.1",
                    "sub",
                    "archived-front-door",
                ),
                started_at,
                8 * 1024,
                handle.clone(),
            )
            .unwrap();
        let frame_payload = bytes::Bytes::from_static(&[
            0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
            0x3c, 0x80, 0, 0, 0, 1, 0x65,
        ]);
        for offset_ms in [0, 1_000] {
            writer
                .append_one(crate::storage::RecordingFrame {
                    received_at: started_at + Duration::from_millis(offset_ms),
                    timestamp: Some(Duration::from_millis(offset_ms)),
                    frame: crate::storage::MediaFrame::Video(crate::storage::VideoFrame {
                        codec: crate::storage::VideoCodec::H264,
                        is_keyframe: true,
                        width: 640,
                        height: 360,
                        data: frame_payload.clone(),
                    }),
                })
                .unwrap();
        }
        writer.finalize().unwrap();
        let fragments = handle
            .media_fragments_in_range("archived-front-door/sub", 0, i64::MAX)
            .unwrap();
        let event_time_ms = fragments[0].start_ms + 100;
        handle
            .insert_event(TimelineEvent {
                id: "face-1".to_owned(),
                camera_id: "127.0.0.1".to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::KeepPeek,
                kind: "face".to_owned(),
                start_time_ms: event_time_ms,
                end_time_ms: Some(event_time_ms + 200),
                confidence: Some(0.98),
                bbox: None,
                zone: None,
                thumbnail_filename: None,
            })
            .unwrap();

        let mut state = media_test_state();
        state.catalog = Some(handle);
        let handler = test_control_handler(state);
        let wrong_source = handler.handle(proto::Request {
            request_id: 199,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::ReplaceTerms(
                        proto::ReplaceEventSearchTerms {
                            event_id: "face-1".to_owned(),
                            source_id: "192.0.2.99".to_owned(),
                            terms: vec![proto::EventSearchTerm {
                                field: proto::EventSearchField::FaceName as i32,
                                value: "Mallory".to_owned(),
                            }],
                        },
                    )),
                },
            )),
        });
        assert!(matches!(
            wrong_source.response.result,
            Some(control_response::Result::Error(proto::Error {
                code,
                ..
            })) if code == proto::ErrorCode::NotFound as i32
        ));
        let event_type_mutation = handler.handle(proto::Request {
            request_id: 200,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::ReplaceTerms(
                        proto::ReplaceEventSearchTerms {
                            event_id: "face-1".to_owned(),
                            source_id: "127.0.0.1".to_owned(),
                            terms: vec![proto::EventSearchTerm {
                                field: proto::EventSearchField::EventType as i32,
                                value: "vehicle".to_owned(),
                            }],
                        },
                    )),
                },
            )),
        });
        assert!(matches!(
            event_type_mutation.response.result,
            Some(control_response::Result::Error(proto::Error {
                code,
                ..
            })) if code == proto::ErrorCode::InvalidRequest as i32
        ));
        let replace = handler.handle(proto::Request {
            request_id: 201,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::ReplaceTerms(
                        proto::ReplaceEventSearchTerms {
                            event_id: "face-1".to_owned(),
                            source_id: "127.0.0.1".to_owned(),
                            terms: vec![proto::EventSearchTerm {
                                field: proto::EventSearchField::FaceName as i32,
                                value: "Alice Example".to_owned(),
                            }],
                        },
                    )),
                },
            )),
        });
        assert!(matches!(
            replace.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::EventSearchMutation(_))
            }))
        ));

        let embedding = proto::EventSearchEmbedding {
            model_id: "vision-embedding".to_owned(),
            values: vec![1.0, 0.0, 0.0],
        };
        let set_embedding = handler.handle(proto::Request {
            request_id: 203,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::SetEmbedding(
                        proto::SetEventSearchEmbedding {
                            event_id: "face-1".to_owned(),
                            embedding: Some(embedding.clone()),
                            source_id: "127.0.0.1".to_owned(),
                        },
                    )),
                },
            )),
        });
        assert!(matches!(
            set_embedding.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::EventSearchMutation(_))
            }))
        ));

        let query = handler.handle(proto::Request {
            request_id: 205,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::Query(proto::QueryEvents {
                        query_id: "event-query-1".to_owned(),
                        search: Some(proto::query_events::Search::Text(proto::EventTextSearch {
                            query: "ali".to_owned(),
                            field: Some(proto::EventSearchField::FaceName as i32),
                        })),
                        source_id: Some("127.0.0.1".to_owned()),
                        stream_id: "main".to_owned(),
                        start_time: Some(millis_timestamp(fragments[0].start_ms - 1_000)),
                        end_time: Some(millis_timestamp(
                            fragments[1]
                                .start_ms
                                .saturating_add(i64::try_from(fragments[1].duration_ms).unwrap()),
                        )),
                        preview_before: Some(millis_duration(500)),
                        preview_after: Some(millis_duration(1_500)),
                        page_size: 10,
                        offset: 0,
                        channel: proto::DataChannelKind::ReliableData as i32,
                        page_token: String::new(),
                    })),
                },
            )),
        });
        let Some(control_response::Result::Ok(proto::Ok {
            result: Some(control_ok::Result::EventSearchDelivery(delivery)),
        })) = query.response.result
        else {
            panic!("event search query must return delivery metadata");
        };
        assert_eq!(delivery.query_id, "event-query-1");
        assert_eq!(query.data_messages.len(), 2);
        let Some(proto::message::Message::EventSearch(search_message)) =
            &query.data_messages[0].message.message
        else {
            panic!("event search result must use its binary message family");
        };
        let Some(proto::event_search_message::Message::Result(result)) = &search_message.message
        else {
            panic!("first event search message must contain a result");
        };
        let hit = result.hit.as_ref().unwrap();
        assert_eq!(hit.event_id, "face-1");
        assert_eq!(hit.keyframes.len(), 2);
        assert_eq!(hit.keyframes[0].source_id, "127.0.0.1");
        assert_eq!(hit.keyframes[0].stream_id, "main");

        let semantic = handler.handle(proto::Request {
            request_id: 207,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::Query(proto::QueryEvents {
                        query_id: "semantic-query-1".to_owned(),
                        search: Some(proto::query_events::Search::Semantic(
                            proto::EventSemanticSearch {
                                embedding: Some(embedding),
                            },
                        )),
                        source_id: Some("127.0.0.1".to_owned()),
                        stream_id: "main".to_owned(),
                        start_time: Some(millis_timestamp(fragments[0].start_ms - 1_000)),
                        end_time: Some(millis_timestamp(fragments[1].start_ms + 2_000)),
                        preview_before: None,
                        preview_after: None,
                        page_size: 10,
                        offset: 0,
                        channel: proto::DataChannelKind::ReliableData as i32,
                        page_token: String::new(),
                    })),
                },
            )),
        });
        assert_eq!(semantic.data_messages.len(), 2);

        let keyframe = &hit.keyframes[0];
        let objects: Vec<proto::EventSearchMediaObject> = [
            (
                "keyframe",
                proto::StoredMediaObjectRepresentation::EncodedKeyframe,
            ),
            (
                "initialization",
                proto::StoredMediaObjectRepresentation::Fmp4Initialization,
            ),
            ("gop", proto::StoredMediaObjectRepresentation::Fmp4Gop),
        ]
        .into_iter()
        .map(
            |(object_id, representation)| proto::EventSearchMediaObject {
                object_id: object_id.to_owned(),
                source_id: keyframe.source_id.clone(),
                stream_id: keyframe.stream_id.clone(),
                recording_id: keyframe.recording_id.clone(),
                fragment_sequence: keyframe.fragment_sequence,
                representation: representation as i32,
            },
        )
        .collect();
        let fetch = handler.handle(proto::Request {
            request_id: 209,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::FetchMedia(
                        proto::FetchEventSearchMedia {
                            transfer_id: "event-media-1".to_owned(),
                            objects: objects.clone(),
                            channel: proto::DataChannelKind::ReliableData as i32,
                        },
                    )),
                },
            )),
        });
        let Some(control_response::Result::Ok(proto::Ok {
            result: Some(control_ok::Result::EventSearchMediaDelivery(media_delivery)),
        })) = fetch.response.result
        else {
            panic!("event search media fetch must return delivery metadata");
        };
        assert_eq!(media_delivery.object_count, 3);
        let chunks = fetch
            .data_messages
            .iter()
            .filter_map(|message| {
                let Some(proto::message::Message::EventSearch(search)) = &message.message.message
                else {
                    return None;
                };
                let Some(proto::event_search_message::Message::MediaChunk(chunk)) = &search.message
                else {
                    return None;
                };
                Some(chunk)
            })
            .collect::<Vec<_>>();
        assert_eq!(chunks.len(), 3);
        let keyframe_chunk = chunks
            .iter()
            .find(|chunk| chunk.object_id == "keyframe")
            .unwrap();
        let initialization_chunk = chunks
            .iter()
            .find(|chunk| chunk.object_id == "initialization")
            .unwrap();
        let gop_chunk = chunks
            .iter()
            .find(|chunk| chunk.object_id == "gop")
            .unwrap();
        assert_eq!(chunks[0].object_id, "initialization");
        assert_eq!(keyframe_chunk.content_type, "video/h264; format=avcc");
        assert_eq!(keyframe_chunk.payload, frame_payload.as_ref());
        assert_eq!(keyframe_chunk.codec, "avc1.42001F");
        assert_eq!((keyframe_chunk.width, keyframe_chunk.height), (64, 96));
        assert_eq!(keyframe_chunk.nal_length_size, 4);
        assert!(!keyframe_chunk.decoder_config.is_empty());
        assert_eq!(&initialization_chunk.payload[4..8], b"ftyp");
        assert_eq!(&gop_chunk.payload[4..8], b"moof");
        assert_eq!(gop_chunk.recording_id, keyframe.recording_id);
        assert_eq!(gop_chunk.fragment_sequence, keyframe.fragment_sequence);
        assert!(matches!(
            fetch.data_messages.last().unwrap().message.message,
            Some(proto::message::Message::EventSearch(
                proto::EventSearchMessage {
                    message: Some(proto::event_search_message::Message::MediaEnd(_))
                }
            ))
        ));

        let cancelled = AtomicBool::new(false);
        let mut cancelled_messages = Vec::new();
        stream_event_search_media(
            &handler.state,
            &proto::FetchEventSearchMedia {
                transfer_id: "event-media-cancelled".to_owned(),
                objects,
                channel: proto::DataChannelKind::ReliableData as i32,
            },
            &cancelled,
            |message| {
                cancelled_messages.push(message);
                cancelled.store(true, Ordering::Release);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(cancelled_messages.len(), 1);
        assert!(matches!(
            cancelled_messages[0].message.message,
            Some(proto::message::Message::EventSearch(
                proto::EventSearchMessage {
                    message: Some(proto::event_search_message::Message::MediaChunk(_))
                }
            ))
        ));

        drop(handler);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn event_search_tasks_are_bounded_per_session() {
        let state = ServerState::empty();
        let session_id = SessionId::from_u64(91);
        let mut tokens = Vec::new();
        for index in 0..MAX_EVENT_SEARCH_TASKS_PER_SESSION {
            tokens.push(
                register_event_search_task(
                    &state,
                    session_id,
                    &format!("event-search-query:{index}"),
                )
                .unwrap(),
            );
        }
        let error = register_event_search_task(&state, session_id, "event-search-query:overflow")
            .unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected);
        for (index, token) in tokens.iter().enumerate() {
            cancel_event_search_task(&state, session_id, &format!("event-search-query:{index}"));
            assert!(token.load(Ordering::Acquire));
        }
        let cancelled_error =
            register_event_search_task(&state, session_id, "event-search-query:0").unwrap_err();
        assert_eq!(cancelled_error.code, proto::ErrorCode::Rejected);
        for (index, token) in tokens.iter().enumerate() {
            finish_event_search_task(
                &state,
                session_id,
                &format!("event-search-query:{index}"),
                token,
            );
        }
        let reused =
            register_event_search_task(&state, session_id, "event-search-query:0").unwrap();
        finish_event_search_task(&state, session_id, "event-search-query:0", &reused);
    }

    #[test]
    fn stored_media_cursor_opens_seeks_updates_and_releases_demand() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-stored-cursor-{}", rand::random::<u64>()));
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let started_at = Instant::now();
        let mut writer =
            crate::storage::medium_term::MediumTermWriter::create_with_catalog_identity(
                &directory,
                crate::storage::RecordingStreamIdentity::new("127.0.0.1", "sub", "front-door"),
                started_at,
                8 * 1024,
                handle.clone(),
            )
            .unwrap();
        let frame_payload = bytes::Bytes::from_static(&[
            0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
            0x3c, 0x80, 0, 0, 0, 1, 0x65,
        ]);
        for offset_ms in [0, 1_000] {
            writer
                .append_one(crate::storage::RecordingFrame {
                    received_at: started_at + Duration::from_millis(offset_ms),
                    timestamp: Some(Duration::from_millis(offset_ms)),
                    frame: crate::storage::MediaFrame::Video(crate::storage::VideoFrame {
                        codec: crate::storage::VideoCodec::H264,
                        is_keyframe: true,
                        width: 640,
                        height: 360,
                        data: frame_payload.clone(),
                    }),
                })
                .unwrap();
        }
        writer.finalize().unwrap();
        let fragments = handle
            .media_fragments_in_range("front-door/sub", 0, i64::MAX)
            .unwrap();
        assert_eq!(fragments.len(), 2);
        let mut state = media_test_state();
        state.catalog = Some(handle);
        let open_time = fragments[0].start_ms + 100;
        let end_time = fragments[1]
            .start_ms
            .saturating_add(i64::try_from(fragments[1].duration_ms).unwrap());
        let (_, paused_playback_state, paused_playback_messages) = open_stored_media(
            &state,
            proto::OpenStoredMedia {
                stored_media_id: "paused-prebuffer".to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                timestamp: Some(millis_timestamp(open_time)),
                end_time: Some(millis_timestamp(end_time)),
                mode: proto::StoredMediaMode::Playback as i32,
                playing: false,
                playback_rate: 1.0,
                media_channel: proto::DataChannelKind::ReliableData as i32,
                data_payload_routes: Vec::new(),
                max_buffer_duration: Some(millis_duration(3_000)),
            },
        )
        .unwrap();
        assert!(!paused_playback_state.playing);
        assert_eq!(
            paused_playback_messages
                .iter()
                .filter(|message| {
                    matches!(
                        &message.message.message,
                        Some(proto::message::Message::StoredMedia(message))
                            if matches!(
                                message.message,
                                Some(proto::stored_media_message::Message::Fragment(_))
                            )
                    )
                })
                .count(),
            2
        );
        let handler = test_control_handler(state.clone());
        let session_id = SessionId::from_u64(77);
        let cursor_id = "review-1";

        let open = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 101,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::Open(proto::OpenStoredMedia {
                            stored_media_id: cursor_id.to_owned(),
                            source_id: "127.0.0.1".to_owned(),
                            stream_id: "main".to_owned(),
                            timestamp: Some(millis_timestamp(open_time)),
                            end_time: Some(millis_timestamp(end_time)),
                            mode: proto::StoredMediaMode::Scrub as i32,
                            playing: false,
                            playback_rate: 1.0,
                            media_channel: proto::DataChannelKind::ReliableData as i32,
                            data_payload_routes: Vec::new(),
                            max_buffer_duration: Some(millis_duration(500)),
                        })),
                    },
                )),
            },
        );

        let Some(control_response::Result::Ok(ok)) = &open.response.result else {
            panic!(
                "indexed stored media open must succeed: {:?}",
                open.response.result
            );
        };
        let Some(control_ok::Result::StoredMediaState(open_state)) = &ok.result else {
            panic!("stored media open must return cursor state");
        };
        assert_eq!(open_state.generation, 1);
        assert_eq!(open_state.status, proto::StoredMediaStatus::Active as i32);
        assert!(
            open_state
                .delivery
                .as_ref()
                .is_some_and(|delivery| delivery.content_type == "video/h264; format=avcc")
        );
        assert_eq!(open_state.mode, proto::StoredMediaMode::Scrub as i32);
        assert!(!open_state.playing);
        assert_eq!(state.recording_demand.viewer_count("front-door/sub"), 1);
        assert_eq!(open.data_messages.len(), 1);
        let Some(proto::message::Message::StoredMedia(open_message)) =
            &open.data_messages[0].message.message
        else {
            panic!("stored media scrub must emit a stored-media message");
        };
        let Some(proto::stored_media_message::Message::KeyFrame(open_keyframe)) =
            &open_message.message
        else {
            panic!("stored media scrub must emit one keyframe");
        };
        assert_eq!(open_keyframe.stored_media_id, cursor_id);
        assert_eq!(open_keyframe.generation, 1);
        let configuration = open_keyframe
            .configuration
            .as_ref()
            .expect("stored keyframe must include decoder configuration");
        assert!(
            configuration
                .codec
                .as_ref()
                .is_some_and(|codec| codec.name.eq_ignore_ascii_case("avc1.42001f"))
        );
        let Some(proto::media_data_format::Format::Video(video)) = configuration
            .format
            .as_ref()
            .and_then(|format| format.format.as_ref())
        else {
            panic!("stored keyframe must include a video format");
        };
        assert!(video.width > 0);
        assert!(video.height > 0);
        assert!(!video.decoder_config.is_empty());
        let frame = open_keyframe
            .frame
            .as_ref()
            .expect("stored keyframe must include its encoded frame");
        assert!(frame.key_frame);
        assert_eq!(frame.fragment_count, 1);
        assert!(!frame.payload.is_empty());
        assert!(open.notifications.is_empty());

        let start_playback = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 102,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::SetPlayback(
                            proto::SetStoredMediaPlayback {
                                stored_media_id: cursor_id.to_owned(),
                                playing: Some(true),
                                playback_rate: Some(1.0),
                                mode: Some(proto::StoredMediaMode::Playback as i32),
                            },
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(ok)) = start_playback.response.result else {
            panic!("stored media playback transition must succeed");
        };
        let Some(control_ok::Result::StoredMediaState(playback_state)) = ok.result else {
            panic!("stored media playback transition must return cursor state");
        };
        assert_eq!(playback_state.stored_media_id, cursor_id);
        assert_eq!(playback_state.generation, 1);
        assert_eq!(playback_state.mode, proto::StoredMediaMode::Playback as i32);
        assert!(playback_state.playing);
        assert!(start_playback.data_messages.iter().all(|message| {
            matches!(
                &message.message.message,
                Some(proto::message::Message::StoredMedia(message))
                    if matches!(
                        &message.message,
                        Some(proto::stored_media_message::Message::Initialization(initialization))
                            if initialization.generation == 1
                    ) || matches!(
                        &message.message,
                        Some(proto::stored_media_message::Message::Fragment(fragment))
                            if fragment.generation == 1
                    )
            )
        }));

        let refill = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 103,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::Refill(
                            proto::RefillStoredMedia {
                                stored_media_id: cursor_id.to_owned(),
                                playback_time: Some(millis_timestamp(open_time + 600)),
                            },
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(ok)) = refill.response.result else {
            panic!("stored media refill must succeed");
        };
        let Some(control_ok::Result::StoredMediaState(refill_state)) = ok.result else {
            panic!("stored media refill must return cursor state");
        };
        assert_eq!(refill_state.generation, 1);
        assert_eq!(refill_state.status, proto::StoredMediaStatus::Ended as i32);
        assert!(refill.data_messages.iter().all(|message| {
            matches!(
                &message.message.message,
                Some(proto::message::Message::StoredMedia(message))
                    if matches!(
                        &message.message,
                        Some(proto::stored_media_message::Message::Initialization(initialization))
                            if initialization.generation == 1
                    ) || matches!(
                        &message.message,
                        Some(proto::stored_media_message::Message::Fragment(fragment))
                            if fragment.generation == 1
                    )
            )
        }));
        assert!(matches!(
            refill.notifications.as_slice(),
            [proto::Notification {
                event: Some(proto::notification::Event::StoredMediaState(state))
            }] if state.status == proto::StoredMediaStatus::Ended as i32
        ));

        let start_scrub = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 104,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::SetPlayback(
                            proto::SetStoredMediaPlayback {
                                stored_media_id: cursor_id.to_owned(),
                                playing: Some(false),
                                playback_rate: None,
                                mode: Some(proto::StoredMediaMode::Scrub as i32),
                            },
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(ok)) = start_scrub.response.result else {
            panic!("stored media scrub transition must succeed");
        };
        let Some(control_ok::Result::StoredMediaState(scrub_state)) = ok.result else {
            panic!("stored media scrub transition must return cursor state");
        };
        assert_eq!(scrub_state.generation, 1);
        assert_eq!(scrub_state.mode, proto::StoredMediaMode::Scrub as i32);
        assert!(!scrub_state.playing);
        assert!(start_scrub.data_messages.is_empty());

        let seek = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 105,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::Seek(proto::SeekStoredMedia {
                            stored_media_id: cursor_id.to_owned(),
                            timestamp: Some(millis_timestamp(fragments[1].start_ms + 100)),
                        })),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(ok)) = seek.response.result else {
            panic!("stored media seek must succeed");
        };
        let Some(control_ok::Result::StoredMediaState(seek_state)) = ok.result else {
            panic!("stored media seek must return cursor state");
        };
        assert_eq!(seek_state.generation, 2);
        assert_eq!(seek_state.mode, proto::StoredMediaMode::Scrub as i32);
        assert_eq!(seek_state.stored_media_id, cursor_id);
        assert_eq!(seek.data_messages.len(), 1);
        assert!(matches!(
            &seek.data_messages[0].message.message,
            Some(proto::message::Message::StoredMedia(message))
                if matches!(
                    &message.message,
                    Some(proto::stored_media_message::Message::KeyFrame(keyframe))
                        if keyframe.generation == 2
                            && keyframe.stored_media_id == cursor_id
                )
        ));

        let resume = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 106,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::SetPlayback(
                            proto::SetStoredMediaPlayback {
                                stored_media_id: cursor_id.to_owned(),
                                playing: Some(true),
                                playback_rate: Some(1.0),
                                mode: Some(proto::StoredMediaMode::Playback as i32),
                            },
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(ok)) = resume.response.result else {
            panic!("stored media resume must succeed");
        };
        let Some(control_ok::Result::StoredMediaState(resume_state)) = ok.result else {
            panic!("stored media resume must return cursor state");
        };
        assert_eq!(resume_state.stored_media_id, cursor_id);
        assert_eq!(resume_state.generation, 2);
        assert_eq!(resume_state.mode, proto::StoredMediaMode::Playback as i32);
        assert!(resume_state.playing);
        assert!(resume.data_messages.iter().all(|message| {
            matches!(
                &message.message.message,
                Some(proto::message::Message::StoredMedia(message))
                    if matches!(
                        &message.message,
                        Some(proto::stored_media_message::Message::Initialization(initialization))
                            if initialization.generation == 2
                    ) || matches!(
                        &message.message,
                        Some(proto::stored_media_message::Message::Fragment(fragment))
                            if fragment.generation == 2
                    )
            )
        }));

        let update = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 107,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::SetPlayback(
                            proto::SetStoredMediaPlayback {
                                stored_media_id: cursor_id.to_owned(),
                                playing: Some(false),
                                playback_rate: Some(2.0),
                                mode: None,
                            },
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(ok)) = update.response.result else {
            panic!("stored media playback update must succeed");
        };
        let Some(control_ok::Result::StoredMediaState(update_state)) = ok.result else {
            panic!("stored media playback update must return cursor state");
        };
        assert!(!update_state.playing);
        assert_eq!(update_state.playback_rate, 2.0);

        let close = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 108,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::Close(
                            proto::CloseStoredMedia {
                                stored_media_id: cursor_id.to_owned(),
                            },
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(ok)) = close.response.result else {
            panic!("stored media close must succeed");
        };
        assert!(ok.result.is_none());
        assert_eq!(state.recording_demand.viewer_count("front-door/sub"), 0);

        drop((handler, state));
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_job_runs_reports_gaps_and_downloads_verified_file() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-export-job-{}", rand::random::<u64>()));
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let started_at = Instant::now();
        let mut writer = crate::storage::medium_term::MediumTermWriter::create_with_catalog(
            &directory,
            "front-door/sub",
            started_at,
            8 * 1_024,
            handle.clone(),
        )
        .unwrap();
        let frame_payload = bytes::Bytes::from_static(&[
            0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
            0x3c, 0x80, 0, 0, 0, 1, 0x65,
        ]);
        for offset_ms in [0, 1_000] {
            writer
                .append_one(crate::storage::RecordingFrame {
                    received_at: started_at + Duration::from_millis(offset_ms),
                    timestamp: Some(Duration::from_millis(offset_ms)),
                    frame: crate::storage::MediaFrame::Video(crate::storage::VideoFrame {
                        codec: crate::storage::VideoCodec::H264,
                        is_keyframe: true,
                        width: 640,
                        height: 360,
                        data: frame_payload.clone(),
                    }),
                })
                .unwrap();
        }
        writer.finalize().unwrap();
        let fragments = handle
            .media_fragments_in_range("front-door/sub", 0, i64::MAX)
            .unwrap();
        let start_ms = fragments[0].start_ms;
        let end_ms = fragments
            .last()
            .map(|fragment| {
                fragment
                    .start_ms
                    .saturating_add(i64::try_from(fragment.duration_ms).unwrap())
            })
            .unwrap();
        let mut state = media_test_state();
        state.catalog = Some(handle);
        state.storage_config.long_term_path = directory.join("export-root");
        let handler = test_control_handler(state.clone());

        let create = handler.handle(proto::Request {
            request_id: 121,
            command: Some(control_request::Command::ExportCommand(
                proto::ExportCommand {
                    action: Some(proto::export_command::Action::Create(
                        proto::CreateExportJob {
                            job_id: "export-ready".to_owned(),
                            source_id: "127.0.0.1".to_owned(),
                            stream_id: "main".to_owned(),
                            start_time: Some(millis_timestamp(start_ms)),
                            end_time: Some(millis_timestamp(end_ms)),
                            allow_partial: false,
                            burn_in_timestamp: false,
                        },
                    )),
                },
            )),
        });
        let Some(control_response::Result::Ok(ok)) = create.response.result else {
            panic!("export creation must succeed");
        };
        let Some(control_ok::Result::ExportJob(created)) = ok.result else {
            panic!("export creation must return a job");
        };
        assert_eq!(created.status, proto::ExportJobStatus::Running as i32);

        let deadline = Instant::now() + Duration::from_secs(2);
        let ready = loop {
            let response = handler.handle(proto::Request {
                request_id: 123,
                command: Some(control_request::Command::ExportCommand(
                    proto::ExportCommand {
                        action: Some(proto::export_command::Action::Get(proto::GetExportJob {
                            job_id: "export-ready".to_owned(),
                        })),
                    },
                )),
            });
            let Some(control_response::Result::Ok(ok)) = response.response.result else {
                panic!("export get must succeed");
            };
            let Some(control_ok::Result::ExportJob(job)) = ok.result else {
                panic!("export get must return a job");
            };
            if job.status == proto::ExportJobStatus::Ready as i32 {
                break job;
            }
            assert!(Instant::now() < deadline, "export did not become ready");
            std::thread::yield_now();
        };
        assert_eq!(ready.progress_per_mille, 1_000);
        assert!(ready.bytes_written > 0);
        assert_eq!(ready.sha256.as_deref().map(str::len), Some(64));
        assert!(ready.expires_at.is_some());
        assert_eq!(
            ready.file_name.as_deref(),
            Some(export_file_name("Front Door", start_ms, end_ms).as_str())
        );

        let download = handler.handle(proto::Request {
            request_id: 125,
            command: Some(control_request::Command::ExportCommand(
                proto::ExportCommand {
                    action: Some(proto::export_command::Action::Download(
                        proto::DownloadExport {
                            job_id: "export-ready".to_owned(),
                            channel: proto::DataChannelKind::ReliableData as i32,
                        },
                    )),
                },
            )),
        });
        let Some(control_response::Result::Ok(ok)) = download.response.result else {
            panic!("export download must succeed");
        };
        let Some(control_ok::Result::ExportDownload(delivery)) = ok.result else {
            panic!("export download must return delivery metadata");
        };
        assert_eq!(
            usize::try_from(delivery.chunk_count).unwrap(),
            download.data_messages.len()
        );
        let mut bytes = Vec::new();
        for message in download.data_messages {
            assert_eq!(message.group, "export:export-ready");
            let Some(proto::message::Message::Export(export)) = message.message.message else {
                panic!("export download must use export data messages");
            };
            let Some(proto::export_message::Message::FileChunk(chunk)) = export.message else {
                panic!("export data message must carry a file chunk");
            };
            bytes.extend_from_slice(&chunk.payload);
        }
        assert_eq!(u64::try_from(bytes.len()).unwrap(), ready.bytes_written);
        assert_eq!(
            encode_lower_hex(Sha256::digest(&bytes)),
            ready.sha256.unwrap()
        );
        let downloaded = directory.join("downloaded-export.mp4");
        std::fs::write(&downloaded, bytes).unwrap();
        assert!(mp4::read_mp4(File::open(downloaded).unwrap()).is_ok());

        let partial = create_export_job(
            &state,
            proto::CreateExportJob {
                job_id: "export-partial".to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                start_time: Some(millis_timestamp(start_ms)),
                end_time: Some(millis_timestamp(end_ms + 2_000)),
                allow_partial: false,
                burn_in_timestamp: false,
            },
        )
        .unwrap();
        assert_eq!(partial.status, proto::ExportJobStatus::Partial as i32);
        assert_eq!(partial.missing_ranges.len(), 1);

        let failed = create_export_job(
            &state,
            proto::CreateExportJob {
                job_id: "export-burn-in".to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                start_time: Some(millis_timestamp(start_ms)),
                end_time: Some(millis_timestamp(end_ms)),
                allow_partial: false,
                burn_in_timestamp: true,
            },
        )
        .unwrap();
        assert_eq!(failed.status, proto::ExportJobStatus::Failed as i32);
        assert!(!failed.retryable);
        assert!(
            failed
                .error
                .as_deref()
                .is_some_and(|error| error.contains("re-encoding"))
        );

        drop(handler);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_range_is_limited_to_two_minutes_and_named_for_the_camera() {
        assert_eq!(
            export_file_name(" Front / Dør ", 0, 120_000),
            "Front-Dør_1970-01-01T00-00-00-000Z_to_1970-01-01T00-02-00-000Z.mp4"
        );
        assert_eq!(
            export_file_name("///", 0, 1),
            "camera_1970-01-01T00-00-00-000Z_to_1970-01-01T00-00-00-001Z.mp4"
        );

        let directory = std::env::temp_dir().join(format!(
            "keeppeek-export-duration-{}",
            rand::random::<u64>()
        ));
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let mut state = media_test_state();
        state.catalog = Some(catalog.handle());
        let allowed = create_export_job(
            &state,
            proto::CreateExportJob {
                job_id: "export-two-minutes".to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                start_time: Some(millis_timestamp(0)),
                end_time: Some(millis_timestamp(120_000)),
                allow_partial: false,
                burn_in_timestamp: false,
            },
        )
        .unwrap();
        assert_eq!(allowed.status, proto::ExportJobStatus::Partial as i32);

        let error = create_export_job(
            &state,
            proto::CreateExportJob {
                job_id: "export-too-long".to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                start_time: Some(millis_timestamp(0)),
                end_time: Some(millis_timestamp(120_001)),
                allow_partial: false,
                burn_in_timestamp: false,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
        assert_eq!(error.message, "export range exceeds 2 minutes");

        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn data_channel_logging_command_persists_and_returns_settings() {
        let (state, _logging, dispatch, filter_file) = logging_test_state("error");
        let handler = test_control_handler(state);
        let response = tracing::dispatcher::with_default(&dispatch, || {
            handler
                .handle(proto::Request {
                    request_id: 75,
                    command: Some(control_request::Command::LoggingCommand(
                        proto::LoggingCommand {
                            action: Some(logging_command::Action::SetFilter(
                                proto::SetLoggingFilter {
                                    filter: "info,str0m=warn".to_owned(),
                                },
                            )),
                        },
                    )),
                })
                .response
        });

        assert_eq!(response.request_id, 75);
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("valid logging command must succeed");
        };
        let Some(control_ok::Result::LoggingSettingsResult(settings)) = ok.result else {
            panic!("logging command must return settings");
        };
        assert_eq!(settings.active_filter, "info,str0m=warn");
        assert_eq!(
            std::fs::read_to_string(filter_file.path()).unwrap(),
            "info,str0m=warn"
        );
        std::fs::remove_dir_all(filter_file.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn data_channel_restart_runs_only_after_response_dispatch() {
        let shutdown = Shutdown::new();
        let restart = Restart::default();
        let handler = test_control_handler(
            ServerState::empty().with_restart_control(shutdown.clone(), restart.clone()),
        );
        let dispatch = handler.handle(proto::Request {
            request_id: 77,
            command: Some(control_request::Command::ServerCommand(
                proto::ServerCommand {
                    action: Some(server_command::Action::Restart(proto::RestartServer {})),
                },
            )),
        });

        assert!(!shutdown.is_cancelled());
        assert!(!restart.is_requested());
        let Some(control_response::Result::Ok(ok)) = dispatch.response.result else {
            panic!("restart command must return a success response");
        };
        assert!(matches!(
            ok.result,
            Some(control_ok::Result::RestartResult(proto::RestartResult {
                restarting: true
            }))
        ));
        dispatch
            .after_send
            .expect("restart must schedule a post-send action")();
        assert!(shutdown.is_cancelled());
        assert!(restart.is_requested());
    }

    #[test]
    fn data_channel_access_key_management_is_loopback_only_and_rotates_live_state() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-server-access-key-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        config::write_private_file(
            &config_path,
            b"access_key = \"{secret:KEEPPEEK_ACCESS_KEY}\"\n",
        )
        .unwrap();
        config::write_private_file(
            &config::secrets_path(&config_path),
            b"KEEPPEEK_ACCESS_KEY = \"550e8400-e29b-41d4-a716-446655440000\"\n",
        )
        .unwrap();
        let access_key = AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        *state.access_key.write().unwrap() = access_key;
        let loopback_session = SessionId::from_u64(81);
        let remote_session = SessionId::from_u64(82);
        state.api_session_owners.lock().unwrap().extend([
            (loopback_session, ApiPrincipal::LoopbackAdministrator),
            (
                remote_session,
                ApiPrincipal::AccessKey(access_key.fingerprint()),
            ),
        ]);
        let handler = test_control_handler(state.clone());

        let remote = handler.handle_for_session(
            remote_session,
            proto::Request {
                request_id: 80,
                command: Some(control_request::Command::ServerCommand(
                    proto::ServerCommand {
                        action: Some(server_command::Action::GetAccessKey(proto::GetAccessKey {})),
                    },
                )),
            },
        );
        assert!(matches!(
            remote.response.result,
            Some(control_response::Result::Error(proto::Error {
                code,
                ..
            })) if code == proto::ErrorCode::Rejected as i32
        ));

        let reveal = handler.handle_for_session(
            loopback_session,
            proto::Request {
                request_id: 81,
                command: Some(control_request::Command::ServerCommand(
                    proto::ServerCommand {
                        action: Some(server_command::Action::GetAccessKey(proto::GetAccessKey {})),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(reveal)) = reveal.response.result else {
            panic!("loopback access key reveal must succeed");
        };
        assert!(matches!(
            reveal.result,
            Some(control_ok::Result::AccessKeyResult(proto::AccessKeyResult {
                access_key: ref value,
                rotated: false,
            })) if value == "550e8400-e29b-41d4-a716-446655440000"
        ));

        let rotation = handler.handle_for_session(
            loopback_session,
            proto::Request {
                request_id: 82,
                command: Some(control_request::Command::ServerCommand(
                    proto::ServerCommand {
                        action: Some(server_command::Action::RotateAccessKey(
                            proto::RotateAccessKey {},
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(rotation_result)) = rotation.response.result else {
            panic!("loopback access key rotation must succeed");
        };
        let Some(control_ok::Result::AccessKeyResult(result)) = rotation_result.result else {
            panic!("rotation must return the replacement access key");
        };
        assert!(result.rotated);
        assert_ne!(result.access_key, access_key.canonical());
        let rotated = AccessKey::parse(&result.access_key).unwrap();
        assert_eq!(*state.access_key.read().unwrap(), rotated);
        assert_eq!(
            state
                .api_session_owners
                .lock()
                .unwrap()
                .get(&remote_session),
            None
        );
        let secret_file = std::fs::read_to_string(config::secrets_path(&config_path)).unwrap();
        assert!(secret_file.contains(&result.access_key));
        assert!(!secret_file.contains(&access_key.canonical()));
        rotation
            .after_send
            .expect("rotation must defer remote session closure")();

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn data_channel_access_key_reveal_rejects_an_unset_key() {
        let state = ServerState::empty();
        let loopback_session = SessionId::from_u64(83);
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(loopback_session, ApiPrincipal::LoopbackAdministrator);
        let response = test_control_handler(state)
            .handle_for_session(
                loopback_session,
                proto::Request {
                    request_id: 83,
                    command: Some(control_request::Command::ServerCommand(
                        proto::ServerCommand {
                            action: Some(server_command::Action::GetAccessKey(
                                proto::GetAccessKey {},
                            )),
                        },
                    )),
                },
            )
            .response;

        assert!(matches!(
            response.result,
            Some(control_response::Result::Error(proto::Error {
                code,
                ..
            })) if code == proto::ErrorCode::Unavailable as i32
        ));
    }

    #[test]
    fn data_channel_camera_discovery_rejects_out_of_range_prefix() {
        let handler = test_control_handler(ServerState::empty());
        let response = handler
            .handle(proto::Request {
                request_id: 79,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::Discover(
                            proto::DiscoverCameras {
                                subnets: vec![256],
                                networks: Vec::new(),
                                discovery_id: String::new(),
                            },
                        )),
                    },
                )),
            })
            .response;

        assert_eq!(response.request_id, 79);
        let Some(control_response::Result::Error(error)) = response.result else {
            panic!("invalid discovery prefix must return an error");
        };
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);
        assert!(error.message.contains("0 and 255"));
    }

    #[test]
    fn data_channel_camera_stream_probe_requires_credentials() {
        let handler = test_control_handler(ServerState::empty());
        let response = handler
            .handle(proto::Request {
                request_id: 80,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::ProbeStreams(
                            proto::ProbeCameraStreams {
                                ip: "192.0.2.50".to_owned(),
                                username: String::new(),
                                password: String::new(),
                                onvif_port: Some(8000),
                                ..Default::default()
                            },
                        )),
                    },
                )),
            })
            .response;

        assert_eq!(response.request_id, 80);
        let Some(control_response::Result::Error(error)) = response.result else {
            panic!("probe without credentials must return an error");
        };
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);
        assert!(error.message.contains("username and password"));
    }

    #[test]
    fn data_channel_camera_catalog_uses_injected_test_cameras() {
        let handler = test_control_handler(
            ServerState::for_test().with_test_camera_catalog(TestCameraCatalog::standard()),
        );
        let metadata = handler
            .handle(proto::Request {
                request_id: 80,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::GetCatalog(
                            proto::GetCameraCatalog {},
                        )),
                    },
                )),
            })
            .response;

        let Some(control_response::Result::Ok(ok)) = metadata.result else {
            panic!("test catalog metadata must succeed");
        };
        let Some(control_ok::Result::CameraCatalogInfo(info)) = ok.result else {
            panic!("test catalog must return metadata");
        };
        assert_eq!(info.version, "test");
        assert_eq!(info.camera_count, 3);

        let search = handler
            .handle(proto::Request {
                request_id: 81,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::SearchCatalog(
                            proto::SearchCameraCatalog {
                                query: "RLC-Test".to_owned(),
                                limit: Some(1),
                                ip: Some("192.0.2.77".to_owned()),
                            },
                        )),
                    },
                )),
            })
            .response;

        let Some(control_response::Result::Ok(ok)) = search.result else {
            panic!("test catalog search must succeed");
        };
        let Some(control_ok::Result::CameraCatalogSearchResult(result)) = ok.result else {
            panic!("test catalog search must return matching cameras");
        };
        let [camera] = result.cameras.as_slice() else {
            panic!("test catalog search must return one camera");
        };
        assert_eq!(camera.id, "keeppeek-test-reolink");
        assert_eq!(camera.brand, "Reolink");
        assert_eq!(camera.model, "RLC-Test");
        let hints = camera
            .stream_hints
            .as_ref()
            .expect("test catalog camera must include stream hints");
        assert_eq!(
            hints.main_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.77/main")
        );
        assert_eq!(hints.sub_rtsp_url.as_deref(), Some("rtsp://192.0.2.77/sub"));
    }

    #[test]
    fn data_channel_camera_update_preserves_secrets_and_supports_clear_and_remove() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-control-camera-{}", rand::random::<u64>()));
        let config_path = directory.join("config.toml");
        crate::config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "{secret:CAMERA_USERNAME}"

                [cameras.gate]
                ip = "192.0.2.77"
                password = "{secret:GATE_PASSWORD}"
                main_rtsp_url = "rtsp://192.0.2.77/main"
                sub_rtsp_url = "rtsp://{secret:GATE_HOST}/sub"
            "#,
        )
        .unwrap();
        crate::config::write_private_file(
            &crate::config::secrets_path(&config_path),
            br#"
                CAMERA_USERNAME = "operator"
                GATE_PASSWORD = "preserved-secret"
                GATE_HOST = "192.0.2.77"
            "#,
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let handler = ServerControlHandler::new(state, router_tx);
        let response = handler
            .handle(proto::Request {
                request_id: 81,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::Update(
                            proto::UpdateCameraConfiguration {
                                ip: "192.0.2.77".to_owned(),
                                display_name: Some(proto::OptionalStringUpdate {
                                    value: Some(optional_string_update::Value::Set(
                                        "Updated Gate".to_owned(),
                                    )),
                                }),
                                manufacturer: None,
                                username: None,
                                password: None,
                                onvif_port: None,
                                http_port: None,
                                main_rtsp_url: Some(proto::OptionalStringUpdate {
                                    value: Some(optional_string_update::Value::Clear(true)),
                                }),
                                sub_rtsp_url: None,
                                uid: None,
                                backend: None,
                                transport: None,
                                record_generic_motion_events: None,
                                recording_mode: Some(proto::CameraRecordingMode::Off as i32),
                                event_recording_duration_secs: Some(90),
                            },
                        )),
                    },
                )),
            })
            .response;

        assert_eq!(router_thread.join().unwrap(), 1);
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("valid camera patch must succeed");
        };
        let Some(control_ok::Result::CameraConfigurationResult(result)) = ok.result else {
            panic!("camera patch must return camera configuration");
        };
        let camera = result.camera.expect("camera patch must return the camera");
        assert_eq!(camera.display_name.as_deref(), Some("Updated Gate"));
        assert!(camera.password_configured);
        assert_eq!(camera.main_rtsp_url, None);
        assert_eq!(
            camera.sub_rtsp_url.as_deref(),
            Some("rtsp://{secret:GATE_HOST}/sub")
        );
        assert_eq!(
            camera.recording_mode,
            proto::CameraRecordingMode::Off as i32
        );
        assert_eq!(camera.event_recording_duration_secs, 90);
        let persisted = crate::config::load_cameras(&config_path).unwrap();
        assert_eq!(persisted["cameras"][0].password, "preserved-secret");
        assert_eq!(
            persisted["cameras"][0].recording_mode,
            CameraRecordingMode::Off
        );
        assert_eq!(persisted["cameras"][0].event_recording_duration_secs, 90);
        let raw = std::fs::read_to_string(&config_path).unwrap();
        assert!(raw.contains("password = \"{secret:GATE_PASSWORD}\""));
        assert!(raw.contains("sub_rtsp_url = \"rtsp://{secret:GATE_HOST}/sub\""));
        assert!(!raw.contains("preserved-secret"));

        let removed = handler
            .handle(proto::Request {
                request_id: 83,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::Remove(
                            proto::RemoveCameraConfiguration {
                                ip: "192.0.2.77".to_owned(),
                            },
                        )),
                    },
                )),
            })
            .response;
        let Some(control_response::Result::Ok(ok)) = removed.result else {
            panic!("camera remove must succeed");
        };
        assert!(matches!(
            ok.result,
            Some(control_ok::Result::CameraConfigurationResult(
                proto::CameraConfigurationResult { removed: true, .. }
            ))
        ));
        assert!(
            crate::config::load_cameras(&config_path)
                .unwrap()
                .get("cameras")
                .is_none_or(Vec::is_empty)
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn data_channel_new_camera_update_persists_references_without_resolved_values() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-control-new-camera-secret-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        crate::config::write_private_file(&config_path, b"").unwrap();
        crate::config::write_private_file(
            &crate::config::secrets_path(&config_path),
            br#"
                CAMERA_USERNAME = "operator"
                CAMERA_PASSWORD = "resolved-camera-password"
                CAMERA_HOST = "192.0.2.88"
            "#,
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let handler = ServerControlHandler::new(state, router_tx);
        let response = handler
            .handle(proto::Request {
                request_id: 82,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::Update(
                            proto::UpdateCameraConfiguration {
                                ip: "192.0.2.88".to_owned(),
                                display_name: Some(proto::OptionalStringUpdate {
                                    value: Some(optional_string_update::Value::Set(
                                        "Front Gate".to_owned(),
                                    )),
                                }),
                                manufacturer: None,
                                username: Some("{secret:CAMERA_USERNAME}".to_owned()),
                                password: Some("{secret:CAMERA_PASSWORD}".to_owned()),
                                onvif_port: None,
                                http_port: None,
                                main_rtsp_url: Some(proto::OptionalStringUpdate {
                                    value: Some(optional_string_update::Value::Set(
                                        "rtsp://{secret:CAMERA_HOST}/main".to_owned(),
                                    )),
                                }),
                                sub_rtsp_url: None,
                                uid: None,
                                backend: None,
                                transport: None,
                                record_generic_motion_events: None,
                                recording_mode: None,
                                event_recording_duration_secs: None,
                            },
                        )),
                    },
                )),
            })
            .response;

        assert_eq!(router_thread.join().unwrap(), 1);
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("new camera update with valid references must succeed");
        };
        let Some(control_ok::Result::CameraConfigurationResult(result)) = ok.result else {
            panic!("new camera update must return camera configuration");
        };
        let camera = result
            .camera
            .expect("new camera result must include the camera");
        assert_eq!(
            camera.main_rtsp_url.as_deref(),
            Some("rtsp://{secret:CAMERA_HOST}/main")
        );
        let raw = std::fs::read_to_string(&config_path).unwrap();
        assert!(raw.contains("username = \"{secret:CAMERA_USERNAME}\""));
        assert!(raw.contains("password = \"{secret:CAMERA_PASSWORD}\""));
        assert!(raw.contains("main_rtsp_url = \"rtsp://{secret:CAMERA_HOST}/main\""));
        assert!(!raw.contains("operator"));
        assert!(!raw.contains("resolved-camera-password"));
        let loaded = crate::config::load_cameras(&config_path).unwrap();
        let camera = &loaded["cameras"][0];
        assert_eq!(camera.username, "operator");
        assert_eq!(camera.password, "resolved-camera-password");
        assert_eq!(
            camera.main_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.88/main")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn data_channel_runtime_configuration_requires_storage_contract() {
        let handler = test_control_handler(ServerState::empty());
        let response = handler
            .handle(proto::Request {
                request_id: 85,
                command: Some(control_request::Command::RuntimeConfigurationCommand(
                    proto::RuntimeConfigurationCommand {
                        action: Some(runtime_configuration_command::Action::Update(
                            proto::UpdateRuntimeConfiguration {
                                host: "127.0.0.1".to_owned(),
                                port: 3000,
                                storage: None,
                                move_existing_recordings: false,
                            },
                        )),
                    },
                )),
            })
            .response;

        assert_eq!(response.request_id, 85);
        let Some(control_response::Result::Error(error)) = response.result else {
            panic!("runtime command without storage must fail");
        };
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);
        assert!(error.message.contains("requires storage"));
    }

    #[test]
    fn storage_write_probe_flushes_renames_and_cleans_up() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-storage-write-probe-{}",
            rand::random::<u64>()
        ));

        storage_write_probe(&directory).unwrap();

        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn data_channel_runtime_configuration_get_returns_sanitized_evidence() {
        let handler = test_control_handler(ServerState::empty());
        let response = handler
            .handle(proto::Request {
                request_id: 87,
                command: Some(control_request::Command::RuntimeConfigurationCommand(
                    proto::RuntimeConfigurationCommand {
                        action: Some(runtime_configuration_command::Action::Get(
                            proto::GetRuntimeConfiguration {},
                        )),
                    },
                )),
            })
            .response;

        assert_eq!(response.request_id, 87);
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("runtime configuration get must succeed");
        };
        let Some(control_ok::Result::RuntimeConfigurationResult(result)) = ok.result else {
            panic!("runtime configuration get must return sanitized configuration");
        };
        assert!(!result.restart_required);
        let config = result
            .config
            .expect("runtime configuration must be present");
        assert_eq!(config.host, "0.0.0.0");
        assert!(config.storage.is_some());
        assert!(config.recording_estimate.is_some());
    }

    #[test]
    fn runtime_configuration_returns_references_instead_of_secret_values() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-runtime-secret-response-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        crate::config::write_private_file(
            &config_path,
            br#"
                host = "{secret:BIND_HOST}"

                [storage]
                medium_term_path = "{secret:RECORDING_PATH}"
                long_term_path = "{secret:RECORDING_PATH}"
            "#,
        )
        .unwrap();
        crate::config::write_private_file(
            &crate::config::secrets_path(&config_path),
            br#"
                BIND_HOST = "127.0.0.1"
                RECORDING_PATH = "/private/recordings"
            "#,
        )
        .unwrap();
        let handler =
            test_control_handler(ServerState::empty().with_camera_config_path(config_path));

        let response = handler
            .handle(proto::Request {
                request_id: 871,
                command: Some(control_request::Command::RuntimeConfigurationCommand(
                    proto::RuntimeConfigurationCommand {
                        action: Some(runtime_configuration_command::Action::Get(
                            proto::GetRuntimeConfiguration {},
                        )),
                    },
                )),
            })
            .response;
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("runtime configuration get must succeed");
        };
        let Some(control_ok::Result::RuntimeConfigurationResult(result)) = ok.result else {
            panic!("runtime configuration get must return sanitized configuration");
        };
        let config = result
            .config
            .expect("runtime configuration must be present");
        let storage = config
            .storage
            .as_ref()
            .expect("storage configuration must be present");
        assert_eq!(config.host, "{secret:BIND_HOST}");
        assert_eq!(storage.long_term_path, "{secret:RECORDING_PATH}");
        let debug = format!("{config:?}");
        assert!(!debug.contains("127.0.0.1"));
        assert!(!debug.contains("/private/recordings"));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn data_channel_health_get_returns_typed_snapshot() {
        let state = ServerState::empty();
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let response = ServerControlHandler::new(state, router_tx)
            .handle(proto::Request {
                request_id: 88,
                command: Some(control_request::Command::HealthCommand(
                    proto::HealthCommand {
                        action: Some(health_command::Action::Get(proto::GetHealth {})),
                    },
                )),
            })
            .response;

        assert_eq!(router_thread.join().unwrap(), 1);
        assert_eq!(response.request_id, 88);
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("health get must succeed");
        };
        let Some(control_ok::Result::HealthResult(health)) = ok.result else {
            panic!("health get must return a typed snapshot");
        };
        let has_degrading_issue = health
            .issues
            .iter()
            .any(|issue| matches!(issue.severity.as_str(), "critical" | "warning"));
        assert_eq!(
            health.status,
            if has_degrading_issue {
                "degraded"
            } else {
                "healthy"
            }
        );
        assert_eq!(health.version, env!("CARGO_PKG_VERSION"));
        assert!(health.generated_at_ms > 0);
        assert_eq!(
            health
                .totals
                .as_ref()
                .map(|totals| totals.configured_cameras),
            Some(0)
        );
        assert_eq!(
            health
                .system
                .as_ref()
                .and_then(|system| system.process.as_ref())
                .map(|process| process.pid),
            Some(std::process::id())
        );
        assert!(
            health
                .storage
                .as_ref()
                .and_then(|storage| storage.demand.as_ref())
                .is_some()
        );
        assert!(health.webrtc.is_some());
        assert!(health.cameras.is_empty());
    }

    #[test]
    fn embedded_ui_serves_root_and_client_routes() {
        let root = serve_ui(&Request::fake_http("GET", "/", Vec::new(), Vec::new()));
        assert_eq!(root.status_code, 200);
        assert!(root.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Content-Type") && value.starts_with("text/html")
        }));
        let root_body = response_data(root);
        assert!(!root_body.is_empty());

        let route = serve_ui(&Request::fake_http(
            "GET",
            "/camera?camera=front-door",
            Vec::new(),
            Vec::new(),
        ));
        assert_eq!(route.status_code, 200);
        assert_eq!(response_data(route), root_body);
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

        let response = read_http_until(address, "/logs?tail=10", "low-level server log");

        shutdown.cancel();
        server.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(filter_file.path().parent().unwrap());

        let stream = response.unwrap();

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
        write!(stream, "GET /logs HTTP/1.1\r\nHost: {address}\r\n\r\n").unwrap();
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
    fn logging_filter_update_persists_and_applies_valid_directive() {
        let (state, logging, dispatch, filter_file) = logging_test_state("error");
        let settings = tracing::dispatcher::with_default(&dispatch, || {
            let settings = set_logging_filter(&state, "info,str0m=warn").unwrap();
            tracing::info!(target: "keeppeek::test", "included after update");
            tracing::info!(target: "str0m", "filtered after update");
            tracing::warn!(target: "str0m", "included after update");
            settings
        });

        assert_eq!(settings.active_filter, "info,str0m=warn");
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
    fn logging_filter_update_rejects_invalid_directive_without_mutation() {
        let (state, logging, dispatch, filter_file) = logging_test_state("warn");
        filter_file.write_log_filter("warn").unwrap();
        let error = tracing::dispatcher::with_default(&dispatch, || {
            let error = set_logging_filter(&state, "keeppeek=verbose").unwrap_err();
            tracing::info!(target: "keeppeek::test", "still filtered");
            tracing::warn!(target: "keeppeek::test", "still included");
            error
        });

        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
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
    fn log_snapshot_applies_cursor_and_limit() {
        let (_state, logging, dispatch, filter_file) = logging_test_state("trace");
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(target: "keeppeek::test", "one");
            tracing::info!(target: "keeppeek::test", "two");
            tracing::info!(target: "keeppeek::test", "three");
        });
        let snapshot = logging.snapshot(Some(1), 1);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].sequence, 2);
        assert_eq!(snapshot.entries[0].message, "two");
        assert!(snapshot.truncated);
        std::fs::remove_dir_all(filter_file.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn log_snapshot_route_returns_the_retained_buffer_as_json() {
        let (state, _logging, dispatch, filter_file) = logging_test_state("trace");
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(target: "keeppeek::test", "one");
            tracing::warn!(target: "keeppeek::test", "two");
        });

        let response = handle_request(
            &Request::fake_http("GET", "/logs/snapshot", Vec::new(), Vec::new()),
            &router_tx,
            &state,
        );

        assert_eq!(response.status_code, 200);
        assert!(response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Content-Type") && value.starts_with("application/json")
        }));
        assert!(response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Cache-Control") && value == "no-store"
        }));
        let snapshot: crate::logging::LogSnapshot =
            serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.entries[0].message, "one");
        assert_eq!(snapshot.entries[1].message, "two");
        assert!(!snapshot.truncated);
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
            "/logs",
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
    fn health_command_returns_runtime_and_resource_sections() {
        let state = ServerState::empty();
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let response = ServerControlHandler::new(state, router_tx)
            .handle(proto::Request {
                request_id: 95,
                command: Some(control_request::Command::HealthCommand(
                    proto::HealthCommand {
                        action: Some(health_command::Action::Get(proto::GetHealth {})),
                    },
                )),
            })
            .response;

        assert_eq!(router_thread.join().unwrap(), 1);
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("health command must succeed");
        };
        let Some(control_ok::Result::HealthResult(health)) = ok.result else {
            panic!("health command must return a typed snapshot");
        };
        assert!(matches!(health.status.as_str(), "healthy" | "degraded"));
        let system = health.system.expect("health must include system evidence");
        let process = system
            .process
            .expect("health must include process evidence");
        assert_eq!(process.pid, std::process::id());
        assert!(process.cpu_capacity_percent.is_some());
        assert!(process.cpu_core_equivalents.is_some());
        assert!(process.memory_capacity_percent.is_some());
        assert!(system.memory.is_some());
        let storage = health
            .storage
            .expect("health must include storage evidence");
        assert!(storage.demand.is_some());
        assert!(health.webrtc.is_some());
    }

    #[test]
    fn metrics_route_returns_prometheus_text_exposition() {
        let state = ServerState::empty();
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let request = Request::fake_http("GET", "/metrics", Vec::new(), Vec::new());

        let response = handle_request(&request, &router_tx, &state);

        assert_eq!(response.status_code, 200);
        assert!(response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Content-Type")
                && value == "text/plain; version=0.0.4; charset=utf-8"
        }));
        let body = String::from_utf8(response_data(response)).unwrap();
        assert!(body.contains("# HELP keeppeek_server_info"));
        assert!(body.contains("# TYPE keeppeek_ingress_frames counter"));
        assert!(body.contains("keeppeek_ingress_frames_total 0"));
        assert!(body.contains("keeppeek_system_memory_total_bytes"));
        assert!(body.contains("keeppeek_webrtc_active_sessions"));
        assert_eq!(router_thread.join().unwrap(), 1);
    }

    #[test]
    fn health_command_reports_bytes_for_a_custom_recording_catalog_path() {
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
            ..Config::default()
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
        let response = ServerControlHandler::new(state, router_tx)
            .handle(proto::Request {
                request_id: 97,
                command: Some(control_request::Command::HealthCommand(
                    proto::HealthCommand {
                        action: Some(health_command::Action::Get(proto::GetHealth {})),
                    },
                )),
            })
            .response;

        assert_eq!(router_thread.join().unwrap(), 1);
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("health command must succeed");
        };
        let Some(control_ok::Result::HealthResult(health)) = ok.result else {
            panic!("health command must return a typed snapshot");
        };
        assert_eq!(
            health.storage.and_then(|storage| storage.catalog_bytes),
            Some(7)
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn configured_camera_capabilities_are_visible_while_lifecycle_is_starting() {
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
            record_generic_motion_events: false,
            recording_mode: Default::default(),
            event_recording_duration_secs: 60,
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
        router_tx
            .send(RouterMessage::WorkerEvent(
                crate::runtime::WorkerEvent::StatusChanged(crate::api::CameraStatus {
                    id: crate::api::CameraId::new("north"),
                    lifecycle: crate::api::CameraLifecycle::Starting,
                    expected_streams: Vec::new(),
                    connected_streams: Vec::new(),
                    last_error: None,
                }),
            ))
            .unwrap();
        assert_eq!(router.wait_and_drain(Some(Duration::ZERO)).unwrap(), 1);

        let handler = ServerControlHandler::new(state, router_tx);
        let capabilities = handler
            .initial_capabilities(SessionId::from_u64(31))
            .expect("server handler must provide initial capabilities");
        let camera = capabilities
            .cameras
            .iter()
            .find(|camera| camera.source_id == "192.0.2.10")
            .expect("configured camera must be advertised while starting");
        assert_eq!(camera.display_name, "North Courtyard");
        let stored_source = capabilities
            .stored_media_sources
            .iter()
            .find(|source| source.source_id == "192.0.2.10")
            .expect("configured camera streams must be advertised while starting");
        assert_eq!(stored_source.streams.len(), 2);

        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let settings = handler
            .handle(proto::Request {
                request_id: 89,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::Get(
                            proto::GetCameraConfigurations {},
                        )),
                    },
                )),
            })
            .response;
        let Some(control_response::Result::Ok(ok)) = settings.result else {
            panic!("camera configuration get must succeed");
        };
        let Some(control_ok::Result::CameraConfigurationResult(result)) = ok.result else {
            panic!("camera configuration get must return settings");
        };
        assert_eq!(result.cameras.len(), 1);
        assert_eq!(
            result.cameras[0].display_name.as_deref(),
            Some("North Courtyard")
        );
        assert_eq!(result.cameras[0].health.as_deref(), Some("starting"));
        assert!(result.cameras[0].username_configured);
        assert!(result.cameras[0].password_configured);
        assert_eq!(router_thread.join().unwrap(), 1);
    }

    #[test]
    fn onvif_metadata_enrichment_updates_camera_identity_and_profiles() {
        let config = CameraConfig {
            ip: "192.0.2.81".parse().unwrap(),
            name: Some("front_gate".to_owned()),
            display_name: Some("Front Gate".to_owned()),
            manufacturer: None,
            username: "operator".to_owned(),
            password: "camera-password".to_owned(),
            onvif_port: Some(8000),
            http_port: Some(80),
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::ReoProto,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: Default::default(),
            event_recording_duration_secs: 60,
        };
        let configs = HashMap::from([("cameras".to_owned(), vec![config.clone()])]);
        let state = ServerState::new(
            &Config::default(),
            &configs,
            &HashMap::new(),
            &StorageConfig::default(),
            RecordingDemand::new(TEST_RECORDING_DEMAND_GRACE),
            WebRtc::new(),
        )
        .with_test_camera_catalog(
            TestCameraCatalog::new([crate::test_support::TestCatalogCamera::new(
                "reolink-rlc-820a",
                "Reolink",
                "RLC-820A",
            )])
            .unwrap(),
        );
        let probe = crate::cameras::ProbedOnvifCamera {
            onvif_port: 8000,
            device: crate::cameras::DeviceInfo {
                manufacturer: Some("Manufacturer".to_owned()),
                model: Some("RLC-820A".to_owned()),
                firmware_version: Some("v3.1".to_owned()),
                serial_number: Some("serial-81".to_owned()),
                hardware_id: Some("IPC".to_owned()),
                p2p_uid: None,
            },
            profiles: vec![crate::cameras::MediaProfile {
                token: "main".to_owned(),
                name: "Main".to_owned(),
                stream_uri: None,
                snapshot_uri: None,
                video: Some(crate::cameras::VideoConfig {
                    encoding: crate::cameras::VideoEncoding::H265,
                    width: 3840,
                    height: 2160,
                    framerate: 25.0,
                    bitrate_kbps: Some(8192),
                    quality: None,
                    gov_length: Some(25),
                    h264_profile: None,
                }),
                audio: None,
            }],
            main_rtsp_url: None,
            sub_rtsp_url: None,
        };

        state.apply_camera_metadata(config.ip, &probe);

        let camera = state.camera("192.0.2.81").unwrap();
        assert_eq!(camera.info.manufacturer.as_deref(), Some("Reolink"));
        assert_eq!(camera.info.model.as_deref(), Some("RLC-820A"));
        assert_eq!(camera.info.firmware_version.as_deref(), Some("v3.1"));
        assert_eq!(camera.info.serial_number.as_deref(), Some("serial-81"));
        assert_eq!(camera.info.hardware_id.as_deref(), Some("IPC"));
        assert_eq!(camera.info.profiles[0].encoding.as_deref(), Some("h265"));
        assert_eq!(
            camera.info.profiles[0].resolution.as_deref(),
            Some("3840x2160")
        );
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
        assert!(recording_mode_includes_stream(
            CameraRecordingMode::EventBoost,
            "sub"
        ));
        assert!(!recording_mode_includes_stream(
            CameraRecordingMode::EventBoost,
            "main"
        ));
        assert!(recording_mode_includes_stream(
            CameraRecordingMode::Both,
            "main"
        ));
        assert!(!recording_mode_includes_stream(
            CameraRecordingMode::Off,
            "sub"
        ));
        assert!(!recording_mode_includes_stream(
            CameraRecordingMode::Off,
            "main"
        ));
    }

    #[test]
    fn camera_snapshots_include_configured_transport_profiles_and_motion() {
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
            record_generic_motion_events: false,
            recording_mode: Default::default(),
            event_recording_duration_secs: 60,
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
        let handler = ServerControlHandler::new(state, router_tx);
        let capabilities = handler
            .initial_capabilities(SessionId::from_u64(101))
            .expect("server handler must provide initial capabilities");
        let camera = capabilities
            .cameras
            .iter()
            .find(|camera| camera.source_id == "192.0.2.41")
            .expect("configured camera must be advertised");
        assert_eq!(camera.web_url.as_deref(), Some("http://192.0.2.41:8080"));
        let stored = capabilities
            .stored_media_sources
            .iter()
            .find(|source| source.source_id == "192.0.2.41")
            .expect("configured camera streams must be advertised");
        assert_eq!(stored.streams.len(), 2);

        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let health = handler
            .handle(proto::Request {
                request_id: 103,
                command: Some(control_request::Command::HealthCommand(
                    proto::HealthCommand {
                        action: Some(health_command::Action::Get(proto::GetHealth {})),
                    },
                )),
            })
            .response;
        assert_eq!(router_thread.join().unwrap(), 1);
        let Some(control_response::Result::Ok(ok)) = health.result else {
            panic!("camera health read must succeed");
        };
        let Some(control_ok::Result::HealthResult(health)) = ok.result else {
            panic!("camera health read must return typed health");
        };
        let camera = health
            .cameras
            .iter()
            .find(|camera| camera.id == "192.0.2.41")
            .expect("configured camera health must be present");
        assert_eq!(camera.backend, "retina");
        assert_eq!(camera.transport, "udp");
        assert_eq!(camera.configured_profiles.len(), 2);

        let motion = handler
            .handle(proto::Request {
                request_id: 105,
                command: Some(control_request::Command::CameraControlCommand(
                    proto::CameraControlCommand {
                        action: Some(camera_control_command::Action::GetMotionDetection(
                            proto::GetMotionDetection {
                                source_id: "192.0.2.41".to_owned(),
                            },
                        )),
                    },
                )),
            })
            .response;
        let Some(control_response::Result::Ok(ok)) = motion.result else {
            panic!("camera motion read must succeed");
        };
        let Some(control_ok::Result::MotionDetectionResult(motion)) = ok.result else {
            panic!("camera motion read must return motion evidence");
        };
        assert!(!motion.controllable);
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
                record_generic_motion_events: false,
                recording_mode: Default::default(),
                event_recording_duration_secs: 60,
            },
            battery_uid: None,
            recording_label: "back-yard".to_owned(),
            control: None,
        }]));
        let updated =
            set_camera_manufacturer(&state, "192.0.2.55", Some("Hikvision".to_owned())).unwrap();
        assert_eq!(updated.manufacturer.as_deref(), Some("Hikvision"));
        let saved: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            saved["cameras"]["back_yard"]["manufacturer"].as_str(),
            Some("Hikvision")
        );

        let restored = set_camera_manufacturer(&state, "192.0.2.55", None).unwrap();
        assert_eq!(restored.manufacturer.as_deref(), Some("ONVIF"));
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

        let saved_response = save_camera_settings(
            CameraSettingsUpdate {
                display_name: Some(Some("Manual Gate".to_owned())),
                manufacturer: Some(Some("Reolink".to_owned())),
                username: Some("operator".to_owned()),
                password: Some("not-in-the-response".to_owned()),
                onvif_port: Some(Some(8080)),
                main_rtsp_url: Some(Some("rtsp://192.0.2.77:8554/live/main".to_owned())),
                sub_rtsp_url: Some(Some("rtsp://192.0.2.77:8554/live/sub".to_owned())),
                backend: Some(CameraBackend::ReoProto),
                transport: Some(CameraTransport::Udp),
                record_generic_motion_events: Some(true),
                ..CameraSettingsUpdate::default()
            },
            &router_tx,
            &state,
            "192.0.2.77",
        )
        .unwrap();
        assert_eq!(saved_response.camera.ip, "192.0.2.77");
        assert!(saved_response.camera.password_configured);
        assert!(saved_response.camera.record_generic_motion_events);
        assert!(saved_response.restart_required);
        assert_eq!(router_thread.join().unwrap(), 1);

        let cameras = crate::config::load_cameras(&config_path).unwrap();
        let config = &cameras["cameras"][0];
        assert_eq!(config.ip, "192.0.2.77".parse::<IpAddr>().unwrap());
        assert_eq!(config.password, "not-in-the-response");
        assert_eq!(config.display_name.as_deref(), Some("Manual Gate"));
        assert_eq!(config.backend, CameraBackend::ReoProto);
        assert_eq!(config.transport, CameraTransport::Udp);
        assert!(config.record_generic_motion_events);
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
        let response = ServerControlHandler::new(state.clone(), router_tx)
            .handle(proto::Request {
                request_id: 91,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::Get(
                            proto::GetCameraConfigurations {},
                        )),
                    },
                )),
            })
            .response;
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("persisted camera settings read must succeed");
        };
        let Some(control_ok::Result::CameraConfigurationResult(result)) = ok.result else {
            panic!("persisted camera settings read must return configuration");
        };
        assert_eq!(result.cameras.len(), 1);
        assert_eq!(result.cameras[0].ip, "192.0.2.77");
        assert!(result.cameras[0].password_configured);
        assert!(result.cameras[0].record_generic_motion_events);
        assert_eq!(
            result.cameras[0].main_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.77:8554/live/main")
        );
        assert_eq!(
            result.cameras[0].sub_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.77:8554/live/sub")
        );
        assert_eq!(router_thread.join().unwrap(), 1);

        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        save_camera_settings(
            CameraSettingsUpdate {
                display_name: Some(Some("Updated Manual Gate".to_owned())),
                ..CameraSettingsUpdate::default()
            },
            &router_tx,
            &state,
            "192.0.2.77",
        )
        .unwrap();
        assert_eq!(router_thread.join().unwrap(), 1);
        let cameras = crate::config::load_cameras(&config_path).unwrap();
        let config = &cameras["cameras"][0];
        assert_eq!(config.display_name.as_deref(), Some("Updated Manual Gate"));
        assert_eq!(config.password, "not-in-the-response");
        assert!(config.record_generic_motion_events);
        assert_eq!(
            config.main_rtsp_url.as_deref(),
            Some("rtsp://192.0.2.77:8554/live/main")
        );

        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        save_camera_settings(
            CameraSettingsUpdate {
                main_rtsp_url: Some(None),
                sub_rtsp_url: Some(None),
                ..CameraSettingsUpdate::default()
            },
            &router_tx,
            &state,
            "192.0.2.77",
        )
        .unwrap();
        assert_eq!(router_thread.join().unwrap(), 1);
        let cameras = crate::config::load_cameras(&config_path).unwrap();
        let config = &cameras["cameras"][0];
        assert_eq!(config.main_rtsp_url, None);
        assert_eq!(config.sub_rtsp_url, None);

        delete_camera_settings(&state, "192.0.2.77").unwrap();
        assert!(
            crate::config::load_cameras(&config_path)
                .unwrap()
                .get("cameras")
                .is_none_or(Vec::is_empty)
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn runtime_camera_starts_and_restarts_without_process_restart() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-runtime-camera-{}", rand::random::<u64>()));
        let config_path = directory.join("config.toml");
        crate::config::write_private_file(&config_path, b"[cameras]\n").unwrap();

        let shutdown = Shutdown::new();
        let loop_ = crate::keeppeek::KeepPeekLoop::new(shutdown.clone(), None);
        let runtime = loop_.control();
        let runtime_thread = std::thread::spawn(move || loop_.run());
        let state = ServerState::empty()
            .with_camera_config_path(config_path)
            .with_camera_runtime(runtime);
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });

        let saved = save_camera_settings(
            CameraSettingsUpdate {
                display_name: Some(Some("Front Gate".to_owned())),
                username: Some("operator".to_owned()),
                password: Some("camera-password".to_owned()),
                backend: Some(CameraBackend::ReoProto),
                ..CameraSettingsUpdate::default()
            },
            &router_tx,
            &state,
            "192.0.2.79",
        )
        .unwrap();

        assert!(!saved.restart_required);
        assert_eq!(saved.camera.health.as_deref(), Some("starting"));
        let camera = state.camera("192.0.2.79").unwrap();
        assert_eq!(camera.recording_label, "front_gate");
        assert_eq!(camera.configuration.name.as_deref(), Some("front_gate"));
        assert_eq!(router_thread.join().unwrap(), 1);

        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let updated = save_camera_settings(
            CameraSettingsUpdate {
                display_name: Some(Some("Front Gate Updated".to_owned())),
                recording_mode: Some(CameraRecordingMode::Main),
                event_recording_duration_secs: Some(90),
                ..CameraSettingsUpdate::default()
            },
            &router_tx,
            &state,
            "192.0.2.79",
        )
        .unwrap();

        assert!(!updated.restart_required);
        assert_eq!(
            updated.camera.display_name.as_deref(),
            Some("Front Gate Updated")
        );
        assert_eq!(updated.camera.recording_mode, "main");

        shutdown.cancel();
        runtime_thread.join().unwrap();
        router_thread.join().unwrap();
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
        let saved = save_runtime_settings(
            RuntimeSettingsUpdate {
                host: "127.0.0.1".to_owned(),
                port: 3200,
                storage: RuntimeStorageSettingsUpdate {
                    medium_term_path: "/media/new-keeppeek".to_owned(),
                    long_term_path: "/archive/new-keeppeek".to_owned(),
                    recording_catalog_path: "/metadata/new-recordings.db".to_owned(),
                    event_thumbnail_path: "/metadata/new-thumbnails".to_owned(),
                    event_thumbnail_max_mb: 512,
                    short_term_secs: 30,
                    medium_term_secs: 120,
                    flush_interval_secs: 15,
                    write_buffer_bytes: 16_384,
                    long_term_max_gb: 24,
                },
                move_existing_recordings: false,
            },
            &state,
        )
        .unwrap();

        assert_eq!(saved.config.host, "127.0.0.1");
        assert_eq!(saved.config.port, 3200);
        assert_eq!(saved.config.storage.medium_term_path, "/media/new-keeppeek");
        assert_eq!(saved.config.storage.long_term_path, "/archive/new-keeppeek");
        assert_eq!(
            saved.config.storage.recording_catalog_path,
            "/metadata/new-recordings.db"
        );
        assert_eq!(
            saved.config.storage.event_thumbnail_path,
            "/metadata/new-thumbnails"
        );
        assert_eq!(saved.config.storage.event_thumbnail_max_mb, 512);
        assert_eq!(saved.config.storage.medium_term_secs, 120);
        assert!(saved.restart_required);

        let response = ServerControlHandler::new(state.clone(), router_tx)
            .handle(proto::Request {
                request_id: 93,
                command: Some(control_request::Command::RuntimeConfigurationCommand(
                    proto::RuntimeConfigurationCommand {
                        action: Some(runtime_configuration_command::Action::Get(
                            proto::GetRuntimeConfiguration {},
                        )),
                    },
                )),
            })
            .response;
        let Some(control_response::Result::Ok(ok)) = response.result else {
            panic!("persisted runtime configuration read must succeed");
        };
        let Some(control_ok::Result::RuntimeConfigurationResult(result)) = ok.result else {
            panic!("persisted runtime configuration read must return configuration");
        };
        let persisted = result
            .config
            .expect("persisted runtime configuration must be present");
        let storage = persisted
            .storage
            .expect("persisted runtime storage must be present");
        assert_eq!(persisted.host, "127.0.0.1");
        assert_eq!(persisted.port, 3200);
        assert_eq!(storage.medium_term_path, "/media/new-keeppeek");
        assert_eq!(storage.long_term_path, "/archive/new-keeppeek");
        assert_eq!(
            storage.recording_catalog_path,
            "/metadata/new-recordings.db"
        );
        assert_eq!(storage.event_thumbnail_path, "/metadata/new-thumbnails");
        assert_eq!(storage.event_thumbnail_max_mb, 512);
        assert_eq!(persisted.camera_count, 1);

        let Err(error) = save_runtime_settings(
            RuntimeSettingsUpdate {
                host: "127.0.0.1".to_owned(),
                port: 0,
                storage: RuntimeStorageSettingsUpdate {
                    medium_term_path: "/media/invalid".to_owned(),
                    long_term_path: "/archive/invalid".to_owned(),
                    recording_catalog_path: "/metadata/invalid-recordings.db".to_owned(),
                    event_thumbnail_path: "/metadata/invalid-thumbnails".to_owned(),
                    event_thumbnail_max_mb: 512,
                    short_term_secs: 30,
                    medium_term_secs: 120,
                    flush_interval_secs: 15,
                    write_buffer_bytes: 16_384,
                    long_term_max_gb: 24,
                },
                move_existing_recordings: false,
            },
            &state,
        ) else {
            panic!("zero runtime port must be rejected");
        };
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
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
            ..Config::default()
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
        let response = save_runtime_settings(
            RuntimeSettingsUpdate {
                host: "0.0.0.0".to_owned(),
                port: 3000,
                storage: RuntimeStorageSettingsUpdate {
                    medium_term_path: next.to_string_lossy().into_owned(),
                    long_term_path: next.to_string_lossy().into_owned(),
                    recording_catalog_path: current
                        .join("recordings.db")
                        .to_string_lossy()
                        .into_owned(),
                    event_thumbnail_path: current
                        .join(".event-thumbnails")
                        .to_string_lossy()
                        .into_owned(),
                    event_thumbnail_max_mb: 1024,
                    short_term_secs: 120,
                    medium_term_secs: 1800,
                    flush_interval_secs: 60,
                    write_buffer_bytes: 8192,
                    long_term_max_gb: 0,
                },
                move_existing_recordings: true,
            },
            &state,
        )
        .unwrap();
        let expected_catalog_path = next.join("recordings.db").to_string_lossy().into_owned();
        let expected_thumbnail_path = next
            .join(".event-thumbnails")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            response.config.storage.recording_catalog_path,
            expected_catalog_path
        );
        assert_eq!(
            response.config.storage.event_thumbnail_path,
            expected_thumbnail_path
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
            ..Config::default()
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
        save_runtime_settings(
            RuntimeSettingsUpdate {
                host: "0.0.0.0".to_owned(),
                port: 3000,
                storage: RuntimeStorageSettingsUpdate {
                    medium_term_path: next_recordings.to_string_lossy().into_owned(),
                    long_term_path: next_recordings.to_string_lossy().into_owned(),
                    recording_catalog_path: next_catalog.to_string_lossy().into_owned(),
                    event_thumbnail_path: next_thumbnails.to_string_lossy().into_owned(),
                    event_thumbnail_max_mb: 512,
                    short_term_secs: 120,
                    medium_term_secs: 1800,
                    flush_interval_secs: 60,
                    write_buffer_bytes: 8192,
                    long_term_max_gb: 0,
                },
                move_existing_recordings: true,
            },
            &state,
        )
        .unwrap();
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
    fn settings_discovery_rejects_excessive_subnets_before_network_probing() {
        let subnets = (0_u8..33).collect::<Vec<_>>();
        let state = ServerState::empty();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        let Err(error) = discover_camera_settings(Vec::new(), subnets, &router_tx, &state, None)
        else {
            panic!("excessive discovery subnets must be rejected");
        };

        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
    }
}
