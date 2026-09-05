use crate::api::proto::{
    self, camera_configuration_command, camera_control_command, event_publication_command,
    event_search_command, logging_command, notification_rule_command, notification_rule_result,
    ok as control_ok, optional_string_update, request as control_request,
    response as control_response, runtime_configuration_command, server_command,
};
use crate::{
    access::{
        AccessAuditEvent, AccessKey, AccessManager, AccessRole, AuthenticatedCredential,
        AuthenticationFailure, ClientClassification, ClientClassificationReason,
        CredentialMetadata, IssuedCredential, NetworkAccessPolicy, NewAccessAuditEvent,
    },
    api::{
        ApiError, AudioProfileSummary, CameraInfo, CameraLifecycle, CameraStatus, CreateRequest,
        CreateResponse, DeleteRequest, MotionDetection, ProfileSummary, RecordingCapacityEstimate,
        SanitizedConfig, SanitizedStorage, SdpAnswer as ApiSdpAnswer, Status,
    },
    backup::BackupManager,
    battery_wake::BatteryWakeHandle,
    camera_database::{CameraDatabase, CameraMatch, CatalogCamera, StreamHints},
    cameras::{
        Camera, CameraBackend, CameraConfig, CameraPorts, CameraRecordingMode, CameraTransport,
        MediaProfile, probe_onvif_camera,
        reolink::{PtzOp, ReolinkClient},
    },
    config::{self, Config, StorageMigration, StorageMigrationPaths, StorageToml},
    event_forwarder::Handle as EventForwarderHandle,
    health::{
        CAMERA_HEALTH_CONTRACT_VERSION, CameraHealth, CameraHealthDimensions, CameraHealthEvidence,
        CameraHealthState, HealthIssue, HealthTotals, STREAM_REPORT_FRESHNESS_THRESHOLD_MS,
        ServerHealthResponse, StorageHealth, StreamHealth, StreamHealthDimensions, SystemMonitor,
        project_camera_health,
    },
    keeppeek::{KeepPeekControl, StreamKind},
    logging::{LogStreamError, LoggingService, LoggingSettings},
    notifications::{
        AttemptRecord, ClearScope, Handle as NotificationHandle, HistoryEvent, HistoryGroup, Inbox,
        NotificationItem, RuleRecord, RuleStoreError, Stage, model::Rule as NotificationRule,
    },
    rtsp::{RtspTransport, probe_rtsp_video},
    runtime::{
        FacadeSendError, FacadeSender, RouterError, RouterMessage, RouterQuery, RouterResponse,
    },
    shutdown::{Restart, Shutdown},
    stats::{HealthRegistry, REPORT_INTERVAL, StreamHealthReport},
    storage::{
        CatalogMediaFragment, DEFAULT_PREVIEW_AFTER_MS, DEFAULT_PREVIEW_BEFORE_MS, EventEmbedding,
        EventImageFilter, EventMetadataQuery, EventSearch, EventSearchField, EventSearchTerm,
        EventSemanticSearchQuery, EventStore, EventTextSearchQuery, RecordingCatalogHandle,
        RecordingDemand, RecordingDemandGuard, RecordingHealthRegistry,
        RecordingStreamHealthSnapshot, StorageConfig,
        metadata::{EventAttachment, EventSource, TimelineEvent},
        safety::filesystem_capacity,
    },
    webrtc::{
        ControlDispatch, ControlHandlerError, ControlRequestHandler, DataChannelTarget,
        MediaSubscriptionPlan, OutboundDataMessage, OutboundEventDelivery, PostSendAction,
        SessionId, StreamQuality, WebRtc,
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use hmac::{Hmac, KeyInit, Mac};
use include_dir::{Dir, File as EmbeddedFile, include_dir};
use prost::Message as _;
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
        Arc, Mutex, RwLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use url::Url;
use uuid::Uuid;

mod camera_access;
mod camera_discovery;
mod camera_permissions;
mod configuration;
mod event_publication;
mod event_search;
mod event_subscription;
mod health_snapshot;
mod logging;
mod mqtt_integration;
mod peek_layouts;
pub(crate) mod recording_coverage;
mod runtime_configuration;
mod stored_media;

pub(crate) fn migrate_peek_layout_configuration(
    config_path: &Path,
    camera_ids: &[String],
) -> anyhow::Result<()> {
    peek_layouts::migrate_configuration(config_path, camera_ids)
}

pub(crate) fn validate_peek_layout_configuration(root: &toml::Table) -> anyhow::Result<()> {
    peek_layouts::validate_configuration(root)
}

pub(crate) fn migrate_template_store(config_path: &Path) -> anyhow::Result<()> {
    configuration::migrate_template_store(config_path)
}

pub(crate) fn validate_template_configuration(root: &toml::Table) -> anyhow::Result<()> {
    configuration::validate_configuration(root)
}

pub(crate) fn validate_backup_layout_document(bytes: &[u8]) -> anyhow::Result<()> {
    peek_layouts::validate_backup_document(bytes)
}

pub(crate) fn backup_layout_camera_ids(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    peek_layouts::backup_camera_ids(bytes)
}

pub(crate) fn validate_backup_template_document(bytes: &[u8]) -> anyhow::Result<()> {
    configuration::validate_backup_template_document(bytes)
}

use health_snapshot::{
    bounded_health_detail, connected_video_stream_ids, expected_video_stream_ids,
    frame_freshness_threshold_ms, keyframe_freshness_threshold_ms, normalized_video_stream_id,
    recording_freshness_threshold_ms, recording_progressing, stream_transport_connected,
};

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
const MAX_DELETE_BODY_BYTES: u64 = 16 * 1_024;
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
const EVENT_PAGE_TOKEN_TTL: Duration = Duration::from_secs(15 * 60);
type EventSearchTaskKey = (SessionId, String);
type EventSearchTasks = Arc<Mutex<HashMap<EventSearchTaskKey, Arc<AtomicBool>>>>;
const PTZ_STOP_SPEED: u32 = 32;
const EXPORT_JOB_EXPIRY: Duration = Duration::from_secs(24 * 60 * 60);
const EXPORT_METADATA_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const EXPORT_NO_PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);
const EXPORT_TOTAL_RUNTIME_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const EXPORT_PROGRESS_PERSIST_INTERVAL: Duration = Duration::from_secs(1);
const MAX_EXPORT_HISTORY_JOBS: usize = 500;
const MAX_EXPORT_HISTORY_BYTES: u64 = 8 * MEBIBYTE_BYTES;
const EXPORT_HISTORY_VERSION: u32 = 1;
const EXPORT_HISTORY_FILE: &str = "history.json";
const MAX_EXPORT_DURATION: Duration = Duration::from_secs(2 * 60);
const MAX_EXPORT_DOWNLOAD_BYTES: u64 = 512 * 1_024 * 1_024;
const CAMERA_CATALOG_WEBSITE: &str = "https://www.cctv-database.com/";
const DEFAULT_CAMERA_CATALOG_SEARCH_LIMIT: usize = 20;
const CAMERA_STREAM_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CAMERA_METADATA_WORKERS: usize = 4;
const CONFIGURATION_CAPABILITY_ID: &str = "keeppeek.configuration.v1";
// Limits user-provided search text before it becomes part of in-memory matching work.
const MAX_CAMERA_CATALOG_QUERY_CHARS: usize = 128;
const MAX_NOTIFICATION_RULE_JSON_BYTES: usize = 64 * 1_024;

static UI_ASSETS: Dir<'static> = include_dir!("$KEEPPEEK_UI_BUILD_DIR");

#[derive(Serialize, Deserialize)]
struct EventPageToken {
    cursor: String,
    expires_at_ms: u64,
}

#[derive(Default, Deserialize)]
struct CameraSettingsUpdate {
    #[serde(default)]
    expected_configuration_revision: String,
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
    expected_configuration_revision: String,
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
    minimum_free_gb: u64,
    maximum_used_percent: Option<u8>,
    warning_free_gb: u64,
    critical_free_gb: u64,
    cleanup_hysteresis_gb: u64,
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

#[derive(Serialize)]
struct CameraSettingsUpdateResponse {
    camera: CameraSettings,
    restart_required: bool,
    configuration_revision: String,
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
    groups: Vec<String>,
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
    details: Vec<prost_types::Any>,
}

impl ControlCommandError {
    fn new(code: proto::ErrorCode, http_status: u16, message: impl Into<String>) -> Self {
        Self {
            code,
            _http_status: http_status,
            message: message.into(),
            details: Vec::new(),
        }
    }

    fn with_detail(mut self, detail: prost_types::Any) -> Self {
        self.details.push(detail);
        self
    }
}

fn required_access_role(command: Option<&control_request::Command>) -> AccessRole {
    match command {
        Some(control_request::Command::CameraControlCommand(command)) => match command.action {
            Some(camera_control_command::Action::Ptz(_))
            | Some(camera_control_command::Action::GetMotionDetection(_)) => AccessRole::User,
            _ => AccessRole::Administrator,
        },
        Some(control_request::Command::EventSearchCommand(command)) => match command.action {
            Some(event_search_command::Action::Query(_))
            | Some(event_search_command::Action::CancelQuery(_))
            | Some(event_search_command::Action::FetchMedia(_))
            | Some(event_search_command::Action::CancelMedia(_)) => AccessRole::User,
            _ => AccessRole::Administrator,
        },
        Some(control_request::Command::NotificationRuleCommand(command)) => match command.action {
            Some(notification_rule_command::Action::GetInbox(_))
            | Some(notification_rule_command::Action::GetHistory(_))
            | Some(notification_rule_command::Action::MarkSeen(_))
            | Some(notification_rule_command::Action::Acknowledge(_))
            | Some(notification_rule_command::Action::Clear(_))
            | Some(notification_rule_command::Action::ClearScope(_)) => AccessRole::User,
            _ => AccessRole::Administrator,
        },
        Some(control_request::Command::ServerCommand(command)) => match command.action {
            Some(server_command::Action::GetAccessSession(_)) => AccessRole::User,
            _ => AccessRole::Administrator,
        },
        Some(control_request::Command::StateStoreCommand(command)) => {
            let namespace = match &command.action {
                Some(proto::state_store_command::Action::Get(request)) => &request.namespace,
                Some(proto::state_store_command::Action::Put(request)) => &request.namespace,
                Some(proto::state_store_command::Action::Delete(request)) => &request.namespace,
                Some(proto::state_store_command::Action::Watch(request)) => &request.namespace,
                Some(proto::state_store_command::Action::Unwatch(_)) | None => "",
            };
            if matches!(
                namespace,
                "keeppeek.integrations.mqtt" | camera_permissions::NAMESPACE
            ) {
                AccessRole::Administrator
            } else {
                AccessRole::User
            }
        }
        Some(
            control_request::Command::SubscribeMedia(_)
            | control_request::Command::SubscribeEvents(_)
            | control_request::Command::SubscribeData(_)
            | control_request::Command::Unsubscribe(_)
            | control_request::Command::StoredMediaCommand(_)
            | control_request::Command::GroupCommand(_)
            | control_request::Command::PublicationCommand(_)
            | control_request::Command::PublicationReport(_),
        ) => AccessRole::User,
        _ => AccessRole::Administrator,
    }
}

const fn access_operation(command: Option<&control_request::Command>) -> &'static str {
    match command {
        Some(control_request::Command::SubscribeMedia(_)) => "subscribe_media",
        Some(control_request::Command::SubscribeEvents(_)) => "subscribe_events",
        Some(control_request::Command::SubscribeData(_)) => "subscribe_data",
        Some(control_request::Command::Unsubscribe(_)) => "unsubscribe",
        Some(control_request::Command::PublishEvent(_)) => "publish_event",
        Some(control_request::Command::PublicationCommand(_)) => "publication",
        Some(control_request::Command::StoredMediaCommand(_)) => "stored_media",
        Some(control_request::Command::EventPublicationCommand(_)) => "event_publication",
        Some(control_request::Command::GroupCommand(_)) => "group",
        Some(control_request::Command::StateStoreCommand(_)) => "state_store",
        Some(control_request::Command::CameraControlCommand(_)) => "camera_control",
        Some(control_request::Command::PublicationReport(_)) => "publication_report",
        Some(control_request::Command::CameraConfigurationCommand(_)) => "camera_configuration",
        Some(control_request::Command::RuntimeConfigurationCommand(_)) => "runtime_configuration",
        Some(control_request::Command::LoggingCommand(_)) => "logging",
        Some(control_request::Command::ServerCommand(_)) => "server",
        Some(control_request::Command::HealthCommand(_)) => "health",
        Some(control_request::Command::ExportCommand(_)) => "export",
        Some(control_request::Command::EventSearchCommand(_)) => "event_search",
        Some(control_request::Command::NotificationRuleCommand(_)) => "notification_rule",
        Some(control_request::Command::ConfigurationCommand(_)) => "configuration",
        None => "missing_command",
    }
}

fn sensitive_administrator_operation(
    command: Option<&control_request::Command>,
) -> Option<&'static str> {
    match command {
        Some(control_request::Command::CameraControlCommand(command)) => match command.action {
            Some(camera_control_command::Action::SetMotionDetection(_)) => {
                Some("camera_motion_update")
            }
            Some(camera_control_command::Action::SetManufacturer(_)) => {
                Some("camera_manufacturer_update")
            }
            _ => None,
        },
        Some(control_request::Command::CameraConfigurationCommand(command)) => {
            match command.action {
                Some(camera_configuration_command::Action::Discover(_)) => {
                    Some("camera_discovery_start")
                }
                Some(camera_configuration_command::Action::CancelDiscovery(_)) => {
                    Some("camera_discovery_cancel")
                }
                Some(camera_configuration_command::Action::ProbeStreams(_)) => {
                    Some("camera_stream_probe")
                }
                Some(camera_configuration_command::Action::Update(_)) => {
                    Some("camera_configuration_update")
                }
                Some(camera_configuration_command::Action::Remove(_)) => {
                    Some("camera_configuration_remove")
                }
                _ => None,
            }
        }
        Some(control_request::Command::RuntimeConfigurationCommand(command)) => {
            match command.action {
                Some(runtime_configuration_command::Action::Update(_)) => {
                    Some("runtime_configuration_update")
                }
                Some(runtime_configuration_command::Action::ProbeStorage(_)) => {
                    Some("storage_write_probe")
                }
                _ => None,
            }
        }
        Some(control_request::Command::StateStoreCommand(command)) => match &command.action {
            Some(proto::state_store_command::Action::Put(request)) => {
                match request.namespace.as_str() {
                    "keeppeek.integrations.mqtt" => Some("mqtt_configuration_update"),
                    camera_permissions::NAMESPACE => Some("camera_access_update"),
                    peek_layouts::NAMESPACE => Some("peek_layout_registry_update"),
                    _ => None,
                }
            }
            _ => None,
        },
        Some(control_request::Command::LoggingCommand(command)) => match command.action {
            Some(logging_command::Action::SetFilter(_)) => Some("logging_filter_update"),
            _ => None,
        },
        Some(control_request::Command::ExportCommand(command)) => match command.action {
            Some(proto::export_command::Action::Create(_)) => Some("export_create"),
            Some(proto::export_command::Action::Cancel(_)) => Some("export_cancel"),
            Some(proto::export_command::Action::Retry(_)) => Some("export_retry"),
            Some(proto::export_command::Action::Download(_)) => Some("export_download"),
            _ => None,
        },
        Some(control_request::Command::EventSearchCommand(command)) => match command.action {
            Some(event_search_command::Action::ReplaceTerms(_)) => Some("event_terms_replace"),
            Some(event_search_command::Action::SetEmbedding(_)) => Some("event_embedding_set"),
            _ => None,
        },
        Some(control_request::Command::NotificationRuleCommand(command)) => match command.action {
            Some(notification_rule_command::Action::SaveDraft(_)) => Some("notification_rule_save"),
            Some(notification_rule_command::Action::Activate(_)) => {
                Some("notification_rule_activate")
            }
            Some(notification_rule_command::Action::Delete(_)) => Some("notification_rule_delete"),
            Some(notification_rule_command::Action::Test(_)) => Some("notification_rule_test"),
            _ => None,
        },
        Some(control_request::Command::ConfigurationCommand(command)) => match command.action {
            Some(proto::configuration_command::Action::SaveTemplate(_)) => {
                Some("configuration_template_save")
            }
            Some(proto::configuration_command::Action::DuplicateTemplate(_)) => {
                Some("configuration_template_duplicate")
            }
            Some(proto::configuration_command::Action::DeleteTemplate(_)) => {
                Some("configuration_template_delete")
            }
            Some(proto::configuration_command::Action::Apply(_)) => Some("configuration_apply"),
            Some(proto::configuration_command::Action::ApplyImport(_)) => {
                Some("configuration_template_import")
            }
            _ => None,
        },
        Some(control_request::Command::PublishEvent(_)) => Some("event_publish"),
        Some(control_request::Command::EventPublicationCommand(command)) => {
            match command.action.as_ref() {
                Some(event_publication_command::Action::Commit(_)) => Some("event_publish"),
                _ => None,
            }
        }
        _ => None,
    }
}

impl ControlRequestHandler for ServerControlHandler {
    fn handle(&self, request: proto::Request) -> ControlDispatch {
        self.handle_for_session(SessionId::from_u64(0), request)
    }

    fn authorize_session_command(
        &self,
        session_id: SessionId,
        request: &proto::Request,
    ) -> Result<(), ControlHandlerError> {
        self.authorize_request(session_id, request)
            .map(|_| ())
            .map_err(|(error, _)| ControlHandlerError::new(error.code, error.message))
    }

    fn handle_data_for_session(
        &self,
        session_id: SessionId,
        channel: proto::DataChannelKind,
        message: proto::Message,
    ) -> Result<(), ControlHandlerError> {
        self.authorize_api_session(
            session_id,
            AccessRole::Administrator,
            "event_attachment_publish",
        )
        .map_err(|(error, _)| ControlHandlerError::new(error.code, error.message))?;
        event_publication::ingest(&self.state, session_id, channel, message)
            .map_err(|error| ControlHandlerError::new(error.code, error.message))
    }

    fn unsubscribe_for_session(&self, session_id: SessionId, subscription_ids: &[String]) {
        self.state
            .event_subscriptions
            .unsubscribe(session_id, subscription_ids);
    }

    fn has_event_subscription(&self, session_id: SessionId, subscription_id: &str) -> bool {
        self.state
            .event_subscriptions
            .contains(session_id, subscription_id)
    }

    fn source_reset(&self, camera_ip: IpAddr) {
        let source_id =
            self.state.camera_entries().into_iter().find_map(|camera| {
                (camera.info.ip.parse() == Ok(camera_ip)).then_some(camera.info.id)
            });
        if let Some(source_id) = source_id {
            self.state.event_publications.invalidate_source(&source_id);
            for session_id in self.state.event_subscriptions.invalidate_source(&source_id) {
                self.state.webrtc.request_api_session_close(session_id);
            }
        }
    }

    fn handle_for_session(
        &self,
        session_id: SessionId,
        request: proto::Request,
    ) -> ControlDispatch {
        let request_id = request.request_id;
        let mut after_send: Option<PostSendAction> = None;
        let mut data_messages = Vec::new();
        let mut notifications = Vec::new();
        let sensitive_operation = sensitive_administrator_operation(request.command.as_ref());
        let result = match self.authorize_request(session_id, &request) {
            Err((error, close_session)) => {
                if close_session {
                    let webrtc = self.state.webrtc.clone();
                    after_send = Some(Box::new(move || {
                        webrtc.request_api_session_close(session_id);
                    }));
                }
                Err(error)
            }
            Ok(principal) => {
                let result = match request.command {
                    Some(control_request::Command::CameraControlCommand(command)) => {
                        self.handle_camera_control(session_id, command).map(Some)
                    }
                    Some(control_request::Command::CameraConfigurationCommand(command)) => self
                        .handle_camera_configuration(session_id, command)
                        .map(Some),
                    Some(control_request::Command::LoggingCommand(command)) => {
                        logging::dispatch(&self.state, command).map(Some)
                    }
                    Some(control_request::Command::ServerCommand(command)) => {
                        match self.handle_server(session_id, &principal, command) {
                            Ok((result, action)) => {
                                after_send = Some(action);
                                Ok(Some(result))
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Some(control_request::Command::RuntimeConfigurationCommand(command)) => {
                        runtime_configuration::dispatch(&self.state, command).map(Some)
                    }
                    Some(control_request::Command::StateStoreCommand(command)) => {
                        if peek_layouts::handles(&command) {
                            peek_layouts::dispatch(&self.state, &principal, command).map(Some)
                        } else if camera_permissions::handles(&command) {
                            camera_permissions::dispatch(self, &principal, command).map(Some)
                        } else {
                            mqtt_integration::dispatch(&self.state, command).map(Some)
                        }
                    }
                    Some(control_request::Command::HealthCommand(command)) => {
                        health_snapshot::dispatch(&self.state, &self.router_tx, command).map(Some)
                    }
                    Some(control_request::Command::ExportCommand(command)) => {
                        match self.handle_export(&principal, command) {
                            Ok((result, messages)) => {
                                data_messages = messages;
                                Ok(Some(result))
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Some(control_request::Command::EventSearchCommand(command)) => {
                        match event_search::dispatch(&self.state, session_id, command) {
                            Ok((result, messages)) => {
                                data_messages = messages;
                                Ok(result)
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Some(control_request::Command::NotificationRuleCommand(command)) => self
                        .handle_notification_rules(session_id, command)
                        .map(Some),
                    Some(control_request::Command::ConfigurationCommand(command)) => {
                        configuration::dispatch(&self.state, command).map(Some)
                    }
                    Some(control_request::Command::StoredMediaCommand(command)) => {
                        match stored_media::dispatch(&self.state, session_id, command) {
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
                    Some(control_request::Command::EventPublicationCommand(command)) => {
                        match event_publication::dispatch(&self.state, session_id, command) {
                            Ok(dispatch) => {
                                if let Some(committed) = dispatch.committed {
                                    self.route_committed_event(
                                        committed.event,
                                        committed.timeline_event,
                                        Some(committed.attachment_bytes),
                                    );
                                }
                                if let Some(event) = dispatch.mqtt_retry
                                    && let Err(error) =
                                        self.forward_published_event(&event, event.start_time_ms)
                                {
                                    tracing::warn!(
                                        message = %error.message,
                                        "committed event MQTT retry failed"
                                    );
                                }
                                Ok(Some(dispatch.result))
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Some(control_request::Command::SubscribeEvents(request)) => self
                        .state
                        .event_subscriptions
                        .subscribe(&self.state, session_id, request)
                        .map(control_ok::Result::SubscriptionResult)
                        .map(Some),
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
                if result.is_ok()
                    && let Some(sensitive_operation) = sensitive_operation
                {
                    record_access_audit(
                        &self.state,
                        i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
                        Some(&principal.id()),
                        Some(principal.role),
                        sensitive_operation,
                        None,
                        "success",
                        self.session_classification(session_id),
                    );
                }
                result
            }
        };
        ControlDispatch {
            response: proto::Response {
                request_id,
                result: Some(match result {
                    Ok(result) => control_response::Result::Ok(proto::Ok { result }),
                    Err(error) => control_response::Result::Error(proto::Error {
                        code: error.code as i32,
                        message: error.message,
                        details: error.details,
                    }),
                }),
            },
            after_send,
            data_messages,
            notifications,
        }
    }

    fn session_closed(&self, session_id: SessionId) {
        close_api_session(&self.state, session_id);
    }

    fn initial_capabilities(&self, session_id: SessionId) -> Option<proto::ServerCapabilities> {
        let self_source_session_id = format!("webrtc-client-{session_id}");
        let camera_entries = camera_access::visible_cameras(self, session_id)?;
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
        let mut capability_ids = vec![
            "keeppeek.media-export.v1".to_owned(),
            "keeppeek.event-search".to_owned(),
            "keeppeek.event-publication.v1".to_owned(),
            "stored-media-keyframe-preview.v1".to_owned(),
        ];
        if self.state.notifications.is_some() {
            capability_ids.push("keeppeek.rules.v1".to_owned());
        }
        if self.state.event_forwarder.is_some() {
            capability_ids.push("keeppeek.mqtt-forwarder.v1".to_owned());
        }
        if self.state.camera_config_path.is_some() {
            capability_ids.push(peek_layouts::CAPABILITY_ID.to_owned());
            capability_ids.push(CONFIGURATION_CAPABILITY_ID.to_owned());
        }
        if self.state.backup_manager.is_some() {
            capability_ids.push("keeppeek.backup.v1".to_owned());
        }
        capability_ids.push("keeppeek.identity.v1".to_owned());
        capability_ids.push(camera_permissions::CAPABILITY_ID.to_owned());
        let access_session = if session_id.as_u64() == 0 {
            Some(proto::AccessSession {
                session_id: "0".to_owned(),
                principal_id: "local-administrator".to_owned(),
                display_name: "Local Administrator".to_owned(),
                role: proto::AccessRole::Administrator as i32,
                local: true,
                client_classification: ClientClassificationReason::DirectLocal.as_str().to_owned(),
                created_at_ms: 0,
                last_activity_at_ms: 0,
                absolute_expires_at_ms: i64::MAX,
                credential_expires_at_ms: None,
            })
        } else {
            self.state
                .api_session_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&session_id)
                .map(|session| proto_access_session(session_id, session))
        };
        Some(proto::ServerCapabilities {
            revision: 2,
            cameras,
            source_sessions,
            stored_media_sources,
            self_source_session_id,
            capability_ids,
            access_session,
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
            .find(|camera| {
                proto_camera_source_session(&camera.info, &self.state.webrtc)
                    .is_some_and(|source| source.source_session_id == request.source_session_id)
            })
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
        if !selected_variant
            .delivery_transports
            .contains(&(delivery_transport as i32))
        {
            return Err(ControlHandlerError::new(
                proto::ErrorCode::Unavailable,
                "selected video delivery transport is unavailable",
            ));
        }
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

fn camera_source_session_id(source_id: &str, generation: u64) -> String {
    format!("camera:{source_id}:{generation}")
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
            let codec = live_source.codec.to_owned();
            let profile_dimensions = profile
                .and_then(|profile| profile.resolution.as_deref())
                .and_then(|resolution| resolution.split_once('x'))
                .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
                .unwrap_or((0, 0));
            let (width, height) = if live_source.width > 0 && live_source.height > 0 {
                (live_source.width, live_source.height)
            } else {
                profile_dimensions
            };
            let mut delivery_transports = vec![proto::DeliveryTransport::Rtp as i32];
            if !live_source.decoder_config.is_empty() {
                delivery_transports.push(proto::DeliveryTransport::ReliableData as i32);
            }
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
                            decoder_config: live_source.decoder_config.clone(),
                        },
                    )),
                }),
                delivery_transports,
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
        source_session_id: camera_source_session_id(
            &camera.id,
            webrtc.camera_generation(camera_ip),
        ),
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
                attachments: vec![published_snapshot_capability()],
            })
            .collect(),
        publication_capabilities: Vec::new(),
    })
}

fn published_snapshot_capability() -> proto::EventAttachmentCapability {
    proto::EventAttachmentCapability {
        attachment_type: "snapshot".to_owned(),
        content_type: "image/jpeg".to_owned(),
        delivery_channels: vec![
            proto::DataChannelKind::ReliableData as i32,
            proto::DataChannelKind::UnreliableData as i32,
        ],
        maximum_count: 1,
        minimum_count: 1,
    }
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

    fn authorize_request(
        &self,
        session_id: SessionId,
        request: &proto::Request,
    ) -> Result<ApiPrincipal, (ControlCommandError, bool)> {
        let operation = access_operation(request.command.as_ref());
        let principal = self.authorize_api_session(
            session_id,
            required_access_role(request.command.as_ref()),
            operation,
        )?;
        camera_access::authorize_command(&self.state, &principal, request).map_err(|error| {
            self.state
                .access_metrics
                .authorization_denials
                .fetch_add(1, Ordering::Relaxed);
            record_access_audit(
                &self.state,
                i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
                Some(&principal.id()),
                Some(principal.role),
                "command_denied",
                Some(operation),
                "camera_access_denied",
                self.session_classification(session_id),
            );
            (error, false)
        })?;
        Ok(principal)
    }

    fn authorize_api_session(
        &self,
        session_id: SessionId,
        required_role: AccessRole,
        operation: &'static str,
    ) -> Result<ApiPrincipal, (ControlCommandError, bool)> {
        if session_id.as_u64() == 0 {
            return Ok(ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        }
        let now_at = Instant::now();
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let mut sessions = self
            .state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(session) = sessions.get_mut(&session_id) else {
            drop(sessions);
            self.state
                .access_metrics
                .authorization_denials
                .fetch_add(1, Ordering::Relaxed);
            record_access_audit(
                &self.state,
                now_ms,
                None,
                None,
                "command_denied",
                Some(&session_id.to_string()),
                "unknown_session",
                ClientClassificationReason::UnknownSession,
            );
            return Err((
                ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    401,
                    "API session is unavailable",
                ),
                false,
            ));
        };
        let credential_active =
            session
                .principal
                .credential_binding()
                .is_none_or(|(id, revision)| {
                    self.state
                        .access_manager
                        .credential_is_active(id, revision, now_ms)
                });
        let expired = now_ms >= session.absolute_expires_at_ms
            || now_at.saturating_duration_since(session.last_activity)
                >= self.state.api_session_policy.idle_timeout
            || !credential_active;
        if expired {
            let session = sessions
                .remove(&session_id)
                .expect("expired session must still be present");
            drop(sessions);
            self.state
                .access_metrics
                .sessions_revoked_or_expired
                .fetch_add(1, Ordering::Relaxed);
            record_access_audit(
                &self.state,
                now_ms,
                Some(&session.principal.id()),
                Some(session.principal.role),
                "command_denied",
                Some(&session_id.to_string()),
                "expired_or_revoked_session",
                session.classification.reason,
            );
            return Err((
                ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    401,
                    "API session expired or was revoked",
                ),
                true,
            ));
        }
        if !session.principal.role.permits(required_role) {
            let principal = session.principal.clone();
            let classification = session.classification;
            drop(sessions);
            self.state
                .access_metrics
                .authorization_denials
                .fetch_add(1, Ordering::Relaxed);
            record_access_audit(
                &self.state,
                now_ms,
                Some(&principal.id()),
                Some(principal.role),
                "command_denied",
                Some(operation),
                "insufficient_role",
                classification.reason,
            );
            return Err((
                ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    403,
                    "Administrator role is required for this operation",
                ),
                false,
            ));
        }
        session.last_activity = now_at;
        session.last_activity_at_ms = now_ms;
        Ok(session.principal.clone())
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
        if event.canonical_attachment_id.is_some() || event.bounding_box_attachment_id.is_some() {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "envelope-only event publication cannot reference attachments",
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
        if proto_camera_source_session(&camera.info, &self.state.webrtc)
            .is_none_or(|source| source.source_session_id != source_session_id)
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
        if !event_publication::text_and_payload_valid(&event) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "published event text or payload is invalid",
            ));
        }
        let payload = event_publication::json_payload(event.payload.as_ref()).map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "published event payload is invalid",
            )
        })?;
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
        let icon =
            crate::storage::metadata::event_icon(event.icon_key.as_deref(), &event.event_type);
        let published_event = event.clone();
        let stored_event = TimelineEvent {
            id: event.event_id,
            revision: event.revision,
            camera_id: event.source_id,
            stream,
            source: EventSource::KeepPeek,
            kind: event.event_type,
            start_time_ms,
            end_time_ms,
            confidence: event.confidence,
            bbox,
            bbox_attachment_id: None,
            zone: event.zone,
            text: event.text,
            payload,
            attachments: Vec::new(),
            canonical_attachment_id: None,
            icon_key: icon.key.to_owned(),
            rejected_icon_key: icon.rejected,
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
                if let Err(error) = self.forward_published_event(&existing, start_time_ms) {
                    tracing::warn!(
                        message = %error.message,
                        "committed event MQTT retry failed"
                    );
                }
                Ok(())
            } else {
                Err(ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    409,
                    "event ID already exists with different content",
                ))
            };
        }
        events.insert(stored_event.clone()).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                format!("unable to store published event: {error}"),
            )
        })?;
        self.route_committed_event(published_event, stored_event, None);
        Ok(())
    }

    fn forward_published_event(
        &self,
        event: &TimelineEvent,
        occurred_at_ms: i64,
    ) -> Result<(), ControlCommandError> {
        let Some(forwarder) = &self.state.event_forwarder else {
            return Ok(());
        };
        forwarder
            .publish_timeline(
                event,
                crate::event_forwarder::model::EventTransition::Created,
                occurred_at_ms,
            )
            .map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::Unavailable,
                    503,
                    format!("event was stored but MQTT forwarding is unavailable: {error}"),
                )
            })
    }

    fn route_committed_event(
        &self,
        mut event: proto::Event,
        timeline_event: TimelineEvent,
        attachment_bytes: Option<Arc<[u8]>>,
    ) {
        event.origin = proto::EventOrigin::Keeppeek as i32;
        event.icon_key = Some(timeline_event.icon_key.clone());
        event.rejected_icon_key = timeline_event.rejected_icon_key.clone();
        event.image_availability = if timeline_event.canonical_attachment_id.is_none() {
            proto::EventImageAvailability::None as i32
        } else if attachment_bytes.is_some() {
            proto::EventImageAvailability::Available as i32
        } else {
            proto::EventImageAvailability::Unavailable as i32
        };
        if let Err(error) =
            self.forward_published_event(&timeline_event, timeline_event.start_time_ms)
        {
            tracing::warn!(message = %error.message, "committed event MQTT fanout failed");
        }
        for delivery in self.state.event_subscriptions.deliveries(&event) {
            let mut delivered_event = event.clone();
            delivered_event.subscription_id = Some(delivery.subscription_id.clone());
            let attachment_bytes = delivery
                .attachment_target
                .and_then(|_| attachment_bytes.as_ref().map(Arc::clone));
            let queued = self.state.webrtc.try_enqueue_api_event(
                delivery.session_id,
                OutboundEventDelivery {
                    event: delivered_event,
                    attachment_target: delivery.attachment_target,
                    attachment_bytes,
                    guard: delivery.guard,
                },
            );
            if !matches!(queued, Ok(true)) {
                self.state
                    .event_subscriptions
                    .shed(delivery.session_id, &delivery.subscription_id);
                tracing::warn!(
                    session_id = %delivery.session_id,
                    subscription_id = %delivery.subscription_id,
                    "shed event subscription after its delivery queue stopped accepting work"
                );
            }
        }
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

    fn handle_notification_rules(
        &self,
        session_id: SessionId,
        command: proto::NotificationRuleCommand,
    ) -> Result<control_ok::Result, ControlCommandError> {
        let principal_id = self.notification_principal(session_id)?;
        let principal_id = principal_id.as_str();
        let notifications = self.state.notifications.as_ref().ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "notification runtime is unavailable",
            )
        })?;
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let result = match command.action {
            Some(notification_rule_command::Action::ListRules(_)) => {
                let rules = notifications
                    .rules(principal_id)
                    .map_err(|error| notification_command_error("list rules", error, false))?
                    .into_iter()
                    .map(proto_notification_rule_record)
                    .collect::<anyhow::Result<Vec<_>>>()
                    .map_err(|error| notification_command_error("encode rules", error, false))?;
                notification_rule_result::Result::Rules(proto::NotificationRuleList { rules })
            }
            Some(notification_rule_command::Action::SaveDraft(request)) => {
                if request.definition_json.len() > MAX_NOTIFICATION_RULE_JSON_BYTES {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        413,
                        "notification rule definition is too large",
                    ));
                }
                let mut definition: serde_json::Value =
                    serde_json::from_str(&request.definition_json).map_err(|_| {
                        ControlCommandError::new(
                            proto::ErrorCode::InvalidRequest,
                            400,
                            "notification rule definition is invalid",
                        )
                    })?;
                let rule_id = definition
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ControlCommandError::new(
                            proto::ErrorCode::InvalidRequest,
                            400,
                            "notification rule definition has no rule ID",
                        )
                    })?;
                let existing = notifications
                    .rules(principal_id)
                    .map_err(|error| notification_command_error("load rule draft", error, false))?
                    .into_iter()
                    .find(|record| record.id == rule_id);
                restore_notification_destinations(&mut definition, existing.as_ref())?;
                let mut rule: NotificationRule =
                    serde_json::from_value(definition).map_err(|_| {
                        ControlCommandError::new(
                            proto::ErrorCode::InvalidRequest,
                            400,
                            "notification rule definition is invalid",
                        )
                    })?;
                rule.owner_id = principal_id.to_owned();
                let record = notifications
                    .save_draft(rule, request.expected_draft_revision, now_ms)
                    .map_err(|error| notification_command_error("save draft", error, true))?;
                notification_rule_result::Result::Rule(
                    proto_notification_rule_record(record).map_err(|error| {
                        notification_command_error("encode saved rule", error, false)
                    })?,
                )
            }
            Some(notification_rule_command::Action::Activate(request)) => {
                validate_client_id(&request.rule_id, "notification rule ID")?;
                let record = notifications
                    .activate(
                        request.rule_id,
                        principal_id,
                        request.expected_active_revision,
                        request.expected_draft_revision,
                        now_ms,
                    )
                    .map_err(|error| notification_command_error("activate rule", error, true))?;
                notification_rule_result::Result::Rule(
                    proto_notification_rule_record(record).map_err(|error| {
                        notification_command_error("encode activated rule", error, false)
                    })?,
                )
            }
            Some(notification_rule_command::Action::Delete(request)) => {
                validate_client_id(&request.rule_id, "notification rule ID")?;
                notifications
                    .delete(
                        request.rule_id.clone(),
                        principal_id,
                        request.expected_active_revision,
                        request.expected_draft_revision,
                        now_ms,
                    )
                    .map_err(|error| notification_command_error("delete rule", error, false))?;
                notification_rule_result::Result::Mutation(proto::NotificationMutationResult {
                    logical_id: request.rule_id,
                })
            }
            Some(notification_rule_command::Action::Test(request)) => {
                validate_client_id(&request.rule_id, "notification rule ID")?;
                let summary = notifications
                    .test_rule(request.rule_id, principal_id, now_ms)
                    .map_err(|error| notification_command_error("test rule", error, true))?;
                notification_rule_result::Result::Test(proto::NotificationTestResult {
                    matched_rules: summary.matched,
                    created_notifications: summary.created,
                    queued_attempts: summary.queued_attempts,
                })
            }
            Some(notification_rule_command::Action::GetInbox(request)) => {
                let inbox = notifications
                    .inbox(
                        principal_id,
                        notification_page_limit(request.limit),
                        camera_access::for_session(&self.state, session_id)?,
                    )
                    .map_err(|error| notification_command_error("load inbox", error, false))?;
                notification_rule_result::Result::Inbox(proto_notification_inbox(inbox))
            }
            Some(notification_rule_command::Action::GetHistory(request)) => {
                let groups = notifications
                    .history(
                        principal_id,
                        notification_page_limit(request.limit),
                        camera_access::for_session(&self.state, session_id)?,
                    )
                    .map_err(|error| notification_command_error("load history", error, false))?;
                notification_rule_result::Result::History(proto::NotificationHistory {
                    groups: groups
                        .into_iter()
                        .map(proto_notification_history_group)
                        .collect(),
                })
            }
            Some(notification_rule_command::Action::MarkSeen(request)) => {
                validate_client_id(&request.logical_id, "logical notification ID")?;
                notifications
                    .mark_seen(request.logical_id.clone(), principal_id, now_ms)
                    .map_err(|error| {
                        notification_command_error("mark notification seen", error, false)
                    })?;
                notification_rule_result::Result::Mutation(proto::NotificationMutationResult {
                    logical_id: request.logical_id,
                })
            }
            Some(notification_rule_command::Action::Acknowledge(request)) => {
                validate_client_id(&request.logical_id, "logical notification ID")?;
                notifications
                    .acknowledge(request.logical_id.clone(), principal_id, now_ms)
                    .map_err(|error| {
                        notification_command_error("acknowledge notification", error, false)
                    })?;
                notification_rule_result::Result::Mutation(proto::NotificationMutationResult {
                    logical_id: request.logical_id,
                })
            }
            Some(notification_rule_command::Action::Clear(request)) => {
                validate_client_id(&request.logical_id, "logical notification ID")?;
                notifications
                    .clear(request.logical_id.clone(), principal_id, now_ms)
                    .map_err(|error| {
                        notification_command_error("clear notification", error, false)
                    })?;
                notification_rule_result::Result::Mutation(proto::NotificationMutationResult {
                    logical_id: request.logical_id,
                })
            }
            Some(notification_rule_command::Action::ClearScope(request)) => {
                let scope = match request.scope {
                    Some(proto::clear_notifications::Scope::All(true)) => ClearScope::All,
                    Some(proto::clear_notifications::Scope::RuleId(rule_id)) => {
                        validate_client_id(&rule_id, "notification rule ID")?;
                        ClearScope::Rule(rule_id)
                    }
                    Some(proto::clear_notifications::Scope::BeforeMs(before_ms)) => {
                        ClearScope::Before(before_ms)
                    }
                    Some(proto::clear_notifications::Scope::All(false)) | None => {
                        return Err(ControlCommandError::new(
                            proto::ErrorCode::InvalidRequest,
                            400,
                            "notification clear scope is required",
                        ));
                    }
                };
                let cleared_count = notifications
                    .clear_scope(principal_id, scope, now_ms)
                    .map_err(|error| {
                        notification_command_error("clear notifications", error, false)
                    })?;
                notification_rule_result::Result::Cleared(proto::NotificationClearResult {
                    cleared_count,
                })
            }
            None => {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "notification rule command has no action",
                ));
            }
        };
        Ok(control_ok::Result::NotificationRuleResult(
            proto::NotificationRuleResult {
                result: Some(result),
            },
        ))
    }

    fn notification_principal(&self, session_id: SessionId) -> Result<String, ControlCommandError> {
        if session_id.as_u64() == 0 {
            return Ok("local-administrator".to_owned());
        }
        let owners = self
            .state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        owners
            .get(&session_id)
            .map(|session| session.principal.id())
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    403,
                    "notification commands require an authenticated API session",
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

    fn handle_camera_configuration(
        &self,
        session_id: SessionId,
        command: proto::CameraConfigurationCommand,
    ) -> Result<control_ok::Result, ControlCommandError> {
        match command.action {
            Some(camera_configuration_command::Action::Get(_)) => {
                let _configuration_update = self
                    .state
                    .config_update
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let configuration_revision = camera_configuration_revision(&self.state)?;
                Ok(control_ok::Result::CameraConfigurationResult(
                    proto::CameraConfigurationResult {
                        camera: None,
                        restart_required: false,
                        removed: false,
                        cameras: camera_settings(&self.router_tx, &self.state)
                            .into_iter()
                            .map(proto_camera_settings)
                            .collect(),
                        configuration_revision,
                    },
                ))
            }
            Some(camera_configuration_command::Action::Discover(request)) => {
                camera_discovery::discover(&self.state, &self.router_tx, session_id, request)
            }
            Some(camera_configuration_command::Action::GetDiscovery(request)) => {
                camera_discovery::get(&self.state, &self.router_tx, session_id, request)
            }
            Some(camera_configuration_command::Action::CancelDiscovery(request)) => {
                camera_discovery::cancel(&self.state, &self.router_tx, session_id, request)
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
                networks = camera_discovery::prefer_configured_networks(
                    networks,
                    self.state
                        .camera_entries()
                        .into_iter()
                        .filter_map(|camera| camera.info.ip.parse::<Ipv4Addr>().ok()),
                );
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
                        configuration_revision: result.configuration_revision,
                    },
                ))
            }
            Some(camera_configuration_command::Action::Remove(request)) => {
                let configuration_revision = delete_camera_settings(
                    &self.state,
                    &request.ip,
                    &request.expected_configuration_revision,
                )?;
                Ok(control_ok::Result::CameraConfigurationResult(
                    proto::CameraConfigurationResult {
                        camera: None,
                        restart_required: false,
                        removed: true,
                        cameras: Vec::new(),
                        configuration_revision,
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
                        health_snapshot::proto_health_profile(ProfileSummary {
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
        principal: &ApiPrincipal,
        command: proto::ServerCommand,
    ) -> Result<(control_ok::Result, PostSendAction), ControlCommandError> {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let classification = self.session_classification(session_id);
        match command.action {
            Some(server_command::Action::Restart(_)) => {
                let Some(control) = self.state.restart_control.clone() else {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::Unavailable,
                        409,
                        "server restart is unavailable",
                    ));
                };
                record_access_audit(
                    &self.state,
                    now_ms,
                    Some(&principal.id()),
                    Some(principal.role),
                    "server_restart",
                    None,
                    "success",
                    classification,
                );
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
                self.require_local_administrator(principal)?;
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
                let issued = self
                    .state
                    .access_manager
                    .claim_initial_access_key(access_key)
                    .map_err(|error| access_command_error("retrieve initial access key", error))?;
                record_access_audit(
                    &self.state,
                    now_ms,
                    Some(&principal.id()),
                    Some(principal.role),
                    "credential_claim",
                    Some(&issued.metadata.id.to_string()),
                    "success",
                    classification,
                );
                Ok((
                    control_ok::Result::AccessKeyResult(proto::AccessKeyResult {
                        access_key: issued.access_key.canonical(),
                        rotated: false,
                    }),
                    Box::new(|| {}),
                ))
            }
            Some(server_command::Action::RotateAccessKey(_)) => {
                self.require_local_administrator(principal)?;
                let credential_id = self
                    .state
                    .access_manager
                    .legacy_credential_id()
                    .ok_or_else(|| {
                        ControlCommandError::new(
                            proto::ErrorCode::Unavailable,
                            409,
                            "initial Administrator credential is unavailable",
                        )
                    })?;
                let issued = self.rotate_credential(credential_id, now_ms)?;
                let revoked_sessions = self.remove_credential_sessions(credential_id);
                tracing::info!(
                    revoked_sessions = revoked_sessions.len(),
                    "initial remote Administrator credential rotated"
                );
                record_access_audit(
                    &self.state,
                    now_ms,
                    Some(&principal.id()),
                    Some(principal.role),
                    "credential_rotate",
                    Some(&credential_id.to_string()),
                    "success",
                    classification,
                );
                Ok((
                    control_ok::Result::AccessKeyResult(proto::AccessKeyResult {
                        access_key: issued.access_key.canonical(),
                        rotated: true,
                    }),
                    close_api_sessions_action(self.state.webrtc.clone(), revoked_sessions),
                ))
            }
            Some(server_command::Action::GetAccessSession(_)) => {
                let current = self.current_access_session(session_id)?;
                Ok((
                    control_ok::Result::AccessSessionResult(proto::AccessSessionResult {
                        current: Some(current),
                        sessions: Vec::new(),
                    }),
                    Box::new(|| {}),
                ))
            }
            Some(server_command::Action::ListAccessCredentials(_)) => Ok((
                control_ok::Result::AccessCredentialResult(proto::AccessCredentialResult {
                    credentials: self
                        .state
                        .access_manager
                        .list_credentials()
                        .into_iter()
                        .map(proto_access_credential)
                        .collect(),
                    access_key: None,
                }),
                Box::new(|| {}),
            )),
            Some(server_command::Action::CreateAccessCredential(request)) => {
                let role = access_role_from_proto(request.role)?;
                let issued = self
                    .state
                    .access_manager
                    .create_credential(
                        &request.name,
                        request.description.as_deref(),
                        role,
                        request.expires_at_ms,
                        now_ms,
                    )
                    .map_err(|error| access_command_error("create credential", error))?;
                let credential_id = issued.metadata.id;
                record_access_audit(
                    &self.state,
                    now_ms,
                    Some(&principal.id()),
                    Some(principal.role),
                    "credential_create",
                    Some(&credential_id.to_string()),
                    "success",
                    classification,
                );
                Ok((
                    control_ok::Result::AccessCredentialResult(proto_issued_credential(issued)),
                    Box::new(|| {}),
                ))
            }
            Some(server_command::Action::RotateAccessCredential(request)) => {
                let credential_id = parse_credential_id(&request.credential_id)?;
                let issued = self.rotate_credential(credential_id, now_ms)?;
                let revoked_sessions = self.remove_credential_sessions(credential_id);
                record_access_audit(
                    &self.state,
                    now_ms,
                    Some(&principal.id()),
                    Some(principal.role),
                    "credential_rotate",
                    Some(&credential_id.to_string()),
                    "success",
                    classification,
                );
                Ok((
                    control_ok::Result::AccessCredentialResult(proto_issued_credential(issued)),
                    close_api_sessions_action(self.state.webrtc.clone(), revoked_sessions),
                ))
            }
            Some(server_command::Action::SetAccessCredentialEnabled(request)) => {
                let credential_id = parse_credential_id(&request.credential_id)?;
                let credential = self
                    .state
                    .access_manager
                    .set_credential_enabled(credential_id, request.enabled)
                    .map_err(|error| access_command_error("change credential", error))?;
                let revoked_sessions = self.remove_credential_sessions(credential_id);
                record_access_audit(
                    &self.state,
                    now_ms,
                    Some(&principal.id()),
                    Some(principal.role),
                    if request.enabled {
                        "credential_enable"
                    } else {
                        "credential_disable"
                    },
                    Some(&credential_id.to_string()),
                    "success",
                    classification,
                );
                Ok((
                    control_ok::Result::AccessCredentialResult(proto::AccessCredentialResult {
                        credentials: vec![proto_access_credential(credential)],
                        access_key: None,
                    }),
                    close_api_sessions_action(self.state.webrtc.clone(), revoked_sessions),
                ))
            }
            Some(server_command::Action::RevokeAccessCredential(request)) => {
                let credential_id = parse_credential_id(&request.credential_id)?;
                let credential = self
                    .state
                    .access_manager
                    .revoke_credential(credential_id, now_ms)
                    .map_err(|error| access_command_error("revoke credential", error))?;
                let revoked_sessions = self.remove_credential_sessions(credential_id);
                record_access_audit(
                    &self.state,
                    now_ms,
                    Some(&principal.id()),
                    Some(principal.role),
                    "credential_revoke",
                    Some(&credential_id.to_string()),
                    "success",
                    classification,
                );
                Ok((
                    control_ok::Result::AccessCredentialResult(proto::AccessCredentialResult {
                        credentials: vec![proto_access_credential(credential)],
                        access_key: None,
                    }),
                    close_api_sessions_action(self.state.webrtc.clone(), revoked_sessions),
                ))
            }
            Some(server_command::Action::ListAccessSessions(_)) => {
                let sessions = self
                    .state
                    .api_session_owners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .map(|(session_id, session)| proto_access_session(*session_id, session))
                    .collect();
                Ok((
                    control_ok::Result::AccessSessionResult(proto::AccessSessionResult {
                        current: Some(self.current_access_session(session_id)?),
                        sessions,
                    }),
                    Box::new(|| {}),
                ))
            }
            Some(server_command::Action::RevokeAccessSession(request)) => {
                let current = self.current_access_session(session_id)?;
                let target_session_id = request
                    .session_id
                    .parse::<u64>()
                    .map(SessionId::from_u64)
                    .map_err(|_| {
                        ControlCommandError::new(
                            proto::ErrorCode::InvalidRequest,
                            400,
                            "session ID is invalid",
                        )
                    })?;
                let removed = self
                    .state
                    .api_session_owners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&target_session_id);
                record_access_audit(
                    &self.state,
                    now_ms,
                    Some(&principal.id()),
                    Some(principal.role),
                    "session_revoke",
                    Some(&target_session_id.to_string()),
                    if removed.is_some() {
                        "success"
                    } else {
                        "not_found"
                    },
                    classification,
                );
                let sessions = removed.map(|_| vec![target_session_id]).unwrap_or_default();
                Ok((
                    control_ok::Result::AccessSessionResult(proto::AccessSessionResult {
                        current: Some(current),
                        sessions: Vec::new(),
                    }),
                    close_api_sessions_action(self.state.webrtc.clone(), sessions),
                ))
            }
            Some(server_command::Action::ListAccessAudit(request)) => Ok((
                control_ok::Result::AccessAuditResult(proto::AccessAuditResult {
                    events: self
                        .state
                        .access_manager
                        .list_audit(request.limit as usize)
                        .into_iter()
                        .map(proto_access_audit_event)
                        .collect(),
                }),
                Box::new(|| {}),
            )),
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "server command has no action",
            )),
        }
    }

    fn require_local_administrator(
        &self,
        principal: &ApiPrincipal,
    ) -> Result<(), ControlCommandError> {
        if principal.is_local() && principal.role == AccessRole::Administrator {
            return Ok(());
        }
        Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "initial access key material is available only to a local Administrator session",
        ))
    }

    fn current_access_session(
        &self,
        session_id: SessionId,
    ) -> Result<proto::AccessSession, ControlCommandError> {
        if session_id.as_u64() == 0 {
            return Ok(proto::AccessSession {
                session_id: "0".to_owned(),
                principal_id: "local-administrator".to_owned(),
                display_name: "Local Administrator".to_owned(),
                role: proto::AccessRole::Administrator as i32,
                local: true,
                client_classification: ClientClassificationReason::DirectLocal.as_str().to_owned(),
                created_at_ms: 0,
                last_activity_at_ms: 0,
                absolute_expires_at_ms: i64::MAX,
                credential_expires_at_ms: None,
            });
        }
        self.state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|session| proto_access_session(session_id, session))
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    401,
                    "API session is unavailable",
                )
            })
    }

    fn session_classification(&self, session_id: SessionId) -> ClientClassificationReason {
        if session_id.as_u64() == 0 {
            return ClientClassificationReason::DirectLocal;
        }
        self.state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map_or(ClientClassificationReason::UnknownSession, |session| {
                session.classification.reason
            })
    }

    fn rotate_credential(
        &self,
        credential_id: Uuid,
        now_ms: i64,
    ) -> Result<IssuedCredential, ControlCommandError> {
        if !self
            .state
            .access_manager
            .is_legacy_credential(credential_id)
        {
            return self
                .state
                .access_manager
                .rotate_credential(credential_id, now_ms)
                .map_err(|error| access_command_error("rotate credential", error));
        }
        let Some(config_path) = &self.state.camera_config_path else {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                409,
                "initial credential rotation requires persisted configuration",
            ));
        };
        let _update = self
            .state
            .config_update
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let access_key = config::rotate_access_key_secret(config_path)
            .map_err(|error| access_command_error("rotate initial credential", error))?;
        let issued = self
            .state
            .access_manager
            .replace_credential_key(credential_id, access_key, now_ms)
            .map_err(|error| access_command_error("rotate initial credential", error))?;
        *self
            .state
            .access_key
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = access_key;
        Ok(issued)
    }

    fn remove_credential_sessions(&self, credential_id: Uuid) -> Vec<SessionId> {
        let mut sessions = self
            .state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let revoked = sessions
            .iter()
            .filter_map(|(session_id, session)| {
                session
                    .principal
                    .credential_binding()
                    .is_some_and(|(id, _)| id == credential_id)
                    .then_some(*session_id)
            })
            .collect::<Vec<_>>();
        let revoked_set = revoked.iter().copied().collect::<HashSet<_>>();
        sessions.retain(|session_id, _| !revoked_set.contains(session_id));
        drop(sessions);
        self.state
            .access_metrics
            .sessions_revoked_or_expired
            .fetch_add(revoked.len() as u64, Ordering::Relaxed);
        cancel_http_streams_for_credential(&self.state, credential_id);
        revoked
    }

    fn handle_export(
        &self,
        principal: &ApiPrincipal,
        command: proto::ExportCommand,
    ) -> Result<(control_ok::Result, Vec<OutboundDataMessage>), ControlCommandError> {
        cleanup_expired_exports(&self.state);
        let requester_id = principal.id();
        match command.action {
            Some(proto::export_command::Action::Create(request)) => {
                let job = create_export_job(&self.state, &requester_id, request)?;
                Ok((control_ok::Result::ExportJob(job), Vec::new()))
            }
            Some(proto::export_command::Action::List(_)) => {
                let jobs = export_jobs(&self.state, &requester_id);
                Ok((
                    control_ok::Result::ExportJobs(proto::ExportJobList { jobs }),
                    Vec::new(),
                ))
            }
            Some(proto::export_command::Action::Get(request)) => {
                let job = export_job(&self.state, &requester_id, &request.job_id)?;
                Ok((control_ok::Result::ExportJob(job), Vec::new()))
            }
            Some(proto::export_command::Action::Cancel(request)) => {
                let job = cancel_export_job(&self.state, &requester_id, &request.job_id)?;
                Ok((control_ok::Result::ExportJob(job), Vec::new()))
            }
            Some(proto::export_command::Action::Retry(request)) => {
                let job = retry_export_job(&self.state, &requester_id, &request.job_id)?;
                Ok((control_ok::Result::ExportJob(job), Vec::new()))
            }
            Some(proto::export_command::Action::Download(request)) => {
                let (result, messages) = download_export(&self.state, &requester_id, request)?;
                Ok((control_ok::Result::ExportDownload(result), messages))
            }
            None => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "export command has no action",
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
    revision: u64,
    descriptor: EventAttachment,
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
    requester_id: &str,
    request: proto::CreateExportJob,
) -> Result<proto::ExportJob, ControlCommandError> {
    validate_export_job_id(&request.job_id)?;
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
    let event_seed = export_event_seed(
        state,
        request.event_seed.as_ref(),
        &request.source_id,
        start_ms,
        end_ms,
    )?;
    {
        let jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = jobs.values().find(|record| {
            record.requester_id == requester_id
                && record.job.status == proto::ExportJobStatus::Running as i32
                && export_requests_match_output(&record.request, &request)
        }) {
            return Ok(existing.job.clone());
        }
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
    let has_missing_ranges = !missing_ranges.is_empty();
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
    let artifact_id = Uuid::new_v4().simple().to_string();
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
        progress_per_mille: if status == proto::ExportJobStatus::Running {
            100
        } else {
            0
        },
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
        event_seed,
    };
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = jobs.values().find(|record| {
        record.requester_id == requester_id
            && record.job.status == proto::ExportJobStatus::Running as i32
            && export_requests_match_normalized(record, &request, has_missing_ranges)
    }) {
        return Ok(existing.job.clone());
    }
    if jobs.contains_key(&request.job_id) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "export job ID already exists",
        ));
    }
    if jobs.len() >= MAX_EXPORT_HISTORY_JOBS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "export history is full; wait for active jobs or retention cleanup",
        ));
    }
    jobs.insert(
        request.job_id.clone(),
        ExportJobRecord {
            requester_id: requester_id.to_owned(),
            artifact_id: artifact_id.clone(),
            request: request.clone(),
            job: job.clone(),
            path: None,
            cancel: cancel.clone(),
            created_at_ms: now_ms,
            started_at_ms: (status == proto::ExportJobStatus::Running).then_some(now_ms),
            updated_at_ms: now_ms,
            completed_at_ms: (status != proto::ExportJobStatus::Running).then_some(now_ms),
            downloaded_at_ms: None,
        },
    );
    if let Some(history_path) = &state.export_history_path
        && let Err(error) = persist_export_jobs(history_path, &jobs)
    {
        jobs.remove(&request.job_id);
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to persist export job: {error}"),
        ));
    }
    drop(jobs);
    if status == proto::ExportJobStatus::Running {
        spawn_export_worker(
            state.clone(),
            request,
            fragments,
            cancel,
            artifact_id,
            end_ms,
            file_name,
        );
    }
    Ok(job)
}

fn export_event_seed(
    state: &ServerState,
    requested: Option<&proto::EventExportSeed>,
    source_id: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Option<proto::EventExportSeed>, ControlCommandError> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    validate_client_id(&requested.event_id, "export event ID")?;
    if requested.revision == 0 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export event revision must be positive",
        ));
    }
    let store = state.events.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "event storage is unavailable",
        )
    })?;
    let event = store
        .event_by_id(&requested.event_id)
        .map_err(|error| stored_catalog_error("load export event seed", error))?
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "export event was not found",
            )
        })?;
    if event.camera_id != source_id
        || event.start_time_ms >= end_ms
        || event
            .end_time_ms
            .unwrap_or_else(|| event.start_time_ms.saturating_add(1))
            <= start_ms
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export event is outside the requested source or range",
        ));
    }
    if event.revision != requested.revision {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "export event revision is stale",
        ));
    }
    let canonical_attachment = event
        .canonical_attachment()
        .cloned()
        .map(proto_event_attachment_descriptor);
    let requested_attachment_id = requested
        .canonical_attachment
        .as_ref()
        .map(|attachment| attachment.attachment_id.as_str());
    let actual_attachment_id = canonical_attachment
        .as_ref()
        .map(|attachment| attachment.attachment_id.as_str());
    if requested_attachment_id != actual_attachment_id {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "export canonical attachment identity is stale",
        ));
    }
    let image_available = event
        .canonical_attachment()
        .is_some_and(crate::storage::metadata::is_supported_event_image)
        && store
            .thumbnail_path(&event.camera_id, &event.id)
            .ok()
            .flatten()
            .is_some();
    let image_availability =
        proto_event_image_availability(event.canonical_attachment_id.is_some(), image_available);
    Ok(Some(proto::EventExportSeed {
        event_id: event.id,
        revision: event.revision,
        canonical_attachment,
        icon_key: Some(event.icon_key),
        image_availability,
    }))
}

fn export_requests_match_output(
    left: &proto::CreateExportJob,
    right: &proto::CreateExportJob,
) -> bool {
    left.source_id == right.source_id
        && left.stream_id == right.stream_id
        && left.start_time == right.start_time
        && left.end_time == right.end_time
        && left.allow_partial == right.allow_partial
        && left.burn_in_timestamp == right.burn_in_timestamp
}

fn export_requests_match_normalized(
    existing: &ExportJobRecord,
    requested: &proto::CreateExportJob,
    requested_has_gaps: bool,
) -> bool {
    let existing_has_gaps = !existing.job.missing_ranges.is_empty();
    existing.request.source_id == requested.source_id
        && existing.request.stream_id == requested.stream_id
        && existing.request.start_time == requested.start_time
        && existing.request.end_time == requested.end_time
        && (existing.request.allow_partial && existing_has_gaps)
            == (requested.allow_partial && requested_has_gaps)
        && existing.request.burn_in_timestamp == requested.burn_in_timestamp
}

fn spawn_export_worker(
    state: ServerState,
    request: proto::CreateExportJob,
    fragments: Vec<CatalogMediaFragment>,
    cancel: Arc<AtomicBool>,
    artifact_id: String,
    end_ms: i64,
    file_name: String,
) {
    let job_id = request.job_id;
    let _ = cleanup_export_attempt_artifacts(&state, &job_id, &artifact_id);
    let path = export_attempt_directory(&state, &job_id, &artifact_id).join(&file_name);
    let monitor_state = state.clone();
    let monitor_job_id = job_id.clone();
    let monitor_cancel = cancel.clone();
    let monitor_path = path;
    let monitor_file_name = file_name;
    let monitor_artifact_id = artifact_id.clone();
    let spawn = std::thread::Builder::new()
        .name(format!("export-monitor-{job_id}"))
        .spawn(move || {
            let (events, receiver) = mpsc::sync_channel(64);
            let worker_cancel = monitor_cancel.clone();
            let worker_path = monitor_path.clone();
            let worker_attempt_directory = worker_path.parent().map(Path::to_path_buf);
            let worker_job_id = monitor_job_id.clone();
            let estimated_bytes = export_estimated_bytes(&fragments).max(1);
            let worker = std::thread::Builder::new()
                .name(format!("export-worker-{worker_job_id}"))
                .spawn(move || {
                    let result = crate::storage::playback::export_fragment_ranges_with_progress(
                        &fragments,
                        end_ms,
                        &worker_path,
                        || {
                            let _ = events.try_send(ExportWorkerEvent::Heartbeat);
                            worker_cancel.load(Ordering::Acquire)
                        },
                        |bytes| {
                            let _ = events.try_send(ExportWorkerEvent::Progress {
                                per_mille: 200u32.saturating_add(
                                    u32::try_from(bytes.saturating_mul(650) / estimated_bytes)
                                        .unwrap_or(650)
                                        .min(650),
                                ),
                                bytes,
                            });
                        },
                    )
                    .and_then(|artifact| {
                        let _ = events.send(ExportWorkerEvent::Progress {
                            per_mille: 900,
                            bytes: artifact.bytes,
                        });
                        let checksum = sha256_file_with_progress(
                            &worker_path,
                            &worker_cancel,
                            artifact.bytes,
                            |bytes| {
                                let _ = events.try_send(ExportWorkerEvent::Progress {
                                    per_mille: 900u32.saturating_add(
                                        u32::try_from(
                                            bytes.saturating_mul(90) / artifact.bytes.max(1),
                                        )
                                        .unwrap_or(90)
                                        .min(90),
                                    ),
                                    bytes: artifact.bytes,
                                });
                            },
                        )?;
                        Ok((artifact, checksum))
                    });
                    let cleanup = result.is_err() || worker_cancel.load(Ordering::Acquire);
                    let delivered = events.send(ExportWorkerEvent::Finished(result)).is_ok();
                    if (cleanup || !delivered)
                        && let Some(directory) = worker_attempt_directory
                    {
                        let _ = std::fs::remove_dir_all(directory);
                    }
                });
            if let Err(error) = worker {
                finish_export_worker(
                    &monitor_state,
                    &monitor_job_id,
                    &monitor_cancel,
                    ExportArtifactTarget {
                        path: &monitor_path,
                        file_name: &monitor_file_name,
                        artifact_id: &monitor_artifact_id,
                    },
                    Err(anyhow::anyhow!("unable to start export worker: {error}")),
                );
                return;
            }
            monitor_export_worker(
                &monitor_state,
                &monitor_job_id,
                &monitor_cancel,
                ExportArtifactTarget {
                    path: &monitor_path,
                    file_name: &monitor_file_name,
                    artifact_id: &monitor_artifact_id,
                },
                receiver,
                ExportDeadlines {
                    no_progress: EXPORT_NO_PROGRESS_TIMEOUT,
                    total_runtime: EXPORT_TOTAL_RUNTIME_TIMEOUT,
                },
            );
        });
    if let Err(error) = spawn
        && fail_export_job(
            &state,
            &job_id,
            &cancel,
            format!("unable to start export monitor: {error}"),
        )
    {
        let _ = cleanup_export_attempt_artifacts(&state, &job_id, &artifact_id);
    }
}

enum ExportWorkerEvent {
    Heartbeat,
    Progress { per_mille: u32, bytes: u64 },
    Finished(anyhow::Result<(crate::storage::playback::ExportArtifact, String)>),
}

#[derive(Clone, Copy)]
struct ExportDeadlines {
    no_progress: Duration,
    total_runtime: Duration,
}

#[derive(Clone, Copy)]
struct ExportArtifactTarget<'a> {
    path: &'a Path,
    file_name: &'a str,
    artifact_id: &'a str,
}

fn monitor_export_worker(
    state: &ServerState,
    job_id: &str,
    cancel: &Arc<AtomicBool>,
    target: ExportArtifactTarget<'_>,
    receiver: mpsc::Receiver<ExportWorkerEvent>,
    deadlines: ExportDeadlines,
) {
    let started_at = Instant::now();
    let mut last_progress = started_at;
    let mut last_persisted = started_at;
    loop {
        let now = Instant::now();
        let total_remaining = deadlines
            .total_runtime
            .saturating_sub(now.duration_since(started_at));
        let progress_remaining = deadlines
            .no_progress
            .saturating_sub(now.duration_since(last_progress));
        let wait = total_remaining.min(progress_remaining);
        if wait.is_zero() {
            cancel.store(true, Ordering::Release);
            let message = if total_remaining.is_zero() {
                "Export exceeded the 5 minute runtime deadline"
            } else {
                "Export made no progress for 30 seconds"
            };
            fail_export_job(state, job_id, cancel, message.to_owned());
            let _ = cleanup_export_attempt_artifacts(state, job_id, target.artifact_id);
            return;
        }
        match receiver.recv_timeout(wait) {
            Ok(ExportWorkerEvent::Heartbeat) => {
                if cancel.load(Ordering::Acquire) {
                    return;
                }
                last_progress = Instant::now();
            }
            Ok(ExportWorkerEvent::Progress { per_mille, bytes }) => {
                last_progress = Instant::now();
                let mut jobs = state
                    .export_jobs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(record) = jobs.get_mut(job_id) else {
                    cancel.store(true, Ordering::Release);
                    return;
                };
                if !Arc::ptr_eq(&record.cancel, cancel)
                    || record.job.status != proto::ExportJobStatus::Running as i32
                {
                    cancel.store(true, Ordering::Release);
                    return;
                }
                record.job.progress_per_mille =
                    record.job.progress_per_mille.max(per_mille.min(990));
                record.job.bytes_written = record.job.bytes_written.max(bytes);
                record.updated_at_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
                if last_persisted.elapsed() >= EXPORT_PROGRESS_PERSIST_INTERVAL {
                    persist_export_jobs_logged(state, &jobs, "progress");
                    last_persisted = Instant::now();
                }
            }
            Ok(ExportWorkerEvent::Finished(result)) => {
                finish_export_worker(state, job_id, cancel, target, result);
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                fail_export_job(
                    state,
                    job_id,
                    cancel,
                    "Export worker stopped unexpectedly".to_owned(),
                );
                let _ = cleanup_export_attempt_artifacts(state, job_id, target.artifact_id);
                return;
            }
        }
    }
}

fn finish_export_worker(
    state: &ServerState,
    job_id: &str,
    cancel: &Arc<AtomicBool>,
    target: ExportArtifactTarget<'_>,
    result: anyhow::Result<(crate::storage::playback::ExportArtifact, String)>,
) {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(record) = jobs.get_mut(job_id) else {
        drop(jobs);
        let _ = cleanup_export_attempt_artifacts(state, job_id, target.artifact_id);
        return;
    };
    if !Arc::ptr_eq(&record.cancel, cancel)
        || record.artifact_id != target.artifact_id
        || record.job.status != proto::ExportJobStatus::Running as i32
    {
        drop(jobs);
        let _ = cleanup_export_attempt_artifacts(state, job_id, target.artifact_id);
        return;
    }
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    match result {
        Ok((artifact, checksum)) if !cancel.load(Ordering::Acquire) => {
            let expires_ms = unix_time_ms()
                .saturating_add(u64::try_from(EXPORT_JOB_EXPIRY.as_millis()).unwrap_or(u64::MAX));
            record.job.status = proto::ExportJobStatus::Ready as i32;
            record.job.progress_per_mille = 1_000;
            record.job.bytes_written = artifact.bytes;
            record.job.aligned_start_time = Some(millis_timestamp(artifact.aligned_start_ms));
            record.job.file_name = Some(target.file_name.to_owned());
            record.job.sha256 = Some(checksum);
            record.job.expires_at = Some(millis_timestamp(
                i64::try_from(expires_ms).unwrap_or(i64::MAX),
            ));
            record.job.error = None;
            record.job.retryable = false;
            record.path = Some(target.path.to_path_buf());
            record.updated_at_ms = now_ms;
            record.completed_at_ms = Some(now_ms);
        }
        Ok(_) => {
            record.job.status = proto::ExportJobStatus::Cancelled as i32;
            record.job.error = Some("Export was cancelled".to_owned());
            record.job.retryable = true;
            record.updated_at_ms = now_ms;
            record.completed_at_ms = Some(now_ms);
        }
        Err(error) => {
            let cancelled = cancel.load(Ordering::Acquire);
            record.job.status = if cancelled {
                proto::ExportJobStatus::Cancelled as i32
            } else {
                proto::ExportJobStatus::Failed as i32
            };
            record.job.error = Some(if cancelled {
                "Export was cancelled".to_owned()
            } else {
                error.to_string()
            });
            record.job.retryable = true;
            record.updated_at_ms = now_ms;
            record.completed_at_ms = Some(now_ms);
        }
    }
    let keep_artifact = record.job.status == proto::ExportJobStatus::Ready as i32;
    persist_export_jobs_logged(state, &jobs, "completion");
    drop(jobs);
    if !keep_artifact {
        let _ = cleanup_export_attempt_artifacts(state, job_id, target.artifact_id);
    }
}

fn fail_export_job(
    state: &ServerState,
    job_id: &str,
    cancel: &Arc<AtomicBool>,
    message: String,
) -> bool {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(record) = jobs.get_mut(job_id) else {
        return false;
    };
    if !Arc::ptr_eq(&record.cancel, cancel)
        || record.job.status != proto::ExportJobStatus::Running as i32
    {
        return false;
    }
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    record.job.status = proto::ExportJobStatus::Failed as i32;
    record.job.error = Some(message);
    record.job.retryable = true;
    record.updated_at_ms = now_ms;
    record.completed_at_ms = Some(now_ms);
    record.path = None;
    persist_export_jobs_logged(state, &jobs, "failure");
    true
}

fn persist_export_jobs_logged(
    state: &ServerState,
    jobs: &HashMap<String, ExportJobRecord>,
    transition: &str,
) {
    if let Some(history_path) = &state.export_history_path
        && let Err(error) = persist_export_jobs(history_path, jobs)
    {
        tracing::warn!(%error, transition, "unable to persist export history");
    }
}

fn export_job(
    state: &ServerState,
    requester_id: &str,
    job_id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(job_id)
        .filter(|record| record.requester_id == requester_id)
        .map(|record| record.job.clone())
        .ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::NotFound, 404, "export job was not found")
        })
}

fn export_jobs(state: &ServerState, requester_id: &str) -> Vec<proto::ExportJob> {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .filter(|record| record.requester_id == requester_id)
        .map(|record| record.job.clone())
        .collect::<Vec<_>>();
    jobs.sort_unstable_by(|left, right| {
        export_requested_start(right).cmp(&export_requested_start(left))
    });
    jobs
}

fn cancel_export_job(
    state: &ServerState,
    requester_id: &str,
    job_id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let record = jobs
        .get_mut(job_id)
        .filter(|record| record.requester_id == requester_id)
        .ok_or_else(|| {
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
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    record.updated_at_ms = now_ms;
    record.completed_at_ms = Some(now_ms);
    let job = record.job.clone();
    let artifact_id = record.artifact_id.clone();
    if let Some(history_path) = &state.export_history_path {
        persist_export_jobs(history_path, &jobs).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                format!("unable to persist export cancellation: {error}"),
            )
        })?;
    }
    drop(jobs);
    let _ = cleanup_export_attempt_artifacts(state, job_id, &artifact_id);
    Ok(job)
}

fn retry_export_job(
    state: &ServerState,
    requester_id: &str,
    job_id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    let (request, artifact_id) = {
        let mut jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = jobs
            .get(job_id)
            .filter(|record| record.requester_id == requester_id)
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::NotFound,
                    404,
                    "export job was not found",
                )
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
        let artifact_id = record.artifact_id.clone();
        let previous = record.clone();
        jobs.remove(job_id);
        if let Some(history_path) = &state.export_history_path
            && let Err(error) = persist_export_jobs(history_path, &jobs)
        {
            jobs.insert(job_id.to_owned(), previous);
            return Err(ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                format!("unable to persist export retry: {error}"),
            ));
        }
        (request, artifact_id)
    };
    let _ = cleanup_export_attempt_artifacts(state, job_id, &artifact_id);
    create_export_job(state, requester_id, request)
}

fn download_export(
    state: &ServerState,
    requester_id: &str,
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
    let (job, path, expected_checksum) = {
        let jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = jobs
            .get(&request.job_id)
            .filter(|record| record.requester_id == requester_id)
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::NotFound,
                    404,
                    "export job was not found",
                )
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
        let expected_checksum = record.job.sha256.clone().ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                "ready export has no checksum",
            )
        })?;
        (record.job.clone(), path, expected_checksum)
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
    let actual_checksum = encode_lower_hex(Sha256::digest(&payload));
    if actual_checksum != expected_checksum {
        let mut jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let artifact_id = if let Some(record) = jobs.get_mut(&request.job_id)
            && record.requester_id == requester_id
        {
            let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
            record.job.status = proto::ExportJobStatus::Failed as i32;
            record.job.error =
                Some("Export checksum verification failed; retry the export".to_owned());
            record.job.retryable = true;
            record.updated_at_ms = now_ms;
            record.completed_at_ms = Some(now_ms);
            record.path = None;
            Some(record.artifact_id.clone())
        } else {
            None
        };
        if let Some(history_path) = &state.export_history_path
            && let Err(error) = persist_export_jobs(history_path, &jobs)
        {
            tracing::warn!(%error, "unable to persist export checksum failure");
        }
        drop(jobs);
        if let Some(artifact_id) = artifact_id {
            let _ = cleanup_export_attempt_artifacts(state, &request.job_id, &artifact_id);
        }
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "export checksum verification failed; retry the export",
        ));
    }
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
    {
        let mut jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(record) = jobs.get_mut(&request.job_id)
            && record.requester_id == requester_id
        {
            let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
            record.downloaded_at_ms = Some(now_ms);
            record.updated_at_ms = now_ms;
        }
        if let Some(history_path) = &state.export_history_path {
            persist_export_jobs(history_path, &jobs).map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::Unavailable,
                    503,
                    format!("unable to persist export download: {error}"),
                )
            })?;
        }
    }
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
    let mut attempts = Vec::new();
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut changed = false;
    for record in jobs.values_mut() {
        if record.job.status == proto::ExportJobStatus::Ready as i32 {
            let missing = record.path.as_ref().is_none_or(|path| !path.is_file());
            let expired = record
                .job
                .expires_at
                .as_ref()
                .and_then(timestamp_ms)
                .is_some_and(|expires| expires <= now_ms);
            if missing || expired {
                record.job.status = if missing {
                    proto::ExportJobStatus::Failed as i32
                } else {
                    proto::ExportJobStatus::Expired as i32
                };
                record.job.error =
                    missing.then(|| "Export artifact is missing; retry the export".to_owned());
                record.job.retryable = true;
                record.updated_at_ms = now_ms;
                record.completed_at_ms = Some(now_ms);
                record.path = None;
                changed = true;
                attempts.push((record.job.job_id.clone(), record.artifact_id.clone()));
            }
        }
    }
    let retention_ms = i64::try_from(EXPORT_METADATA_RETENTION.as_millis()).unwrap_or(i64::MAX);
    let retained_after_ms = now_ms.saturating_sub(retention_ms);
    jobs.retain(|job_id, record| {
        let retain = record.job.status == proto::ExportJobStatus::Running as i32
            || record.updated_at_ms >= retained_after_ms;
        if !retain {
            attempts.push((job_id.clone(), record.artifact_id.clone()));
            changed = true;
        }
        retain
    });
    if jobs.len() > MAX_EXPORT_HISTORY_JOBS {
        let mut terminal = jobs
            .iter()
            .filter(|(_, record)| record.job.status != proto::ExportJobStatus::Running as i32)
            .map(|(job_id, record)| (job_id.clone(), record.updated_at_ms))
            .collect::<Vec<_>>();
        terminal.sort_unstable_by_key(|(_, updated_at_ms)| *updated_at_ms);
        for (job_id, _) in terminal
            .into_iter()
            .take(jobs.len().saturating_sub(MAX_EXPORT_HISTORY_JOBS))
        {
            if let Some(record) = jobs.remove(&job_id) {
                attempts.push((job_id, record.artifact_id));
            }
            changed = true;
        }
    }
    if changed
        && let Some(history_path) = &state.export_history_path
        && let Err(error) = persist_export_jobs(history_path, &jobs)
    {
        tracing::warn!(%error, "unable to persist expired export jobs");
    }
    drop(jobs);
    for (job_id, artifact_id) in attempts {
        let _ = cleanup_export_attempt_artifacts(state, &job_id, &artifact_id);
    }
}

fn export_job_directory(state: &ServerState, job_id: &str) -> PathBuf {
    state
        .storage_config
        .long_term_path
        .join(".exports")
        .join(job_id)
}

fn export_attempt_directory(state: &ServerState, job_id: &str, artifact_id: &str) -> PathBuf {
    export_job_directory(state, job_id).join(artifact_id)
}

fn cleanup_export_attempt_artifacts(
    state: &ServerState,
    job_id: &str,
    artifact_id: &str,
) -> std::io::Result<()> {
    cleanup_export_attempt_directory(
        &state.storage_config.long_term_path.join(".exports"),
        job_id,
        artifact_id,
    )
}

fn cleanup_export_attempt_directory(
    export_root: &Path,
    job_id: &str,
    artifact_id: &str,
) -> std::io::Result<()> {
    if !safe_export_job_id(job_id) || !safe_export_job_id(artifact_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid export artifact identity",
        ));
    }
    let job_directory = export_root.join(job_id);
    match std::fs::remove_dir_all(job_directory.join(artifact_id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }?;
    match std::fs::remove_dir(job_directory) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
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

fn sha256_file_with_progress(
    path: &Path,
    cancelled: &AtomicBool,
    total_bytes: u64,
    progress: impl Fn(u64),
) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1_024];
    let mut processed = 0u64;
    loop {
        if cancelled.load(Ordering::Acquire) {
            anyhow::bail!("export was cancelled");
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        processed = processed.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        progress(processed.min(total_bytes));
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
        return Ok((
            stored_media_cursor_state(state, session_id, &refill.stored_media_id)?,
            Vec::new(),
        ));
    }
    let target_buffer_ms = max_buffer_ms.min(STORED_MEDIA_TARGET_BUFFER_MS);
    let buffer_end =
        playback_time_ms.saturating_add(i64::try_from(target_buffer_ms).unwrap_or(i64::MAX));
    let delivery_end = end_time_ms.map_or(buffer_end, |end_time| end_time.min(buffer_end));
    if delivery_end <= delivered_through_ms {
        return Ok((
            stored_media_cursor_state(state, session_id, &refill.stored_media_id)?,
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
        return Ok((
            stored_media_cursor_state(state, session_id, &refill.stored_media_id)?,
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

fn stored_media_cursor_state(
    state: &ServerState,
    session_id: SessionId,
    stored_media_id: &str,
) -> Result<proto::StoredMediaState, ControlCommandError> {
    let cursors = state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cursor = cursors
        .get(&(session_id, stored_media_id.to_owned()))
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "stored media cursor was not found",
            )
        })?;
    Ok(proto_stored_media_state(stored_media_id, cursor))
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
    access: crate::access::CameraAccess,
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
            let result =
                query_events(&worker_state, request, &access).map(|(_, messages)| messages);
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
    access: &crate::access::CameraAccess,
) -> Result<(proto::EventSearchDelivery, Vec<OutboundDataMessage>), ControlCommandError> {
    camera_access::authorize_event_query(access, &request)?;
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
        validate_event_search_source(state, source_id, &request.stream_id)?;
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
    let page_token = (!request.page_token.is_empty())
        .then(|| open_event_page_token(state, &request.page_token))
        .transpose()?;

    let catalog = event_search_catalog(state)?;
    let search = EventSearch::new(catalog.clone());
    let mut include_preview_keyframes = true;
    let mut page = match request.search {
        Some(proto::query_events::Search::Metadata(metadata)) => {
            validate_event_metadata_search(&metadata)?;
            include_preview_keyframes = metadata.include_preview_keyframes;
            if request.source_id.is_some() {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "metadata search sources must use the metadata filter",
                ));
            }
            let source_ids = if metadata.source_ids.is_empty() {
                camera_access::query_cameras(state, access, &[])?
                    .into_iter()
                    .filter(|camera| {
                        camera
                            .info
                            .profiles
                            .iter()
                            .any(|profile| profile.stream == request.stream_id)
                    })
                    .map(|camera| camera.info.id)
                    .collect()
            } else {
                for source_id in &metadata.source_ids {
                    validate_event_search_source(state, source_id, &request.stream_id)?;
                }
                metadata.source_ids
            };
            let origins = metadata
                .origins
                .into_iter()
                .map(storage_event_source)
                .collect::<Result<Vec<_>, _>>()?;
            let image = match proto::EventImageFilter::try_from(metadata.image) {
                Ok(proto::EventImageFilter::Any) => EventImageFilter::Any,
                Ok(proto::EventImageFilter::WithImage) => EventImageFilter::WithImage,
                Ok(proto::EventImageFilter::WithoutImage) => EventImageFilter::WithoutImage,
                Err(_) => {
                    return Err(ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "event image filter is invalid",
                    ));
                }
            };
            if source_ids.is_empty() {
                crate::storage::EventSearchPage {
                    hits: Vec::new(),
                    next_page_token: None,
                    candidates_truncated: false,
                }
            } else {
                search
                    .search_metadata(EventMetadataQuery {
                        event_ids: metadata.event_ids,
                        source_ids,
                        event_types: metadata.event_types,
                        origins,
                        zones: metadata.zones,
                        minimum_confidence: metadata.minimum_confidence,
                        image,
                        text: metadata.text,
                        stream_id: request.stream_id.clone(),
                        start_time_ms: start_ms,
                        end_time_ms: end_ms,
                        preview_before_ms,
                        preview_after_ms,
                        page_size,
                        page_token,
                        include_preview_keyframes: false,
                    })
                    .map_err(|error| event_search_error("search event metadata", error))?
            }
        }
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
                    page_token,
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
                page_token,
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
    if include_preview_keyframes {
        remap_event_search_keyframes(state, catalog, &request.stream_id, &mut page.hits)?;
    }
    refresh_event_search_image_availability(state, &mut page.hits);
    let next_page_token = page
        .next_page_token
        .take()
        .map(|cursor| seal_event_page_token(state, cursor))
        .transpose()?;

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
            next_page_token: next_page_token.unwrap_or_default(),
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
    event_id: String,
    event_revision: u64,
    attachment_id: String,
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
        if !object_ids.insert(object.object_id.clone()) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event search object IDs must be unique within a transfer",
            ));
        }
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
        if representation == proto::StoredMediaObjectRepresentation::EventAttachment {
            let resolved = resolve_event_search_attachment(state, object)?;
            total_bytes = total_bytes.checked_add(resolved.length).ok_or_else(|| {
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
            objects.push(resolved);
            continue;
        }
        validate_client_id(&object.source_id, "event search media source ID")?;
        validate_client_id(&object.recording_id, "event search recording ID")?;
        if object.fragment_sequence == 0 {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event search fragment sequence must be positive",
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
            proto::StoredMediaObjectRepresentation::Unspecified
            | proto::StoredMediaObjectRepresentation::EventAttachment => {
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
            event_id: String::new(),
            event_revision: 0,
            attachment_id: String::new(),
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
        proto::StoredMediaObjectRepresentation::EventAttachment => 1,
        proto::StoredMediaObjectRepresentation::EncodedKeyframe => 2,
        proto::StoredMediaObjectRepresentation::Fmp4Gop => 3,
        proto::StoredMediaObjectRepresentation::Unspecified => 4,
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

fn resolve_event_search_attachment(
    state: &ServerState,
    object: &proto::EventSearchMediaObject,
) -> Result<ResolvedEventSearchMediaObject, ControlCommandError> {
    validate_client_id(&object.source_id, "event attachment source ID")?;
    validate_client_id(&object.event_id, "event attachment event ID")?;
    validate_client_id(&object.attachment_id, "event attachment ID")?;
    if object.event_revision == 0 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event attachment revision must be positive",
        ));
    }
    let store = state.events.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "event attachment storage is unavailable",
        )
    })?;
    let event = store
        .event_by_id(&object.event_id)
        .map_err(|error| stored_catalog_error("load event attachment metadata", error))?
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                "event attachment was not found",
            )
        })?;
    if event.camera_id != object.source_id {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "event attachment was not found",
        ));
    }
    if event.revision != object.event_revision {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "event attachment revision is stale",
        ));
    }
    if event.canonical_attachment_id.as_deref() != Some(object.attachment_id.as_str()) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "requested attachment is not canonical for this event revision",
        ));
    }
    let descriptor = event.canonical_attachment().cloned().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "stored event has no canonical attachment descriptor",
        )
    })?;
    if !crate::storage::metadata::is_supported_event_image(&descriptor) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "canonical event attachment is unavailable",
        ));
    }
    let path = store
        .thumbnail_path(&event.camera_id, &event.id)
        .map_err(|error| stored_catalog_error("resolve canonical event attachment", error))?
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "canonical event attachment is unavailable",
            )
        })?;
    let length = path
        .metadata()
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "canonical event attachment is unavailable",
            )
        })?
        .len();
    if descriptor
        .byte_len
        .is_some_and(|expected| expected != length)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "canonical event attachment length changed",
        ));
    }
    Ok(ResolvedEventSearchMediaObject {
        object_id: object.object_id.clone(),
        event_id: event.id,
        event_revision: event.revision,
        attachment_id: descriptor.id,
        recording_id: String::new(),
        fragment_sequence: 0,
        representation: proto::StoredMediaObjectRepresentation::EventAttachment,
        content_type: descriptor.content_type,
        path,
        offset: 0,
        length,
        codec: String::new(),
        width: 0,
        height: 0,
        decoder_config: Vec::new(),
        nal_length_size: 0,
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
                event_id: object.event_id.clone(),
                event_revision: object.event_revision,
                attachment_id: object.attachment_id.clone(),
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

fn seal_event_page_token(
    state: &ServerState,
    cursor: String,
) -> Result<String, ControlCommandError> {
    let ttl_ms = EVENT_PAGE_TOKEN_TTL
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    seal_event_page_token_until(state, cursor, unix_time_ms().saturating_add(ttl_ms))
}

fn seal_event_page_token_until(
    state: &ServerState,
    cursor: String,
    expires_at_ms: u64,
) -> Result<String, ControlCommandError> {
    let payload = serde_json::to_vec(&EventPageToken {
        cursor,
        expires_at_ms,
    })
    .map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "event search page token could not be encoded",
        )
    })?;
    let signature = hmac_sha256(&state.event_page_token_key, &payload);
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(payload),
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

fn open_event_page_token(state: &ServerState, token: &str) -> Result<String, ControlCommandError> {
    let invalid = || {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search page token is invalid",
        )
    };
    let (payload, signature) = token.split_once('.').ok_or_else(invalid)?;
    let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| invalid())?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| invalid())?;
    let mut verifier = Hmac::<Sha256>::new_from_slice(state.event_page_token_key.as_ref())
        .expect("HMAC-SHA256 accepts 32-byte keys");
    verifier.update(&payload);
    if verifier.verify_slice(&signature).is_err() {
        return Err(invalid());
    }
    let token: EventPageToken = serde_json::from_slice(&payload).map_err(|_| invalid())?;
    if token.cursor.is_empty() || token.cursor.len() > 4_096 {
        return Err(invalid());
    }
    if token.expires_at_ms <= unix_time_ms() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "event search page token expired; restart the query",
        ));
    }
    Ok(token.cursor)
}

fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    let mut signer = Hmac::<Sha256>::new_from_slice(key).expect("HMAC-SHA256 accepts 32-byte keys");
    signer.update(message);
    signer.finalize().into_bytes().into()
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

fn storage_event_source(value: i32) -> Result<EventSource, ControlCommandError> {
    match proto::EventOrigin::try_from(value) {
        Ok(proto::EventOrigin::Camera) => Ok(EventSource::Camera),
        Ok(proto::EventOrigin::Keeppeek) => Ok(EventSource::KeepPeek),
        _ => Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event origin filter is invalid",
        )),
    }
}

fn validate_event_metadata_search(
    search: &proto::EventMetadataSearch,
) -> Result<(), ControlCommandError> {
    const MAX_FILTER_VALUES: usize = 64;
    if search.event_ids.len() > MAX_FILTER_VALUES
        || search.source_ids.len() > MAX_FILTER_VALUES
        || search.event_types.len() > MAX_FILTER_VALUES
        || search.origins.len() > MAX_FILTER_VALUES
        || search.zones.len() > MAX_FILTER_VALUES
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event metadata filter count exceeds 64",
        ));
    }
    for event_id in &search.event_ids {
        validate_client_id(event_id, "event metadata event ID")?;
    }
    for value in search.event_types.iter().chain(&search.zones) {
        let value = value.trim();
        if value.is_empty() || value.len() > 256 {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event metadata filter values must contain 1 to 256 UTF-8 bytes",
            ));
        }
    }
    if search
        .minimum_confidence
        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event minimum confidence must be between zero and one",
        ));
    }
    if let Some(text) = search.text.as_deref() {
        let text = text.trim();
        if text.is_empty() || text.len() > 256 {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event metadata text must contain 1 to 256 UTF-8 bytes",
            ));
        }
    }
    Ok(())
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

fn validate_event_search_source(
    state: &ServerState,
    source_id: &str,
    stream_id: &str,
) -> Result<(), ControlCommandError> {
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
        .any(|profile| profile.stream == stream_id)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "event search stream was not found",
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
    for hit in hits {
        if !hit.keyframes.is_empty() {
            continue;
        }
        let Some(camera) = state.camera(&hit.source_id) else {
            continue;
        };
        let stored_stream_id = recording_stream(&camera, stream_id);
        if let Some(mut keyframe) = catalog
            .resolve_event_keyframe(&hit.event_id, stored_stream_id)
            .map_err(|error| stored_catalog_error("resolve event keyframe", error))?
        {
            keyframe.stream_id = stream_id.to_owned();
            hit.keyframes = vec![keyframe];
        }
    }
    Ok(())
}

fn refresh_event_search_image_availability(
    state: &ServerState,
    hits: &mut [crate::storage::EventSearchHit],
) {
    for hit in hits {
        hit.image_available = hit
            .canonical_attachment
            .as_ref()
            .is_some_and(crate::storage::metadata::is_supported_event_image)
            && state.events.as_ref().is_some_and(|store| {
                store
                    .thumbnail_path(&hit.source_id, &hit.event_id)
                    .ok()
                    .flatten()
                    .is_some()
            });
    }
}

fn proto_event_search_hit(hit: crate::storage::EventSearchHit) -> proto::EventSearchHit {
    let source_id = hit.source_id.clone();
    let image_availability =
        proto_event_image_availability(hit.has_image_attachment, hit.image_available);
    proto::EventSearchHit {
        event_id: hit.event_id,
        revision: hit.revision,
        source_id: source_id.clone(),
        event_type: hit.event_type,
        origin: match hit.origin {
            EventSource::Camera => proto::EventOrigin::Camera as i32,
            EventSource::KeepPeek => proto::EventOrigin::Keeppeek as i32,
        },
        start_time: Some(millis_timestamp(hit.start_time_ms)),
        end_time: hit.end_time_ms.map(millis_timestamp),
        confidence: hit.confidence,
        bounding_box: hit
            .bbox
            .map(|[x, y, width, height]| proto::EventBoundingBox {
                x,
                y,
                width,
                height,
            }),
        zone: hit.zone,
        text: hit.text,
        has_image_attachment: hit.has_image_attachment,
        canonical_attachment: hit
            .canonical_attachment
            .map(proto_event_attachment_descriptor),
        attachments: hit
            .attachments
            .into_iter()
            .map(proto_event_attachment_descriptor)
            .collect(),
        icon_key: Some(hit.icon_key),
        rejected_icon_key: hit.rejected_icon_key,
        bounding_box_attachment_id: hit.bbox_attachment_id,
        image_availability,
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

fn proto_event_attachment_descriptor(
    attachment: EventAttachment,
) -> proto::EventAttachmentDescriptor {
    proto::EventAttachmentDescriptor {
        attachment_id: attachment.id,
        attachment_type: attachment.attachment_type,
        content_type: attachment.content_type,
        byte_len: attachment.byte_len,
        ordinal: attachment.ordinal,
        timestamp: attachment.timestamp_ms.map(millis_timestamp),
        text: attachment.text,
    }
}

fn proto_event_payload(payload: serde_json::Map<String, serde_json::Value>) -> prost_types::Struct {
    prost_types::Struct {
        fields: payload
            .into_iter()
            .map(|(key, value)| (key, proto_event_payload_value(value)))
            .collect(),
    }
}

fn proto_event_payload_value(value: serde_json::Value) -> prost_types::Value {
    let kind = match value {
        serde_json::Value::Null => prost_types::value::Kind::NullValue(0),
        serde_json::Value::Bool(value) => prost_types::value::Kind::BoolValue(value),
        serde_json::Value::Number(value) => {
            prost_types::value::Kind::NumberValue(value.as_f64().unwrap_or_default())
        }
        serde_json::Value::String(value) => prost_types::value::Kind::StringValue(value),
        serde_json::Value::Array(values) => {
            prost_types::value::Kind::ListValue(prost_types::ListValue {
                values: values.into_iter().map(proto_event_payload_value).collect(),
            })
        }
        serde_json::Value::Object(value) => {
            prost_types::value::Kind::StructValue(proto_event_payload(value))
        }
    };
    prost_types::Value { kind: Some(kind) }
}

const fn proto_event_image_availability(has_image: bool, available: bool) -> i32 {
    if !has_image {
        proto::EventImageAvailability::None as i32
    } else if available {
        proto::EventImageAvailability::Available as i32
    } else {
        proto::EventImageAvailability::Unavailable as i32
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
    access: &crate::access::CameraAccess,
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

    let mut cameras = camera_access::query_cameras(state, access, &query.source_ids)?;
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
            let mut physical_ranges = HashMap::<String, Vec<(i64, i64)>>::new();
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
                let physical = if let Some(ranges) = physical_ranges.get(&stream_id) {
                    ranges.clone()
                } else {
                    let queried = if bucket_ms > 0 {
                        catalog
                            .availability_ranges_in_range(&stream_id, start_ms, end_ms, bucket_ms)
                            .map_err(|error| {
                                ControlCommandError::new(
                                    proto::ErrorCode::Internal,
                                    500,
                                    format!("unable to query recording availability: {error}"),
                                )
                            })?
                    } else {
                        catalog
                            .media_fragments_in_range(&stream_id, start_ms, end_ms)
                            .map_err(|error| {
                                ControlCommandError::new(
                                    proto::ErrorCode::Internal,
                                    500,
                                    format!("unable to query recording availability: {error}"),
                                )
                            })?
                            .into_iter()
                            .map(|fragment| {
                                let end = fragment.start_ms.saturating_add(
                                    i64::try_from(fragment.duration_ms).unwrap_or(i64::MAX),
                                );
                                (fragment.start_ms.max(start_ms), end.min(end_ms))
                            })
                            .collect()
                    };
                    physical_ranges.insert(stream_id.clone(), queried.clone());
                    queried
                };
                ranges.extend(physical.into_iter().map(|(range_start, range_end)| {
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
        let Ok(payload) = std::fs::read(&attachment.path) else {
            continue;
        };
        if attachment
            .descriptor
            .byte_len
            .is_some_and(|byte_len| byte_len != payload.len() as u64)
        {
            continue;
        }
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
                                revision: attachment.revision,
                                attachment_id: attachment.descriptor.id.clone(),
                                attachment_type: attachment.descriptor.attachment_type.clone(),
                                content_type: attachment.descriptor.content_type.clone(),
                                ordinal: attachment.descriptor.ordinal,
                                timestamp: attachment.descriptor.timestamp_ms.map(millis_timestamp),
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
            let canonical_descriptor = event.canonical_attachment().cloned();
            let attachment = canonical_descriptor
                .as_ref()
                .filter(|descriptor| crate::storage::metadata::is_supported_event_image(descriptor))
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
            let image_available = attachment.is_some();
            let descriptors = event
                .attachments
                .clone()
                .into_iter()
                .map(proto_event_attachment_descriptor)
                .collect();
            let stored_attachment = if selection.include_attachments {
                attachment.and_then(|(path, byte_len)| {
                    let descriptor = canonical_descriptor.clone()?;
                    (descriptor
                        .byte_len
                        .is_none_or(|expected| expected == byte_len))
                    .then_some(StoredTimelineAttachment {
                        event_id: event.id.clone(),
                        revision: event.revision,
                        descriptor,
                        path,
                    })
                })
            } else {
                None
            };
            let image_availability = proto_event_image_availability(
                event.canonical_attachment_id.is_some(),
                image_available,
            );
            results.push(StoredTimelineEvent {
                event: proto::Event {
                    event_id: event.id,
                    revision: event.revision,
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
                    text: event.text,
                    payload: event.payload.map(proto_event_payload),
                    attachments: descriptors,
                    source_session_id: None,
                    subscription_id: None,
                    canonical_attachment_id: event.canonical_attachment_id,
                    icon_key: Some(event.icon_key),
                    rejected_icon_key: event.rejected_icon_key,
                    bounding_box_attachment_id: event.bbox_attachment_id,
                    image_availability,
                },
                attachment: stored_attachment,
            });
        }
        let operational_events = store
            .operational_events_in_range(&camera.info.id, start_ms, end_ms)
            .map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::Internal,
                    500,
                    format!("unable to query stored operational events: {error}"),
                )
            })?;
        for event in operational_events {
            if !event_types.is_empty() && !event_types.contains(event.key.kind.as_str()) {
                continue;
            }
            results.push(StoredTimelineEvent {
                event: health_snapshot::proto_operational_event(event),
                attachment: None,
            });
        }
    }
    results.sort_unstable_by(|left, right| {
        let timestamp = |event: &proto::Event| {
            event
                .start_time
                .as_ref()
                .map_or((i64::MIN, i32::MIN), |value| (value.seconds, value.nanos))
        };
        timestamp(&left.event)
            .cmp(&timestamp(&right.event))
            .then(left.event.event_id.cmp(&right.event.event_id))
    });
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

fn validate_export_job_id(value: &str) -> Result<(), ControlCommandError> {
    validate_client_id(value, "export job ID")?;
    if !safe_export_job_id(value) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export job ID may contain only letters, digits, periods, hyphens, and underscores",
        ));
    }
    Ok(())
}

fn safe_export_job_id(value: &str) -> bool {
    !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
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
            configuration_revision: config.configuration_revision,
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
                minimum_free_gb: Some(config.storage.minimum_free_gb),
                maximum_used_percent: Some(
                    config.storage.maximum_used_percent.map_or(0, u32::from),
                ),
                warning_free_gb: Some(config.storage.warning_free_gb),
                critical_free_gb: Some(config.storage.critical_free_gb),
                cleanup_hysteresis_gb: Some(config.storage.cleanup_hysteresis_gb),
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
        expected_configuration_revision: update.expected_configuration_revision,
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
        groups: Vec::new(),
        battery_uid,
        recording_label: camera_config
            .name
            .clone()
            .unwrap_or_else(|| camera_config.ip.to_string()),
        control,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ApiPrincipalIdentity {
    Local(IpAddr),
    Credential { id: Uuid, revision: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ApiPrincipal {
    identity: ApiPrincipalIdentity,
    display_name: String,
    role: AccessRole,
    credential_expires_at_ms: Option<i64>,
}

impl ApiPrincipal {
    fn local(address: IpAddr) -> Self {
        Self {
            identity: ApiPrincipalIdentity::Local(address),
            display_name: "Local Administrator".to_owned(),
            role: AccessRole::Administrator,
            credential_expires_at_ms: None,
        }
    }

    fn credential(credential: AuthenticatedCredential) -> Self {
        Self {
            identity: ApiPrincipalIdentity::Credential {
                id: credential.id,
                revision: credential.revision,
            },
            display_name: credential.name,
            role: credential.role,
            credential_expires_at_ms: credential.expires_at_ms,
        }
    }

    fn id(&self) -> String {
        match self.identity {
            ApiPrincipalIdentity::Local(_) => "local-administrator".to_owned(),
            ApiPrincipalIdentity::Credential { id, .. } => id.to_string(),
        }
    }

    const fn is_local(&self) -> bool {
        matches!(self.identity, ApiPrincipalIdentity::Local(_))
    }

    const fn credential_binding(&self) -> Option<(Uuid, u64)> {
        match self.identity {
            ApiPrincipalIdentity::Local(_) => None,
            ApiPrincipalIdentity::Credential { id, revision } => Some((id, revision)),
        }
    }
}

#[derive(Clone)]
struct ApiSessionRecord {
    principal: ApiPrincipal,
    classification: ClientClassification,
    created_at_ms: i64,
    last_activity_at_ms: i64,
    absolute_expires_at_ms: i64,
    last_activity: Instant,
}

#[derive(Clone, Copy)]
struct ApiSessionPolicy {
    idle_timeout: Duration,
    absolute_timeout: Duration,
    max_per_principal: usize,
    max_per_address: usize,
    failed_authentication_limit: u32,
    failed_authentication_window: Duration,
}

struct HttpStreamCancellation {
    credential_id: Uuid,
    credential_revision: u64,
    cancelled: Weak<AtomicBool>,
}

#[derive(Default)]
struct AccessMetrics {
    authentication_successes: AtomicU64,
    authentication_failures: AtomicU64,
    authorization_denials: AtomicU64,
    sessions_created: AtomicU64,
    sessions_revoked_or_expired: AtomicU64,
}

const fn proto_access_role(role: AccessRole) -> i32 {
    match role {
        AccessRole::Administrator => proto::AccessRole::Administrator as i32,
        AccessRole::User => proto::AccessRole::User as i32,
    }
}

fn proto_access_credential(credential: CredentialMetadata) -> proto::AccessCredential {
    proto::AccessCredential {
        credential_id: credential.id.to_string(),
        name: credential.name,
        description: credential.description,
        role: proto_access_role(credential.role),
        created_at_ms: credential.created_at_ms,
        last_used_at_ms: credential.last_used_at_ms,
        expires_at_ms: credential.expires_at_ms,
        disabled: credential.disabled,
        revoked_at_ms: credential.revoked_at_ms,
        revision: credential.revision,
        rotated_at_ms: credential.rotated_at_ms,
        initial_access_key_pending: credential.initial_access_key_pending,
    }
}

fn proto_issued_credential(credential: IssuedCredential) -> proto::AccessCredentialResult {
    proto::AccessCredentialResult {
        credentials: vec![proto_access_credential(credential.metadata)],
        access_key: Some(credential.access_key.canonical()),
    }
}

fn proto_access_session(session_id: SessionId, session: &ApiSessionRecord) -> proto::AccessSession {
    proto::AccessSession {
        session_id: session_id.to_string(),
        principal_id: session.principal.id(),
        display_name: session.principal.display_name.clone(),
        role: proto_access_role(session.principal.role),
        local: session.principal.is_local(),
        client_classification: session.classification.reason.as_str().to_owned(),
        created_at_ms: session.created_at_ms,
        last_activity_at_ms: session.last_activity_at_ms,
        absolute_expires_at_ms: session.absolute_expires_at_ms,
        credential_expires_at_ms: session.principal.credential_expires_at_ms,
    }
}

fn proto_access_audit_event(event: AccessAuditEvent) -> proto::AccessAuditEvent {
    proto::AccessAuditEvent {
        event_id: event.id.to_string(),
        timestamp_ms: event.timestamp_ms,
        principal_id: event.principal_id,
        role: event.role.map_or(0, proto_access_role),
        action: event.action,
        target_id: event.target_id,
        result: event.result,
        client_classification: event.client_classification,
    }
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
    requester_id: String,
    artifact_id: String,
    request: proto::CreateExportJob,
    job: proto::ExportJob,
    path: Option<PathBuf>,
    cancel: Arc<AtomicBool>,
    created_at_ms: i64,
    started_at_ms: Option<i64>,
    updated_at_ms: i64,
    completed_at_ms: Option<i64>,
    downloaded_at_ms: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct PersistedExportHistory {
    version: u32,
    jobs: Vec<PersistedExportJobRecord>,
}

#[derive(Serialize, Deserialize)]
struct PersistedExportJobRecord {
    requester_id: String,
    artifact_id: String,
    request: String,
    job: String,
    created_at_ms: i64,
    started_at_ms: Option<i64>,
    updated_at_ms: i64,
    completed_at_ms: Option<i64>,
    downloaded_at_ms: Option<i64>,
}

impl PersistedExportJobRecord {
    fn from_record(record: &ExportJobRecord) -> Self {
        Self {
            requester_id: record.requester_id.clone(),
            artifact_id: record.artifact_id.clone(),
            request: URL_SAFE_NO_PAD.encode(record.request.encode_to_vec()),
            job: URL_SAFE_NO_PAD.encode(record.job.encode_to_vec()),
            created_at_ms: record.created_at_ms,
            started_at_ms: record.started_at_ms,
            updated_at_ms: record.updated_at_ms,
            completed_at_ms: record.completed_at_ms,
            downloaded_at_ms: record.downloaded_at_ms,
        }
    }

    fn into_record(self, export_root: &Path, now_ms: i64) -> anyhow::Result<ExportJobRecord> {
        let request_bytes = URL_SAFE_NO_PAD.decode(self.request)?;
        let job_bytes = URL_SAFE_NO_PAD.decode(self.job)?;
        let request = proto::CreateExportJob::decode(request_bytes.as_slice())?;
        let mut job = proto::ExportJob::decode(job_bytes.as_slice())?;
        anyhow::ensure!(
            request.job_id == job.job_id
                && safe_export_job_id(&job.job_id)
                && safe_export_job_id(&self.artifact_id),
            "persisted export job identity is invalid"
        );

        let previous_completed_at_ms = self.completed_at_ms;
        let mut completed_at_ms = previous_completed_at_ms;
        let path = match proto::ExportJobStatus::try_from(job.status) {
            Ok(proto::ExportJobStatus::Running) => {
                job.status = proto::ExportJobStatus::Failed as i32;
                job.error = Some("Server restarted before the export completed".to_owned());
                job.retryable = true;
                completed_at_ms = Some(now_ms);
                None
            }
            Ok(proto::ExportJobStatus::Ready) => {
                let file_name = job
                    .file_name
                    .as_deref()
                    .filter(|name| safe_export_path_component(name))
                    .ok_or_else(|| anyhow::anyhow!("ready export has an invalid file name"))?;
                let path = export_root
                    .join(&job.job_id)
                    .join(&self.artifact_id)
                    .join(file_name);
                if path.is_file() {
                    Some(path)
                } else {
                    job.status = proto::ExportJobStatus::Failed as i32;
                    job.error = Some("Export artifact is missing; retry the export".to_owned());
                    job.retryable = true;
                    completed_at_ms = Some(now_ms);
                    None
                }
            }
            Ok(
                proto::ExportJobStatus::Partial
                | proto::ExportJobStatus::Failed
                | proto::ExportJobStatus::Cancelled
                | proto::ExportJobStatus::Expired,
            ) => None,
            Ok(proto::ExportJobStatus::Unspecified) | Err(_) => {
                anyhow::bail!("persisted export job status is invalid")
            }
        };
        let recovered = completed_at_ms != previous_completed_at_ms;
        if path.is_none() {
            let _ = cleanup_export_attempt_directory(export_root, &job.job_id, &self.artifact_id);
        }
        Ok(ExportJobRecord {
            requester_id: self.requester_id,
            artifact_id: self.artifact_id,
            request,
            job,
            path,
            cancel: Arc::new(AtomicBool::new(false)),
            created_at_ms: self.created_at_ms,
            started_at_ms: self.started_at_ms,
            updated_at_ms: if recovered {
                now_ms
            } else {
                self.updated_at_ms
            },
            completed_at_ms,
            downloaded_at_ms: self.downloaded_at_ms,
        })
    }
}

fn safe_export_path_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

fn export_history_path(storage: &StorageConfig) -> PathBuf {
    storage
        .long_term_path
        .join(".exports")
        .join(EXPORT_HISTORY_FILE)
}

fn load_export_jobs(history_path: &Path) -> anyhow::Result<HashMap<String, ExportJobRecord>> {
    let metadata = match std::fs::metadata(history_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.len() <= MAX_EXPORT_HISTORY_BYTES,
        "export history exceeds {MAX_EXPORT_HISTORY_BYTES} bytes"
    );
    let bytes = std::fs::read(history_path)?;
    anyhow::ensure!(
        u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= MAX_EXPORT_HISTORY_BYTES,
        "export history exceeds {MAX_EXPORT_HISTORY_BYTES} bytes"
    );
    let history: PersistedExportHistory = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        history.version == EXPORT_HISTORY_VERSION,
        "unsupported export history version {}",
        history.version
    );
    let export_root = history_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("export history has no parent directory"))?;
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let mut jobs = HashMap::new();
    for persisted in history.jobs {
        match persisted.into_record(export_root, now_ms) {
            Ok(record) => {
                jobs.insert(record.job.job_id.clone(), record);
            }
            Err(error) => tracing::warn!(%error, "ignoring invalid persisted export job"),
        }
    }
    let retention_ms = i64::try_from(EXPORT_METADATA_RETENTION.as_millis()).unwrap_or(i64::MAX);
    let retained_after_ms = now_ms.saturating_sub(retention_ms);
    let mut records = jobs.into_values().collect::<Vec<_>>();
    records.sort_unstable_by_key(|record| std::cmp::Reverse(record.updated_at_ms));
    let mut retained = HashMap::new();
    for record in records {
        if record.updated_at_ms >= retained_after_ms && retained.len() < MAX_EXPORT_HISTORY_JOBS {
            retained.insert(record.job.job_id.clone(), record);
        } else {
            let _ = cleanup_export_attempt_directory(
                export_root,
                &record.job.job_id,
                &record.artifact_id,
            );
        }
    }
    Ok(retained)
}

fn persist_export_jobs(
    history_path: &Path,
    jobs: &HashMap<String, ExportJobRecord>,
) -> anyhow::Result<()> {
    let mut records = jobs.values().collect::<Vec<_>>();
    records.sort_unstable_by(|left, right| left.job.job_id.cmp(&right.job.job_id));
    let history = PersistedExportHistory {
        version: EXPORT_HISTORY_VERSION,
        jobs: records
            .into_iter()
            .map(PersistedExportJobRecord::from_record)
            .collect(),
    };
    let serialized = serde_json::to_vec(&history)?;
    config::write_private_file_atomically(history_path, &serialized)?;
    Ok(())
}

#[derive(Clone)]
pub struct ServerState {
    host: String,
    port: u16,
    access_key: Arc<RwLock<AccessKey>>,
    access_manager: AccessManager,
    access_metrics: Arc<AccessMetrics>,
    network_access: NetworkAccessPolicy,
    require_secure_remote: bool,
    api_session_policy: ApiSessionPolicy,
    allowed_origins: Arc<HashSet<String>>,
    api_session_owners: Arc<Mutex<HashMap<SessionId, ApiSessionRecord>>>,
    http_stream_cancellations: Arc<Mutex<Vec<HttpStreamCancellation>>>,
    stored_media_cursors: Arc<Mutex<HashMap<(SessionId, String), StoredMediaCursor>>>,
    stored_media_cursor_reservations: Arc<Mutex<HashSet<(SessionId, String)>>>,
    ptz_owners: Arc<Mutex<HashMap<String, SessionId>>>,
    export_jobs: Arc<Mutex<HashMap<String, ExportJobRecord>>>,
    export_history_path: Option<Arc<PathBuf>>,
    event_search_tasks: EventSearchTasks,
    event_publications: event_publication::Registry,
    event_subscriptions: event_subscription::Registry,
    event_page_token_key: Arc<[u8; 32]>,
    camera_discovery_tasks: camera_discovery::Registry,
    configuration_plans: configuration::Registry,
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
    backup_manager: Option<Arc<BackupManager>>,
    logging: Option<LoggingService>,
    notifications: Option<NotificationHandle>,
    event_forwarder: Option<EventForwarderHandle>,
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
        let mut groups_by_ip = HashMap::<IpAddr, Vec<String>>::new();
        for (group, group_cameras) in camera_configs {
            for camera in group_cameras {
                groups_by_ip
                    .entry(camera.ip)
                    .or_default()
                    .push(group.clone());
            }
        }
        for groups in groups_by_ip.values_mut() {
            groups.sort_unstable();
            groups.dedup();
        }
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
            .map(|camera_config| {
                let mut entry = camera_entry(camera_config, cameras.get(&camera_config.ip));
                entry.groups = groups_by_ip
                    .get(&camera_config.ip)
                    .cloned()
                    .unwrap_or_default();
                entry
            })
            .collect::<Vec<_>>();
        entries.sort_unstable_by(|left, right| left.info.id.cmp(&right.info.id));
        let camera_count = entries.len();
        let sanitized_config = sanitized_config(config, storage, camera_count, &entries);
        let access_manager = AccessManager::ephemeral(config.access_key);
        access_manager.configure_rate_limit(
            config.access.failed_authentication_limit,
            Duration::from_secs(config.access.failed_authentication_window_secs),
        );
        let export_history_path = export_history_path(storage);
        let export_jobs = load_export_jobs(&export_history_path).unwrap_or_else(|error| {
            tracing::warn!(%error, path = %export_history_path.display(), "unable to load export history");
            HashMap::new()
        });
        if let Err(error) = persist_export_jobs(&export_history_path, &export_jobs) {
            tracing::warn!(%error, path = %export_history_path.display(), "unable to persist recovered export history");
        }

        Self {
            host: config.host.clone(),
            port: config.port,
            access_key: Arc::new(RwLock::new(config.access_key)),
            access_manager,
            access_metrics: Arc::new(AccessMetrics::default()),
            network_access: NetworkAccessPolicy::new(
                config.access.local_networks.clone(),
                config.access.trusted_proxies.clone(),
            ),
            require_secure_remote: config.access.require_secure_remote,
            api_session_policy: ApiSessionPolicy {
                idle_timeout: Duration::from_secs(config.access.session_idle_timeout_secs),
                absolute_timeout: Duration::from_secs(config.access.session_absolute_timeout_secs),
                max_per_principal: config.access.max_sessions_per_principal as usize,
                max_per_address: config.access.max_sessions_per_address as usize,
                failed_authentication_limit: config.access.failed_authentication_limit,
                failed_authentication_window: Duration::from_secs(
                    config.access.failed_authentication_window_secs,
                ),
            },
            allowed_origins: Arc::new(config.direct_card.allowed_origins.iter().cloned().collect()),
            api_session_owners: Arc::new(Mutex::new(HashMap::new())),
            http_stream_cancellations: Arc::new(Mutex::new(Vec::new())),
            stored_media_cursors: Arc::new(Mutex::new(HashMap::new())),
            stored_media_cursor_reservations: Arc::new(Mutex::new(HashSet::new())),
            ptz_owners: Arc::new(Mutex::new(HashMap::new())),
            export_jobs: Arc::new(Mutex::new(export_jobs)),
            export_history_path: Some(Arc::new(export_history_path)),
            event_search_tasks: Arc::new(Mutex::new(HashMap::new())),
            event_publications: event_publication::Registry::default(),
            event_subscriptions: event_subscription::Registry::default(),
            event_page_token_key: Arc::new(rand::random()),
            camera_discovery_tasks: camera_discovery::Registry::default(),
            configuration_plans: configuration::Registry::default(),
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
            backup_manager: None,
            logging: None,
            notifications: None,
            event_forwarder: None,
            started_at: Instant::now(),
        }
    }

    fn empty() -> Self {
        let config = Config::default();
        let storage = StorageConfig {
            long_term_path: std::env::temp_dir().join(format!(
                "keeppeek-test-export-history-{}",
                rand::random::<u64>()
            )),
            ..StorageConfig::default()
        };
        let mut state = Self::new(
            &config,
            &HashMap::new(),
            &HashMap::new(),
            &storage,
            RecordingDemand::new(TEST_RECORDING_DEMAND_GRACE),
            WebRtc::new(),
        );
        let _ = std::fs::remove_dir_all(&storage.long_term_path);
        state.export_history_path = None;
        state
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

    fn upsert_camera(&self, mut entry: CameraEntry) {
        let mut cameras = self
            .cameras
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed_groups;
        if let Some(existing) = cameras
            .iter_mut()
            .find(|camera| camera.info.id == entry.info.id)
        {
            if entry.groups.is_empty() {
                entry.groups.clone_from(&existing.groups);
            }
            changed_groups = if entry.groups == existing.groups {
                Vec::new()
            } else {
                entry
                    .groups
                    .iter()
                    .chain(&existing.groups)
                    .cloned()
                    .collect()
            };
            *existing = entry;
        } else {
            changed_groups = entry.groups.clone();
            cameras.push(entry);
        }
        cameras.sort_unstable_by(|left, right| left.info.id.cmp(&right.info.id));
        drop(cameras);
        if !changed_groups.is_empty() {
            camera_access::invalidate_group_sessions(self, &changed_groups);
        }
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

    pub(crate) fn with_access_manager(mut self, access_manager: AccessManager) -> Self {
        access_manager.configure_rate_limit(
            self.api_session_policy.failed_authentication_limit,
            self.api_session_policy.failed_authentication_window,
        );
        self.access_manager = access_manager;
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

    pub fn with_backup_manager(mut self, manager: BackupManager) -> Self {
        self.backup_manager = Some(Arc::new(manager));
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

    pub(crate) fn with_notifications(mut self, notifications: NotificationHandle) -> Self {
        self.notifications = Some(notifications);
        self
    }

    pub(crate) fn configuration_update_lock(&self) -> Arc<Mutex<()>> {
        self.config_update.clone()
    }

    pub(crate) fn with_event_forwarder(mut self, event_forwarder: EventForwarderHandle) -> Self {
        self.event_forwarder = Some(event_forwarder);
        self
    }
}

#[allow(clippy::too_many_arguments)]
fn record_access_audit(
    state: &ServerState,
    timestamp_ms: i64,
    principal_id: Option<&str>,
    role: Option<AccessRole>,
    action: &str,
    target_id: Option<&str>,
    result: &str,
    classification: ClientClassificationReason,
) {
    state.access_manager.record_audit(NewAccessAuditEvent {
        timestamp_ms,
        principal_id,
        role,
        action,
        target_id,
        result,
        client_classification: classification,
    });
}

fn access_role_from_proto(role: i32) -> Result<AccessRole, ControlCommandError> {
    match proto::AccessRole::try_from(role) {
        Ok(proto::AccessRole::Administrator) => Ok(AccessRole::Administrator),
        Ok(proto::AccessRole::User) => Ok(AccessRole::User),
        Ok(proto::AccessRole::Unspecified) | Err(_) => Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "credential role must be Administrator or User",
        )),
    }
}

fn parse_credential_id(value: &str) -> Result<Uuid, ControlCommandError> {
    Uuid::parse_str(value).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "credential ID is invalid",
        )
    })
}

fn access_command_error(operation: &str, error: anyhow::Error) -> ControlCommandError {
    tracing::warn!(%error, %operation, "access command failed");
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        409,
        format!("unable to {operation}: {error}"),
    )
}

fn close_api_session(state: &ServerState, session_id: SessionId) {
    let closed_session = state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&session_id);
    if let Some(session) = closed_session {
        record_access_audit(
            state,
            i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
            Some(&session.principal.id()),
            Some(session.principal.role),
            "session_closed",
            Some(&session_id.to_string()),
            "success",
            session.classification.reason,
        );
    }
    event_search::close_session(state, session_id);
    state.event_publications.close_session(session_id);
    state.event_subscriptions.close_session(session_id);
    state.camera_discovery_tasks.close_session(session_id);
    stored_media::close_session(state, session_id);
    let source_ids = {
        let mut owners = state
            .ptz_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source_ids = owners
            .iter()
            .filter_map(|(source_id, owner)| (*owner == session_id).then_some(source_id.clone()))
            .collect::<Vec<_>>();
        owners.retain(|_, owner| *owner != session_id);
        source_ids
    };
    for source_id in source_ids {
        if let Some(camera) = state.camera(&source_id)
            && let Some(control) = camera.control
            && let Err(error) = reolink_ptz(&control, PtzOp::Stop, PTZ_STOP_SPEED)
        {
            tracing::warn!(%source_id, %error, "unable to stop session-owned PTZ movement");
        }
    }
}

fn close_api_sessions_action(webrtc: WebRtc, sessions: Vec<SessionId>) -> PostSendAction {
    Box::new(move || {
        for session_id in sessions {
            webrtc.request_api_session_close(session_id);
        }
    })
}

fn cancel_http_streams_for_credential(state: &ServerState, credential_id: Uuid) {
    let mut streams = state
        .http_stream_cancellations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    streams.retain(|stream| {
        let Some(cancelled) = stream.cancelled.upgrade() else {
            return false;
        };
        if stream.credential_id == credential_id {
            cancelled.store(true, Ordering::Release);
            return false;
        }
        true
    });
}

fn notification_command_error(
    operation: &str,
    error: anyhow::Error,
    invalid_fallback: bool,
) -> ControlCommandError {
    match error.downcast_ref::<RuleStoreError>() {
        Some(RuleStoreError::Conflict {
            active_revision,
            draft_revision,
        }) => ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "notification rule revision conflict",
        )
        .with_detail(prost_types::Any {
            type_url: "type.keeppeek.dev/notification-rule-conflict.v1".to_owned(),
            value: serde_json::to_vec(&serde_json::json!({
                "active_revision": active_revision,
                "draft_revision": draft_revision,
            }))
            .unwrap_or_default(),
        }),
        Some(RuleStoreError::NotFound) => ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "notification rule was not found",
        ),
        Some(RuleStoreError::NotAuthorized) => ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "notification resource is not owned by this principal",
        ),
        None if invalid_fallback => ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("unable to {operation}: {error}"),
        ),
        None => {
            tracing::warn!(%operation, error = %error, "notification command failed");
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to {operation}"),
            )
        }
    }
}

fn notification_page_limit(limit: Option<u32>) -> usize {
    usize::try_from(limit.unwrap_or(100).clamp(1, 200)).unwrap_or(200)
}

fn proto_notification_rule_record(
    record: RuleRecord,
) -> anyhow::Result<proto::NotificationRuleRecord> {
    Ok(proto::NotificationRuleRecord {
        rule_id: record.id,
        owner_id: record.owner_id,
        active_definition_json: record
            .active
            .map(|rule| redacted_notification_rule_json(&rule))
            .transpose()?,
        active_revision: record.active_revision,
        draft_definition_json: redacted_notification_rule_json(&record.draft)?,
        draft_revision: record.draft_revision,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
        last_match_at_ms: record.last_match_at_ms,
        last_delivery_at_ms: record.last_delivery_at_ms,
    })
}

fn redacted_notification_rule_json(rule: &NotificationRule) -> anyhow::Result<String> {
    let mut definition = serde_json::to_value(rule)?;
    if let Some(actions) = definition
        .get_mut("actions")
        .and_then(serde_json::Value::as_array_mut)
    {
        for (action, configured_action) in actions.iter_mut().zip(&rule.actions) {
            let Some(action) = action.as_object_mut() else {
                continue;
            };
            if !configured_action.destination.is_empty() {
                if configured_action.channel == crate::notifications::model::Channel::Push
                    && let Ok(public_config) = crate::notifications::pushover::public_config(
                        &configured_action.destination,
                    )
                {
                    action.insert("pushover".to_owned(), serde_json::to_value(public_config)?);
                }
                action.insert(
                    "destination".to_owned(),
                    serde_json::Value::String(String::new()),
                );
                action.insert(
                    "destination_configured".to_owned(),
                    serde_json::Value::Bool(true),
                );
                action.insert(
                    "destination_ref".to_owned(),
                    serde_json::Value::String(notification_destination_ref(
                        &configured_action.destination,
                    )),
                );
            }
        }
    }
    serde_json::to_string(&definition).map_err(Into::into)
}

fn restore_notification_destinations(
    definition: &mut serde_json::Value,
    existing: Option<&RuleRecord>,
) -> Result<(), ControlCommandError> {
    let Some(actions) = definition
        .get_mut("actions")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Ok(());
    };
    for action in actions.iter_mut() {
        let Some(action) = action.as_object_mut() else {
            continue;
        };
        let pushover_config = action.remove("pushover");
        let preserve = action
            .remove("destination_configured")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if !preserve {
            action.remove("destination_ref");
            continue;
        }
        let destination_ref = action
            .remove("destination_ref")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "configured notification destination has no redacted reference",
                )
            })?;
        let channel = action.get("channel").and_then(serde_json::Value::as_str);
        let previous = existing
            .into_iter()
            .flat_map(|record| record.draft.actions.iter())
            .find(|previous| {
                Some(previous.channel.as_str()) == channel
                    && notification_destination_ref(&previous.destination) == destination_ref
            })
            .map(|previous| previous.destination.as_str())
            .filter(|destination| !destination.is_empty())
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "configured notification destination could not be preserved",
                )
            })?;
        let destination = if channel == Some("push") {
            pushover_config
                .map(|config| {
                    crate::notifications::pushover::merge_public_config(previous, config).map_err(
                        |_| {
                            ControlCommandError::new(
                                proto::ErrorCode::InvalidRequest,
                                400,
                                "Pushover public configuration is invalid",
                            )
                        },
                    )
                })
                .transpose()?
                .unwrap_or_else(|| previous.to_owned())
        } else {
            previous.to_owned()
        };
        action.insert(
            "destination".to_owned(),
            serde_json::Value::String(destination),
        );
    }
    Ok(())
}

fn notification_destination_ref(destination: &str) -> String {
    encode_lower_hex(Sha256::digest(destination.as_bytes()))
}

fn proto_notification_inbox(inbox: Inbox) -> proto::NotificationInbox {
    proto::NotificationInbox {
        items: inbox
            .items
            .into_iter()
            .map(proto_notification_item)
            .collect(),
        unread_count: inbox.unread_count,
    }
}

fn proto_notification_item(item: NotificationItem) -> proto::NotificationItem {
    let has_image = item.canonical_attachment.is_some();
    proto::NotificationItem {
        logical_id: item.logical_id,
        rule_id: item.rule_id,
        source_id: item.source_id,
        source_identity: item.source_identity,
        lifecycle: item.lifecycle,
        stage: notification_stage_name(item.stage).to_owned(),
        revision: item.revision,
        title: item.title,
        body: item.body,
        deep_link: item.deep_link,
        attachment_available: item.attachment_available,
        canonical_attachment: item
            .canonical_attachment
            .map(proto_event_attachment_descriptor),
        icon_key: item.icon_key,
        image_availability: proto_event_image_availability(has_image, item.image_available),
        severity: item.severity.as_str().to_owned(),
        created_at_ms: item.created_at_ms,
        updated_at_ms: item.updated_at_ms,
        seen_at_ms: item.seen_at_ms,
        acknowledged_at_ms: item.acknowledged_at_ms,
    }
}

fn proto_notification_history_group(group: HistoryGroup) -> proto::NotificationHistoryGroup {
    proto::NotificationHistoryGroup {
        notification: Some(proto_notification_item(group.notification)),
        events: group
            .events
            .into_iter()
            .map(proto_notification_history_event)
            .collect(),
        attempts: group
            .attempts
            .into_iter()
            .map(proto_notification_attempt)
            .collect(),
    }
}

fn proto_notification_history_event(event: HistoryEvent) -> proto::NotificationHistoryEvent {
    proto::NotificationHistoryEvent {
        sequence: event.sequence,
        revision: event.revision,
        stage: notification_stage_name(event.stage).to_owned(),
        outcome: event.outcome,
        reason: event.reason,
        occurred_at_ms: event.occurred_at_ms,
        next_eligible_at_ms: event.next_eligible_at_ms,
    }
}

fn proto_notification_attempt(attempt: AttemptRecord) -> proto::NotificationDeliveryAttempt {
    proto::NotificationDeliveryAttempt {
        sequence: attempt.sequence,
        channel: attempt.channel,
        stage: notification_stage_name(attempt.stage).to_owned(),
        attempt: attempt.attempt,
        outcome: attempt.outcome,
        target_hash: attempt.target_hash,
        provider_status: attempt.provider_status.map(u32::from),
        reason: attempt.reason,
        attempted_at_ms: attempt.attempted_at_ms,
        retry_at_ms: attempt.retry_at_ms,
        provider_request_id: attempt.provider_request_id,
        provider_acknowledged_at_ms: attempt.provider_acknowledged_at_ms,
        provider_expired_at_ms: attempt.provider_expired_at_ms,
        provider_acknowledged_by_hash: attempt.provider_acknowledged_by_hash,
        provider_acknowledgement_state: attempt.provider_acknowledgement_state,
    }
}

const fn notification_stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Preliminary => "preliminary",
        Stage::Enriched => "enriched",
        Stage::Recovery => "recovery",
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
        configuration_revision: configuration_revision(config),
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
            minimum_free_gb: config.storage.minimum_free_gb,
            maximum_used_percent: config.storage.maximum_used_percent,
            warning_free_gb: config.storage.warning_free_gb,
            critical_free_gb: config.storage.critical_free_gb,
            cleanup_hysteresis_gb: config.storage.cleanup_hysteresis_gb,
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

fn configuration_revision(config: &Config) -> String {
    let serialized = toml::to_string(&config.source).unwrap_or_default();
    Sha256::digest(serialized.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn camera_configuration_revision(state: &ServerState) -> Result<String, ControlCommandError> {
    let Some(config_path) = state.camera_config_path.as_deref() else {
        return Ok(String::new());
    };
    let current = config::load_config(config_path).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to load camera configuration revision: {error}"),
        )
    })?;
    Ok(configuration_revision(&current))
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

pub(crate) fn bind_server_listener(host: &str, port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(server_bind_address(host, port))
}

pub fn serve_with_state_on_listener(
    listener: TcpListener,
    shutdown: Shutdown,
    router_tx: FacadeSender<RouterMessage>,
    state: ServerState,
) -> anyhow::Result<std::net::SocketAddr> {
    serve_with_state_on_listener_inner(listener, shutdown, router_tx, state, None)
}

pub(crate) fn serve_with_state_on_listener_ready(
    listener: TcpListener,
    shutdown: Shutdown,
    router_tx: FacadeSender<RouterMessage>,
    state: ServerState,
    ready: std::sync::mpsc::SyncSender<std::net::SocketAddr>,
) -> anyhow::Result<std::net::SocketAddr> {
    serve_with_state_on_listener_inner(listener, shutdown, router_tx, state, Some(ready))
}

fn serve_with_state_on_listener_inner(
    listener: TcpListener,
    shutdown: Shutdown,
    router_tx: FacadeSender<RouterMessage>,
    state: ServerState,
    ready: Option<std::sync::mpsc::SyncSender<std::net::SocketAddr>>,
) -> anyhow::Result<std::net::SocketAddr> {
    let logging = state.logging.clone();
    let session_reaper_state = state.clone();
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
    if let Some(ready) = ready {
        let _ = ready.send(addr);
    }

    while !shutdown.is_cancelled() {
        expire_api_sessions(&session_reaper_state);
        expire_event_publications(&session_reaper_state);
        if let Err(error) = session_reaper_state.access_manager.flush_audit(false) {
            tracing::warn!(%error, "unable to flush access audit events");
        }
        server.poll_timeout(SERVER_POLL_INTERVAL);
    }
    if let Err(error) = session_reaper_state.access_manager.flush_audit(true) {
        tracing::warn!(%error, "unable to flush access audit events during shutdown");
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

fn expire_event_publications(state: &ServerState) {
    for (session_id, publication) in state.event_publications.expire(unix_time_ms()) {
        let notification = proto::Notification {
            event: Some(proto::notification::Event::EventPublicationState(
                publication,
            )),
        };
        match state
            .webrtc
            .try_enqueue_api_notification(session_id, notification)
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(%session_id, "closing API session with a saturated notification queue");
                state.webrtc.request_api_session_close(session_id);
            }
            Err(error) if state.webrtc.has_api_session(session_id) => {
                tracing::warn!(%session_id, %error, "unable to enqueue event publication expiry");
                state.webrtc.request_api_session_close(session_id);
            }
            Err(_) => {}
        }
    }
}

fn expire_api_sessions(state: &ServerState) {
    let now_at = Instant::now();
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let expired = {
        let mut sessions = state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired = sessions
            .iter()
            .filter_map(|(session_id, session)| {
                let result =
                    if now_ms >= session.absolute_expires_at_ms {
                        Some("absolute_expiry")
                    } else if now_at.saturating_duration_since(session.last_activity)
                        >= state.api_session_policy.idle_timeout
                    {
                        Some("idle_expiry")
                    } else if !session.principal.credential_binding().is_none_or(
                        |(id, revision)| {
                            state
                                .access_manager
                                .credential_is_active(id, revision, now_ms)
                        },
                    ) {
                        Some("credential_inactive")
                    } else {
                        None
                    }?;
                Some((*session_id, session.clone(), result))
            })
            .collect::<Vec<_>>();
        let expired_ids = expired
            .iter()
            .map(|(session_id, _, _)| *session_id)
            .collect::<HashSet<_>>();
        sessions.retain(|session_id, _| !expired_ids.contains(session_id));
        expired
    };
    for (session_id, session, result) in expired {
        state
            .access_metrics
            .sessions_revoked_or_expired
            .fetch_add(1, Ordering::Relaxed);
        record_access_audit(
            state,
            now_ms,
            Some(&session.principal.id()),
            Some(session.principal.role),
            "session_expiry",
            Some(&session_id.to_string()),
            result,
            session.classification.reason,
        );
        state.webrtc.request_api_session_close(session_id);
    }
    let mut streams = state
        .http_stream_cancellations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    streams.retain(|stream| {
        let Some(cancelled) = stream.cancelled.upgrade() else {
            return false;
        };
        if !state.access_manager.credential_is_active(
            stream.credential_id,
            stream.credential_revision,
            now_ms,
        ) {
            cancelled.store(true, Ordering::Release);
            return false;
        }
        true
    });
}

pub fn run_server(
    state: ServerState,
    shutdown: Shutdown,
    router_tx: FacadeSender<RouterMessage>,
) -> anyhow::Result<()> {
    let listener = bind_server_listener(&state.host, state.port)?;
    let _ = serve_with_state_on_listener(listener, shutdown, router_tx, state)?;
    Ok(())
}

fn handle_request(
    request: &Request,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> Response {
    let request_path = request.url();
    if request_path == "/api/backups" || request_path.starts_with("/api/backups/") {
        return service_error(404, "not found");
    }
    if request.method() == "OPTIONS"
        && matches!(request_path.as_str(), "/config/apply" | "/config/export")
    {
        return config_api_preflight(request, state);
    }
    router!(request,
        (POST) (/create) => {
            authenticated_api_request(request, state, true, AccessRole::User, |identity| {
                create_api_session(request, state, identity)
            })
        },
        (POST) (/delete) => {
            authenticated_api_request(request, state, true, AccessRole::User, |identity| {
                delete_api_session(request, state, identity)
            })
        },
        (OPTIONS) (/create) => {
            api_preflight(request, state)
        },
        (OPTIONS) (/delete) => {
            api_preflight(request, state)
        },
        (GET) (/logs) => {
            authenticated_api_request(request, state, false, AccessRole::Administrator, |identity| {
                log_stream(request, state, &identity)
            })
        },
        (GET) (/logs/snapshot) => {
            authenticated_api_request(request, state, false, AccessRole::Administrator, |_| {
                log_snapshot(state)
            })
        },
        (GET) (/metrics) => {
            authenticated_api_request(request, state, false, AccessRole::Administrator, |_| {
                prometheus_metrics(router_tx, state)
            })
        },
        (GET) (/recording-coverage) => {
            authenticated_api_request(request, state, false, AccessRole::User, |identity| {
                recording_coverage::get(request, router_tx, state, &identity.principal)
            })
        },
        (GET) (/config/export) => {
            authenticated_api_request(request, state, true, AccessRole::Administrator, |identity| {
                config_export(state, &identity)
            })
        },
        (POST) (/config/apply) => {
            authenticated_api_request(request, state, true, AccessRole::Administrator, |identity| {
                config_apply(request, state, &identity)
            })
        },
        _ => serve_ui(request)
    )
}

fn config_export(state: &ServerState, identity: &AuthenticatedApiRequest) -> Response {
    let result =
        backup_manager(state).and_then(|manager| manager.export_configuration(unix_time_ms()));
    match result {
        Ok((file_name, bytes)) => {
            record_backup_http_audit(state, identity, "config_export", None, "success");
            Response::from_data("application/zip", bytes)
                .with_additional_header("Cache-Control", "no-store")
                .with_additional_header(
                    "Content-Disposition",
                    format!("attachment; filename=\"{file_name}\""),
                )
        }
        Err(error) => {
            record_backup_http_audit(state, identity, "config_export", None, "failed");
            backup_error_from_anyhow(error)
        }
    }
}

fn config_apply(
    request: &Request,
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
) -> Response {
    if request
        .header("Content-Type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        != Some("application/zip")
    {
        return backup_rejected_response(
            state,
            identity,
            "config_apply",
            415,
            "configuration apply requires application/zip",
        );
    }
    let Some(content_length) = request
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return backup_rejected_response(
            state,
            identity,
            "config_apply",
            411,
            "configuration apply requires Content-Length",
        );
    };
    let Some(body) = request.data() else {
        return backup_rejected_response(
            state,
            identity,
            "config_apply",
            400,
            "configuration archive body is required",
        );
    };
    backup_json_result(state, identity, "config_apply", None, 202, || {
        backup_manager(state)?.apply_configuration(body, content_length, unix_time_ms())
    })
}

fn config_api_preflight(request: &Request, state: &ServerState) -> Response {
    let response = api_preflight(request, state);
    response.with_additional_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
}

fn backup_manager(state: &ServerState) -> anyhow::Result<&BackupManager> {
    state
        .backup_manager
        .as_deref()
        .ok_or_else(|| crate::backup::ServiceError::busy("backup service is unavailable").into())
}

fn backup_rejected_response(
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
    action: &str,
    status: u16,
    message: &str,
) -> Response {
    record_backup_http_audit(state, identity, action, None, "failed");
    backup_error_response(status, message)
}

fn backup_json_result<T: Serialize>(
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
    action: &str,
    target_id: Option<&str>,
    success_status: u16,
    operation: impl FnOnce() -> anyhow::Result<T>,
) -> Response {
    match operation() {
        Ok(value) => {
            record_backup_http_audit(state, identity, action, target_id, "success");
            Response::json(&value)
                .with_status_code(success_status)
                .with_additional_header("Cache-Control", "no-store")
        }
        Err(error) => {
            record_backup_http_audit(state, identity, action, target_id, "failed");
            backup_error_from_anyhow(error)
        }
    }
}

fn record_backup_http_audit(
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
    action: &str,
    target_id: Option<&str>,
    result: &str,
) {
    if let Some(manager) = &state.backup_manager {
        manager.record_http_result(result == "success");
    }
    record_access_audit(
        state,
        i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
        Some(&identity.principal.id()),
        Some(identity.principal.role),
        action,
        target_id,
        result,
        identity.classification.reason,
    );
}

fn backup_error_from_anyhow(error: anyhow::Error) -> Response {
    if let Some(error) = error.downcast_ref::<crate::backup::ServiceError>() {
        let (status, body) = error.response();
        return Response::json(&body)
            .with_status_code(status)
            .with_additional_header("Cache-Control", "no-store");
    }
    tracing::warn!("backup operation failed without a public error classification");
    Response::json(&crate::api::backup_proto::BackupError {
        code: crate::api::backup_proto::BackupErrorCode::Internal as i32,
        message: "backup operation failed".to_owned(),
        field: String::new(),
        retryable: false,
    })
    .with_status_code(500)
    .with_additional_header("Cache-Control", "no-store")
}

fn backup_error_response(status: u16, message: &str) -> Response {
    let code = match status {
        404 => crate::api::backup_proto::BackupErrorCode::NotFound,
        409 => crate::api::backup_proto::BackupErrorCode::Conflict,
        410 => crate::api::backup_proto::BackupErrorCode::Expired,
        413 => crate::api::backup_proto::BackupErrorCode::Capacity,
        503 => crate::api::backup_proto::BackupErrorCode::Busy,
        _ => crate::api::backup_proto::BackupErrorCode::InvalidRequest,
    };
    Response::json(&crate::api::backup_proto::BackupError {
        code: code as i32,
        message: message.chars().take(512).collect(),
        field: String::new(),
        retryable: matches!(status, 409 | 503),
    })
    .with_status_code(status)
    .with_additional_header("Cache-Control", "no-store")
}

fn authenticated_api_request(
    request: &Request,
    state: &ServerState,
    cors: bool,
    required_role: AccessRole,
    action: impl FnOnce(AuthenticatedApiRequest) -> Response,
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
        Ok(identity) if identity.principal.role.permits(required_role) => action(identity),
        Ok(identity) => {
            state
                .access_metrics
                .authorization_denials
                .fetch_add(1, Ordering::Relaxed);
            record_access_audit(
                state,
                i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
                Some(&identity.principal.id()),
                Some(identity.principal.role),
                "http_denied",
                Some(&request.url()),
                "insufficient_role",
                identity.classification.reason,
            );
            api_status(403, "Administrator role is required for this operation")
        }
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
            "Authorization, Content-Type, Content-Encoding, Prefer",
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

#[derive(Clone)]
struct AuthenticatedApiRequest {
    principal: ApiPrincipal,
    classification: ClientClassification,
}

fn api_principal(
    request: &Request,
    state: &ServerState,
) -> Result<AuthenticatedApiRequest, Response> {
    let classification = state
        .network_access
        .classify(request.remote_addr().ip(), request.headers());
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    if request_has_credential_query(request) {
        state
            .access_metrics
            .authentication_failures
            .fetch_add(1, Ordering::Relaxed);
        record_access_audit(
            state,
            now_ms,
            None,
            None,
            "authentication_failure",
            Some(&request.url()),
            "credential_in_query",
            classification.reason,
        );
        return Err(api_status(
            400,
            "credentials are not accepted in query parameters",
        ));
    }
    if classification.local {
        return Ok(AuthenticatedApiRequest {
            principal: ApiPrincipal::local(classification.effective_address),
            classification,
        });
    }
    let trusted_proxy = matches!(
        classification.reason,
        ClientClassificationReason::TrustedProxyLocal
            | ClientClassificationReason::TrustedProxyRemote
    );
    if state.require_secure_remote && !request.is_secure() && !trusted_proxy {
        state
            .access_metrics
            .authentication_failures
            .fetch_add(1, Ordering::Relaxed);
        record_access_audit(
            state,
            now_ms,
            None,
            None,
            "authentication_failure",
            Some(&request.url()),
            "insecure_remote_transport",
            classification.reason,
        );
        return Err(api_status(
            426,
            "remote access requires HTTPS or a configured trusted proxy",
        ));
    }
    let authorizations = request
        .headers()
        .filter_map(|(name, value)| name.eq_ignore_ascii_case("Authorization").then_some(value))
        .collect::<Vec<_>>();
    let authenticated = state.access_manager.authenticate(
        classification.effective_address,
        &authorizations,
        now_ms,
        Instant::now(),
    );
    let credential = match authenticated {
        Ok(credential) => credential,
        Err(failure) => {
            state
                .access_metrics
                .authentication_failures
                .fetch_add(1, Ordering::Relaxed);
            record_access_audit(
                state,
                now_ms,
                None,
                None,
                "authentication_failure",
                Some(&request.url()),
                failure.as_str(),
                classification.reason,
            );
            let response = if failure == AuthenticationFailure::RateLimited {
                api_status(429, "remote authentication is temporarily rate limited")
                    .with_additional_header("Retry-After", "60")
            } else {
                api_status(401, "Bearer access key is required or invalid")
            };
            return Err(response);
        }
    };
    state
        .access_metrics
        .authentication_successes
        .fetch_add(1, Ordering::Relaxed);
    let principal = ApiPrincipal::credential(credential);
    if request.url() == "/create" {
        record_access_audit(
            state,
            now_ms,
            Some(&principal.id()),
            Some(principal.role),
            "remote_login",
            None,
            "success",
            classification.reason,
        );
    }
    Ok(AuthenticatedApiRequest {
        principal,
        classification,
    })
}

fn request_has_credential_query(request: &Request) -> bool {
    url::form_urlencoded::parse(request.raw_query_string().as_bytes()).any(|(name, _)| {
        ["access_key", "access_token", "authorization", "token"]
            .iter()
            .any(|credential_name| name.eq_ignore_ascii_case(credential_name))
    })
}

fn filter_unusable_ipv4_ice_candidates(sdp: &str) -> (String, usize) {
    let mut filtered = String::with_capacity(sdp.len());
    let mut removed = 0usize;
    for line in sdp.split_inclusive('\n') {
        let content = line.trim_end_matches(&['\r', '\n'][..]);
        let unusable = content
            .strip_prefix("a=candidate:")
            .and_then(|candidate| candidate.split_ascii_whitespace().nth(4))
            .and_then(|address| address.parse::<IpAddr>().ok())
            .is_some_and(|address| match address {
                IpAddr::V4(address) => {
                    address.is_link_local()
                        || address.is_broadcast()
                        || address.is_multicast()
                        || address.is_unspecified()
                }
                IpAddr::V6(_) => false,
            });
        if unusable {
            removed = removed.saturating_add(1);
        } else {
            filtered.push_str(line);
        }
    }
    (filtered, removed)
}

fn create_api_session(
    request: &Request,
    state: &ServerState,
    identity: AuthenticatedApiRequest,
) -> Response {
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
    let (sdp, ignored_ice_candidates) = filter_unusable_ipv4_ice_candidates(&create.offer.sdp);
    if ignored_ice_candidates > 0 {
        tracing::debug!(
            ignored_ice_candidates,
            "ignored unusable IPv4 ICE candidates in SDP offer"
        );
    }
    let offer = match str0m::change::SdpOffer::from_sdp_string(&sdp) {
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
    let principal_sessions = owners
        .values()
        .filter(|owner| owner.principal.identity == identity.principal.identity)
        .count();
    let address_sessions = owners
        .values()
        .filter(|owner| {
            owner.classification.effective_address == identity.classification.effective_address
        })
        .count();
    if principal_sessions >= state.api_session_policy.max_per_principal
        || address_sessions >= state.api_session_policy.max_per_address
    {
        drop(owners);
        state.webrtc.close_api_session(session.id);
        record_access_audit(
            state,
            i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
            Some(&identity.principal.id()),
            Some(identity.principal.role),
            "session_create",
            None,
            "session_limit",
            identity.classification.reason,
        );
        return api_status(429, "API session limit reached");
    }
    let now_at = Instant::now();
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let absolute_timeout_ms =
        i64::try_from(state.api_session_policy.absolute_timeout.as_millis()).unwrap_or(i64::MAX);
    let absolute_expires_at_ms = now_ms.saturating_add(absolute_timeout_ms).min(
        identity
            .principal
            .credential_expires_at_ms
            .unwrap_or(i64::MAX),
    );
    owners.insert(
        session.id,
        ApiSessionRecord {
            principal: identity.principal.clone(),
            classification: identity.classification,
            created_at_ms: now_ms,
            last_activity_at_ms: now_ms,
            absolute_expires_at_ms,
            last_activity: now_at,
        },
    );
    drop(owners);
    state
        .access_metrics
        .sessions_created
        .fetch_add(1, Ordering::Relaxed);
    record_access_audit(
        state,
        now_ms,
        Some(&identity.principal.id()),
        Some(identity.principal.role),
        "session_create",
        Some(&session.id.to_string()),
        "success",
        identity.classification.reason,
    );
    Response::from_data("application/json", compressed)
        .with_status_code(201)
        .with_additional_header("Content-Encoding", "gzip")
}

fn delete_api_session(
    request: &Request,
    state: &ServerState,
    identity: AuthenticatedApiRequest,
) -> Response {
    let Some(body) = request.data() else {
        return api_status(400, "missing delete request body");
    };
    let mut encoded = Vec::new();
    if body
        .take(MAX_DELETE_BODY_BYTES + 1)
        .read_to_end(&mut encoded)
        .is_err()
    {
        return api_status(400, "unable to read delete request body");
    }
    if encoded.len() as u64 > MAX_DELETE_BODY_BYTES {
        return api_status(413, "delete request body exceeds 16 KiB");
    }
    let delete: DeleteRequest = match serde_json::from_slice(&encoded) {
        Ok(delete) => delete,
        Err(error) => return api_status(400, &format!("invalid delete request JSON: {error}")),
    };
    let return_representation = request
        .header("Prefer")
        .is_some_and(|value| value.eq_ignore_ascii_case("return=representation"));
    let Ok(session_id) = delete.session_id.parse::<u64>() else {
        return deleted_session_response(return_representation);
    };
    let session_id = SessionId::from_u64(session_id);
    let owned_session = {
        let mut owners = state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match owners.get(&session_id) {
            None => {
                return if return_representation {
                    Response::text("deleted").with_status_code(200)
                } else {
                    Response::empty_204()
                };
            }
            Some(owner)
                if owner.principal != identity.principal
                    || owner.classification.effective_address
                        != identity.classification.effective_address =>
            {
                None
            }
            Some(_) => owners.remove(&session_id),
        }
    };
    let Some(owned_session) = owned_session else {
        record_access_audit(
            state,
            i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
            Some(&identity.principal.id()),
            Some(identity.principal.role),
            "session_delete",
            Some(&session_id.to_string()),
            "not_owner",
            identity.classification.reason,
        );
        return api_status(404, "WebRTC session not found");
    };
    record_access_audit(
        state,
        i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
        Some(&owned_session.principal.id()),
        Some(owned_session.principal.role),
        "session_delete",
        Some(&session_id.to_string()),
        "success",
        owned_session.classification.reason,
    );
    if return_representation {
        state.webrtc.close_api_session(session_id);
        return Response::text("deleted").with_status_code(200);
    }
    Response {
        status_code: 204,
        headers: Vec::new(),
        data: ResponseBody::from_reader_and_size(
            CloseApiSessionAfterResponse {
                webrtc: state.webrtc.clone(),
                session_id,
            },
            0,
        ),
        upgrade: None,
    }
}

fn deleted_session_response(return_representation: bool) -> Response {
    if return_representation {
        Response::text("deleted").with_status_code(200)
    } else {
        Response::empty_204()
    }
}

struct CloseApiSessionAfterResponse {
    webrtc: WebRtc,
    session_id: SessionId,
}

impl Read for CloseApiSessionAfterResponse {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

impl Drop for CloseApiSessionAfterResponse {
    fn drop(&mut self) {
        self.webrtc.close_api_session(self.session_id);
    }
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

fn log_stream(
    request: &Request,
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
) -> Response {
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
    let mut stream = match logging.stream(after, tail) {
        Ok(stream) => stream,
        Err(LogStreamError::LimitReached) => {
            return service_error(429, "too many active log streams");
        }
        Err(LogStreamError::Closed) => {
            return service_error(503, "log streaming is shutting down");
        }
    };
    if let Some((credential_id, credential_revision)) = identity.principal.credential_binding() {
        let cancelled = Arc::new(AtomicBool::new(false));
        state
            .http_stream_cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(HttpStreamCancellation {
                credential_id,
                credential_revision,
                cancelled: Arc::downgrade(&cancelled),
            });
        stream = stream.with_cancellation(cancelled);
    }
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
            session_duration_ms: stream.report.session_duration_ms,
            recorded_duration_ms: writer_health.map_or(0, |health| health.recorded_duration_ms),
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
    let recorded_duration_ms = |stream_id: &str| {
        recording_states
            .iter()
            .find(|(candidate, _, _)| candidate == stream_id)
            .and_then(|(_, _, health)| *health)
            .map_or(0, |health| health.recorded_duration_ms)
    };
    let recorded_main_duration_ms = recorded_duration_ms("main");
    let recorded_sub_duration_ms = recorded_duration_ms("sub");
    let recorded_total_duration_ms =
        recording_states
            .iter()
            .fold(0_u64, |total, (_, _, health)| {
                total.saturating_add(health.map_or(0, |health| health.recorded_duration_ms))
            });
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
            session_duration_ms: video_streams
                .iter()
                .map(|stream| stream.ingress.report.session_duration_ms)
                .max(),
            recorded_main_duration_ms,
            recorded_sub_duration_ms,
            recorded_total_duration_ms,
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

pub(crate) fn camera_health_snapshots(
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> anyhow::Result<Vec<CameraHealth>> {
    let lifecycle = match query_router(router_tx, RouterQuery::ListCameras)
        .map_err(|_| anyhow::anyhow!("camera lifecycle router is unavailable"))?
    {
        RouterResponse::Cameras(statuses) => statuses
            .into_iter()
            .map(|status| (status.id.to_string(), status))
            .collect::<HashMap<_, _>>(),
        RouterResponse::Camera(_) => anyhow::bail!("router returned an unexpected camera response"),
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
    let uptime_seconds = state.started_at.elapsed().as_secs();
    Ok(state
        .camera_entries()
        .into_iter()
        .map(|camera| {
            let info = state.camera_info(&camera);
            let streams = camera
                .info
                .ip
                .parse::<IpAddr>()
                .ok()
                .and_then(|ip| ingress.remove(&ip))
                .map_or_else(Vec::new, |report| report.streams);
            let context = CameraProjectionContext {
                router_status: lifecycle.get(&camera.recording_label),
                recording_health: &recording_health,
                battery_wake: state.battery_wake.as_ref(),
                storage_config: &state.storage_config,
                uptime_seconds,
            };
            project_camera_snapshot(&camera, info, streams, &context)
        })
        .collect())
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
    let operational_events = state.events.as_ref().map_or_else(Vec::new, |events| {
        match events.open_operational_events() {
            Ok(events) => events,
            Err(error) => {
                issues.push(HealthIssue {
                    severity: "warning".to_owned(),
                    scope: "storage".to_owned(),
                    message: format!("Operational event history is unavailable: {error}"),
                    operational_event_id: None,
                    timeline_start_ms: None,
                    timeline_end_ms: None,
                });
                Vec::new()
            }
        }
    });
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
                operational_event_id: None,
                timeline_start_ms: None,
                timeline_end_ms: None,
            });
            HashMap::new()
        }
        Err(_) => {
            issues.push(HealthIssue {
                severity: "warning".to_owned(),
                scope: "runtime".to_owned(),
                message: "Camera lifecycle router did not answer the health query".to_owned(),
                operational_event_id: None,
                timeline_start_ms: None,
                timeline_end_ms: None,
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
    let recording_snapshot = state.recording_health.snapshot();
    let storage_safety = recording_snapshot.storage;
    let recording_health = recording_snapshot
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
        health_snapshot::aggregate_video_streams(camera, &streams, &mut totals, &mut issues);

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
            let related = operational_events
                .iter()
                .filter(|event| event.key.camera_id == camera_health.id)
                .collect::<Vec<_>>();
            if related.is_empty() {
                issues.push(HealthIssue {
                    severity: "warning".to_owned(),
                    scope: camera_health.id.clone(),
                    message: camera_health.detail.clone(),
                    operational_event_id: None,
                    timeline_start_ms: None,
                    timeline_end_ms: None,
                });
            } else {
                issues.extend(related.into_iter().map(|event| HealthIssue {
                    severity: event.severity.as_str().to_owned(),
                    scope: camera_health.id.clone(),
                    message: event.evidence.explanation.clone(),
                    operational_event_id: Some(event.id.clone()),
                    timeline_start_ms: Some(event.start_time_ms),
                    timeline_end_ms: event.end_time_ms,
                }));
            }
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
                    operational_event_id: None,
                    timeline_start_ms: None,
                    timeline_end_ms: None,
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
        minimum_free_bytes: state.storage_config.minimum_free_bytes,
        maximum_used_percent: state.storage_config.maximum_used_percent,
        warning_free_bytes: state.storage_config.warning_free_bytes,
        critical_free_bytes: state.storage_config.critical_free_bytes,
        cleanup_hysteresis_bytes: state.storage_config.cleanup_hysteresis_bytes,
        catalog_bytes,
        catalog,
        safety: storage_safety,
        demand: state.recording_demand.health_snapshot(),
    };
    if storage.safety.recording_state.as_str() == "paused" {
        issues.push(HealthIssue {
            severity: "critical".to_owned(),
            scope: "storage".to_owned(),
            message: storage.safety.last_failure.as_ref().map_or_else(
                || {
                    "Recording is paused because storage cleanup could not restore headroom"
                        .to_owned()
                },
                |failure| format!("Recording is paused: {failure}"),
            ),
            operational_event_id: None,
            timeline_start_ms: None,
            timeline_end_ms: None,
        });
    } else if matches!(storage.safety.pressure.as_str(), "warning" | "critical") {
        issues.push(HealthIssue {
            severity: storage.safety.pressure.as_str().to_owned(),
            scope: "storage".to_owned(),
            message: format!(
                "Recording storage has {} bytes available; cleanup recovery target is {} bytes",
                storage.safety.available_bytes.unwrap_or(0),
                storage.safety.recovery_free_bytes,
            ),
            operational_event_id: None,
            timeline_start_ms: None,
            timeline_end_ms: None,
        });
    }
    if system.memory.total_bytes > 0
        && system.memory.available_bytes.saturating_mul(100) / system.memory.total_bytes < 5
    {
        issues.push(HealthIssue {
            severity: "critical".to_owned(),
            scope: "system".to_owned(),
            message: "System memory has less than 5% available".to_owned(),
            operational_event_id: None,
            timeline_start_ms: None,
            timeline_end_ms: None,
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
            operational_event_id: None,
            timeline_start_ms: None,
            timeline_end_ms: None,
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
        operational_events,
    }
}

fn prometheus_metrics(router_tx: &FacadeSender<RouterMessage>, state: &ServerState) -> Response {
    let health = server_health(router_tx, state);
    let recording =
        recording_coverage::metric_snapshot(state, &health, unix_time_ms()).map_err(|error| {
            tracing::warn!(%error, "recording coverage metrics are unavailable");
        });
    let mqtt = state
        .event_forwarder
        .as_ref()
        .map(EventForwarderHandle::status);
    let external_analysis = external_analysis_metrics_snapshot(state, &health);
    let backup = state.backup_manager.as_ref().and_then(|manager| {
        manager
            .metric_snapshot(unix_time_ms())
            .map_err(|error| tracing::warn!(%error, "backup metrics are unavailable"))
            .ok()
    });
    let notifications = state
        .notifications
        .as_ref()
        .map(NotificationHandle::metric_snapshot);
    match crate::metrics::encode_health_metrics(
        &health,
        Some(access_metrics_snapshot(state)),
        recording.as_ref().ok(),
        backup,
        notifications,
        mqtt.as_ref(),
        Some(external_analysis),
    ) {
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

fn external_analysis_metrics_snapshot(
    state: &ServerState,
    health: &ServerHealthResponse,
) -> crate::metrics::ExternalAnalysisMetricsSnapshot {
    let publications = state.event_publications.metrics_snapshot();
    let subscriptions = state.event_subscriptions.metrics_snapshot();
    let queue = state.webrtc.api_event_queue_metrics_snapshot();
    crate::metrics::ExternalAnalysisMetricsSnapshot {
        sessions_active: queue.sessions_active,
        media_subscriptions_active: health.webrtc.multi_tracks as u64,
        event_subscriptions_active: subscriptions.active,
        event_subscription_starts: subscriptions.starts,
        event_subscription_rejections: subscriptions.rejections,
        event_subscription_deliveries: subscriptions.deliveries,
        event_subscription_sheds: subscriptions.sheds,
        event_delivery_queue_depth: queue.queue_depth,
        event_delivery_queue_depth_high_water: queue.queue_depth_high_water,
        event_delivery_pending_bytes: queue.pending_bytes,
        event_delivery_pending_bytes_high_water: queue.pending_bytes_high_water,
        event_deliveries_queued: queue.deliveries_queued,
        event_delivery_drops: queue.delivery_drops,
        event_publications_active: publications.active,
        event_publication_staged_bytes: publications.staged_bytes,
        event_publication_starts: publications.starts,
        event_publication_commits: publications.commits,
        event_publication_aborts: publications.aborts,
        event_publication_expirations: publications.expirations,
        event_publication_rejections: publications.rejections,
        event_publication_storage_failures: publications.storage_failures,
        event_publication_commit_latency_ms_p50: publications.commit_latency_ms_p50,
        event_publication_commit_latency_ms_p95: publications.commit_latency_ms_p95,
    }
}

fn access_metrics_snapshot(state: &ServerState) -> crate::metrics::AccessMetricsSnapshot {
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let active_credentials = state
        .access_manager
        .list_credentials()
        .into_iter()
        .filter(|credential| {
            !credential.disabled
                && credential.revoked_at_ms.is_none()
                && credential
                    .expires_at_ms
                    .is_none_or(|expires_at| expires_at > now_ms)
        })
        .count() as u64;
    crate::metrics::AccessMetricsSnapshot {
        authentication_successes: state
            .access_metrics
            .authentication_successes
            .load(Ordering::Relaxed),
        authentication_failures: state
            .access_metrics
            .authentication_failures
            .load(Ordering::Relaxed),
        authorization_denials: state
            .access_metrics
            .authorization_denials
            .load(Ordering::Relaxed),
        sessions_created: state
            .access_metrics
            .sessions_created
            .load(Ordering::Relaxed),
        sessions_revoked_or_expired: state
            .access_metrics
            .sessions_revoked_or_expired
            .load(Ordering::Relaxed),
        active_sessions: state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len() as u64,
        active_credentials,
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
    subnets: Vec<u8>,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
    task: Option<&camera_discovery::TaskHandle>,
) -> Result<Vec<DiscoveredCameraSettings>, ControlCommandError> {
    let discovered = match if networks.is_empty() {
        crate::cameras::discover(Some(Duration::from_secs(5)), &subnets)
    } else {
        let cancelled = task
            .map(camera_discovery::TaskHandle::cancellation_token)
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        crate::cameras::discover_on_networks_with_progress(
            Some(Duration::from_secs(5)),
            &networks,
            &cancelled,
            |cameras| {
                if let Some(task) = task {
                    task.update(cameras);
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
    for (value, name) in [
        (update.storage.minimum_free_gb, "minimum free space"),
        (update.storage.warning_free_gb, "warning free space"),
        (update.storage.critical_free_gb, "critical free space"),
        (update.storage.cleanup_hysteresis_gb, "cleanup hysteresis"),
    ] {
        if value > u64::MAX / GIBIBYTE_BYTES {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("{name} is too large"),
            ));
        }
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
            minimum_free_gb: update.storage.minimum_free_gb,
            maximum_used_percent: update.storage.maximum_used_percent,
            warning_free_gb: update.storage.warning_free_gb,
            critical_free_gb: update.storage.critical_free_gb,
            cleanup_hysteresis_gb: update.storage.cleanup_hysteresis_gb,
        },
        ..Config::default()
    };
    if let Err(error) = settings.storage.validate_safety_thresholds() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("invalid storage safety thresholds: {error}"),
        ));
    }
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
    let mut probe_paths = HashSet::new();
    probe_paths.insert(next_storage_config.medium_term_path.clone());
    probe_paths.insert(next_storage_config.long_term_path.clone());
    probe_paths.insert(next_storage_config.event_thumbnail_path.clone());
    probe_paths.insert(
        next_storage_config
            .recording_catalog_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    );
    for path in probe_paths {
        if let Err(error) = storage_write_probe(&path) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("storage path is not writable: {error}"),
            ));
        }
    }
    let catalog_recording_bytes = state
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.stats().ok())
        .map_or(0, |stats| stats.recording_bytes);
    let capacity = filesystem_capacity(
        &next_storage_config.long_term_path,
        if next_storage_config.long_term_path == state.storage_config.long_term_path
            || update.move_existing_recordings
        {
            catalog_recording_bytes
        } else {
            0
        },
    )
    .map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("storage filesystem is unavailable: {error}"),
        )
    })?;
    let safety = next_storage_config.safety_policy().evaluate(capacity);
    if safety.critical_free_bytes > capacity.total_bytes
        || safety.warning_free_bytes > capacity.total_bytes
        || safety.recovery_free_bytes > capacity.total_bytes
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "storage headroom thresholds exceed destination filesystem capacity",
        ));
    }
    let reclaimable_bytes = if next_storage_config.long_term_path
        == state.storage_config.long_term_path
        || update.move_existing_recordings
    {
        catalog_recording_bytes
    } else {
        0
    };
    if capacity.available_bytes.saturating_add(reclaimable_bytes) < safety.recovery_free_bytes {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "destination filesystem cannot provide the configured cleanup recovery headroom",
        ));
    }
    if update.move_existing_recordings
        && next_storage_config.long_term_path != state.storage_config.long_term_path
        && capacity.available_bytes
            < catalog_recording_bytes.saturating_add(safety.critical_free_bytes)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "destination filesystem cannot hold indexed recordings and critical headroom",
        ));
    }
    let _config_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !update.expected_configuration_revision.is_empty() {
        let current = config::load_config(config_path).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to verify current settings revision: {error}"),
            )
        })?;
        if configuration_revision(&current) != update.expected_configuration_revision {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "runtime configuration changed after this editor was opened; reload before applying the draft",
            ));
        }
    }
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
    let _config_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let current_revision = camera_configuration_revision(state)?;
    if !update.expected_configuration_revision.is_empty()
        && update.expected_configuration_revision != current_revision
    {
        return Err(configuration::revision_conflict(
            &current_revision,
            "camera configuration changed after this editor was opened; reload current values before retrying",
        ));
    }
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
    let existing_config =
        persisted.or_else(|| existing.as_ref().map(|camera| camera.configuration.clone()));
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
    let started_config = start_runtime_camera(state, &config, !is_new_camera, true);
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
        configuration_revision: camera_configuration_revision(state)?,
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
    persist_prepared: bool,
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
    if persist_prepared && let Err(error) = config::upsert_camera(config_path, &camera.config) {
        tracing::warn!(ip = %config.ip, %error, "discovered camera endpoints could not be persisted");
        return None;
    }
    let groups = runtime_camera_groups(config_path, camera.config.ip)?;
    let runtime_result = if restart {
        runtime.restart_camera(camera.clone())
    } else {
        runtime.start_camera(camera.clone())
    };
    if let Err(error) = runtime_result {
        tracing::warn!(ip = %config.ip, %error, "camera configuration could not be applied live");
        return None;
    }
    let mut entry = camera_entry(&camera.config, Some(&camera));
    entry.groups = groups;
    state.upsert_camera(entry);
    Some(camera.config)
}

fn runtime_camera_groups(config_path: &Path, camera_ip: IpAddr) -> Option<Vec<String>> {
    let configured = match config::load_cameras(config_path) {
        Ok(configured) => configured,
        Err(_) => {
            tracing::warn!("camera group membership could not be loaded for a live start");
            return None;
        }
    };
    let mut groups = configured
        .into_iter()
        .filter_map(|(group, cameras)| {
            cameras
                .iter()
                .any(|camera| camera.ip == camera_ip)
                .then_some(group)
        })
        .collect::<Vec<_>>();
    if groups.is_empty() {
        tracing::warn!("camera configuration has no group membership for a live start");
        return None;
    }
    groups.sort_unstable();
    Some(groups)
}

fn delete_camera_settings(
    state: &ServerState,
    camera_id: &str,
    expected_configuration_revision: &str,
) -> Result<String, ControlCommandError> {
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
    let current_revision = camera_configuration_revision(state)?;
    if !expected_configuration_revision.is_empty()
        && expected_configuration_revision != current_revision
    {
        return Err(configuration::revision_conflict(
            &current_revision,
            "camera configuration changed before removal; reload current values before retrying",
        ));
    }
    match config::remove_camera(config_path, ip) {
        Ok(()) => camera_configuration_revision(state),
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
    use crate::{
        api::proto::{health_command, stored_media_command},
        health::CameraHealthReason,
    };

    fn fixture_video_keyframe(name: &str, media_type: mp4::MediaType) -> bytes::Bytes {
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
        let sample = (1..=track.sample_count())
            .find_map(|sample_id| {
                let sample = reader.read_sample(track_id, sample_id).unwrap().unwrap();
                sample.is_sync.then_some(sample)
            })
            .unwrap();
        let mut data = Vec::new();
        let mut append = |nal: &[u8]| {
            data.extend_from_slice(&u32::try_from(nal.len()).unwrap().to_be_bytes());
            data.extend_from_slice(nal);
        };
        match config {
            mp4::MediaConfig::AvcConfig(config) => {
                append(&config.seq_param_set);
                append(&config.pic_param_set);
            }
            mp4::MediaConfig::HevcConfig(config) => {
                append(&config.vps);
                append(&config.sps);
                append(&config.pps);
            }
            _ => panic!("fixture must use H.264 or H.265"),
        }
        data.extend_from_slice(&sample.bytes);
        bytes::Bytes::from(data)
    }

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

    fn assert_independent_player_accepts(path: &Path) {
        if std::env::var_os("KEEPPEEK_VALIDATE_EXPORT_MEDIA").is_none() {
            return;
        }
        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .expect("ffprobe must be installed for media validation");
        assert!(
            probe.status.success(),
            "ffprobe rejected export: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
        let probe: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let video = &probe["streams"][0];
        assert_eq!(video["codec_name"], "h264");
        assert!(video["width"].as_u64().is_some_and(|value| value > 0));
        assert!(video["height"].as_u64().is_some_and(|value| value > 0));

        let decode = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-map", "0:v:0", "-f", "null", "-"])
            .output()
            .expect("ffmpeg must be installed for media validation");
        assert!(
            decode.status.success() && decode.stderr.is_empty(),
            "ffmpeg rejected export: {}",
            String::from_utf8_lossy(&decode.stderr)
        );
    }

    fn test_control_handler(state: ServerState) -> ServerControlHandler {
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        ServerControlHandler::new(state, router_tx)
    }

    fn secured_test_state() -> ServerState {
        let mut state = ServerState::empty();
        let access_key = AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
        state.access_key = Arc::new(RwLock::new(access_key));
        state.access_manager = AccessManager::ephemeral(access_key);
        state.require_secure_remote = false;
        state.allowed_origins = Arc::new(HashSet::from(["https://home.example.net".to_owned()]));
        state
    }

    fn test_session_record(
        principal: ApiPrincipal,
        address: IpAddr,
        reason: ClientClassificationReason,
    ) -> ApiSessionRecord {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        ApiSessionRecord {
            principal,
            classification: ClientClassification {
                peer_address: address,
                effective_address: address,
                local: matches!(
                    reason,
                    ClientClassificationReason::DirectLocal
                        | ClientClassificationReason::TrustedProxyLocal
                ),
                reason,
            },
            created_at_ms: now_ms,
            last_activity_at_ms: now_ms,
            absolute_expires_at_ms: now_ms.saturating_add(60_000),
            last_activity: Instant::now(),
        }
    }

    fn local_test_session() -> ApiSessionRecord {
        test_session_record(
            ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            ClientClassificationReason::DirectLocal,
        )
    }

    fn restricted_test_user(state: &ServerState) -> IssuedCredential {
        let mut issued = state
            .access_manager
            .create_credential("Restricted viewer", None, AccessRole::User, None, 1_000)
            .unwrap();
        issued.metadata = state
            .access_manager
            .set_camera_access(
                issued.metadata.id,
                issued.metadata.revision,
                crate::access::CameraAccess::default(),
            )
            .unwrap();
        issued
    }

    fn bind_credential_test_session(state: &ServerState, session_id: SessionId, key: AccessKey) {
        let address = "203.0.113.7".parse().unwrap();
        let authorization = format!("Bearer {}", key.canonical());
        let authenticated = state
            .access_manager
            .authenticate(address, &[&authorization], 2_000, Instant::now())
            .unwrap();
        state.api_session_owners.lock().unwrap().insert(
            session_id,
            test_session_record(
                ApiPrincipal::credential(authenticated),
                address,
                ClientClassificationReason::DirectRemote,
            ),
        );
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
            operational_event_id: None,
            timeline_start_ms: None,
            timeline_end_ms: None,
        }];

        assert_eq!(camera.state, CameraHealthState::Healthy);
        assert_eq!(camera.reason, CameraHealthReason::Healthy);
        assert_eq!(server_health_status(&external_findings), "degraded");
    }

    #[test]
    fn health_aggregates_ingress_counters_and_stream_quality_issues() {
        let camera = CameraConfig {
            ip: "192.0.2.41".parse().unwrap(),
            name: Some("side-door".to_owned()),
            display_name: Some("Side Door".to_owned()),
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
            ip: "192.0.2.41".parse().unwrap(),
            name: Some("side-door".to_owned()),
            brand: None,
            port: 554,
            streams: vec![crate::stats::StreamReport {
                kind: "video_main".to_owned(),
                session_duration_ms: 10_000,
                codec: Some("h264".to_owned()),
                resolution: Some("1920x1080".to_owned()),
                fps: 15.0,
                expected_fps: 15.0,
                kf_fps: 1.0,
                kbps: 512.0,
                max_frame_kb: 64.0,
                gap_min_ms: 40.0,
                gap_avg_ms: 66.0,
                gap_max_ms: 2_501.0,
                jitter_samples: 10,
                jitter_p50_ms: 60.0,
                jitter_p99_ms: 600.0,
                frames: Some(100),
                bytes: Some(1_000),
                keyframes: Some(5),
                reconnects: Some(2),
                drops: Some(3),
                errors: Some(4),
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
        let router_worker =
            std::thread::spawn(move || router.wait_and_drain(Some(Duration::from_secs(2))));

        let health = server_health(&router_tx, &state);

        assert_eq!(router_worker.join().unwrap().unwrap(), 1);
        assert_eq!(health.totals.ingress_fps, 15.0);
        assert_eq!(health.totals.ingress_bitrate_bps, 512_000);
        assert_eq!(health.totals.frames, 100);
        assert_eq!(health.totals.keyframes, 5);
        assert_eq!(health.totals.reconnects, 2);
        assert_eq!(health.totals.drops, 3);
        assert_eq!(health.totals.errors, 4);
        assert!(
            health
                .issues
                .iter()
                .all(|issue| !issue.message.contains("frame-arrival jitter"))
        );
        assert!(
            health
                .issues
                .iter()
                .any(|issue| issue.message.contains("maximum frame gap is 2501 ms"))
        );
    }

    #[test]
    fn connected_camera_without_frame_progress_is_starting_during_startup_grace() {
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
            recording_mode: CameraRecordingMode::Both,
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
                session_duration_ms: 10_000,
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
        let recording_health = RecordingHealthRegistry::default();
        recording_health.note_progress("front-door/main", Duration::from_secs(8 * 60));
        recording_health.note_progress("front-door/sub", Duration::from_secs(5 * 60));
        let state = ServerState::new(
            &config,
            &camera_configs,
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        )
        .with_health_registry(registry)
        .with_recording_health(recording_health);
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        router_tx
            .send(RouterMessage::WorkerEvent(
                crate::runtime::WorkerEvent::StatusChanged(CameraStatus {
                    id: crate::api::CameraId::new("front-door"),
                    lifecycle: CameraLifecycle::Connected,
                    expected_streams: vec!["main".to_owned(), "sub".to_owned()],
                    connected_streams: vec!["main".to_owned()],
                    last_error: None,
                }),
            ))
            .unwrap();
        assert_eq!(router.wait_and_drain(Some(Duration::ZERO)).unwrap(), 1);
        let router_worker =
            std::thread::spawn(move || router.wait_and_drain(Some(Duration::from_secs(30))));

        let mut health = server_health(&router_tx, &state);
        assert_eq!(router_worker.join().unwrap().unwrap(), 1);
        assert_eq!(health.cameras[0].state, CameraHealthState::Starting);
        assert_eq!(health.cameras[0].reason, CameraHealthReason::Starting);
        assert_eq!(health.totals.connected_cameras, 1);
        assert_eq!(health.totals.fresh_cameras, 0);
        assert_eq!(health.totals.decodable_cameras, 0);
        assert_eq!(health.totals.connected_video_streams, 1);
        assert_eq!(health.totals.fresh_video_streams, 0);
        assert_eq!(health.totals.decodable_video_streams, 0);
        assert_eq!(
            health.cameras[0].dimensions.session_duration_ms,
            Some(10_000)
        );
        assert_eq!(
            health.cameras[0].dimensions.recorded_main_duration_ms,
            480_000
        );
        assert_eq!(
            health.cameras[0].dimensions.recorded_sub_duration_ms,
            300_000
        );
        assert_eq!(
            health.cameras[0].dimensions.recorded_total_duration_ms,
            780_000
        );

        health
            .operational_events
            .push(crate::operational_events::OperationalEvent {
                id: "operational-1".to_owned(),
                key: crate::operational_events::OperationalEventKey {
                    camera_id: "front-door".to_owned(),
                    stream_id: Some("main".to_owned()),
                    kind: crate::operational_events::OperationalEventKind::RecordingInterrupted,
                },
                evidence: crate::operational_events::OperationalEvidence {
                    cause: "recording_not_progressing".to_owned(),
                    explanation: "Requested recording writes are not progressing".to_owned(),
                    affected_streams: vec!["main".to_owned()],
                    recording_interrupted: true,
                    source: "recording_writer".to_owned(),
                },
                severity: crate::operational_events::OperationalSeverity::Critical,
                revision: 2,
                start_time_ms: 1_000,
                end_time_ms: None,
                duration_ms: Some(60_000),
            });
        health.webrtc.multi_track_sessions = 1;
        health.webrtc.multi_tracks = 3;

        let mqtt = crate::event_forwarder::MqttStatus {
            enabled: true,
            state: crate::event_forwarder::MqttConnectionState::Connected,
            detail: "MQTT 5 broker is connected.".to_owned(),
            connected_at_ms: Some(1_786_800_000_000),
            last_received_at_ms: Some(1_786_800_001_000),
            last_delivered_at_ms: Some(1_786_800_002_000),
            pending_items: 2,
            pending_bytes: 2_048,
            oldest_unacknowledged_timestamp_ms: Some(1_786_800_001_000),
            retry_count: 3,
            duplicate_count: 4,
            outbox_limit_bytes: 67_108_864,
        };
        let metrics = crate::metrics::encode_health_metrics(
            &health,
            None,
            None,
            Some(crate::metrics::BackupMetricsSnapshot {
                operation_successes: 7,
                operation_failures: 2,
                retained_backups: 3,
                retained_archive_bytes: 14_595,
                active_restore: 1,
            }),
            Some(crate::metrics::NotificationMetricsSnapshot {
                configured_rules: 2,
                pending_deliveries: 3,
                candidates_accepted: 11,
                candidates_dropped: 1,
                notifications_created: 5,
                notifications_replaced: 2,
                notifications_suppressed: 4,
                delivery_attempts: 7,
                delivery_retries: 2,
                delivery_successes: 4,
                delivery_failures: 1,
            }),
            Some(&mqtt),
            None,
        )
        .unwrap();
        assert!(metrics.contains("state=\"starting\""));
        assert!(!metrics.contains("keeppeek_camera_online"));
        assert!(!metrics.contains("keeppeek_camera_degraded"));
        assert!(metrics.contains("dimension=\"frames_fresh\""));
        assert!(metrics.contains("keeppeek_operational_event_active"));
        assert!(metrics.contains("kind=\"recording_interrupted\""));
        assert!(metrics.contains("severity=\"critical\""));
        assert!(metrics.contains("keeppeek_mqtt_forwarder_connected 1"));
        assert!(metrics.contains("keeppeek_mqtt_forwarder_outbox_items 2"));
        assert!(metrics.contains("keeppeek_mqtt_forwarder_outbox_bytes 2048"));
        assert!(metrics.contains("keeppeek_mqtt_forwarder_retries_total 3"));
        assert!(metrics.contains("keeppeek_mqtt_forwarder_duplicates_total 4"));
        assert!(metrics.contains("keeppeek_backup_operations_successes_total 7"));
        assert!(metrics.contains("keeppeek_backup_operations_failures_total 2"));
        assert!(metrics.contains("keeppeek_backup_artifacts_retained 3"));
        assert!(metrics.contains("keeppeek_backup_artifacts_retained_bytes 14595"));
        assert!(metrics.contains("keeppeek_backup_restore_active 1"));
        assert!(metrics.contains("keeppeek_notification_rules_configured 2"));
        assert!(metrics.contains("keeppeek_notification_deliveries_pending 3"));
        assert!(metrics.contains("keeppeek_notification_candidates_accepted_total 11"));
        assert!(metrics.contains("keeppeek_notification_candidates_dropped_total 1"));
        assert!(metrics.contains("keeppeek_notifications_created_total 5"));
        assert!(metrics.contains("keeppeek_notifications_replaced_total 2"));
        assert!(metrics.contains("keeppeek_notifications_suppressed_total 4"));
        assert!(metrics.contains("keeppeek_notification_delivery_attempts_total 7"));
        assert!(metrics.contains("keeppeek_notification_delivery_retries_total 2"));
        assert!(metrics.contains("keeppeek_notification_delivery_successes_total 4"));
        assert!(metrics.contains("keeppeek_notification_delivery_failures_total 1"));
        assert!(metrics.contains("keeppeek_webrtc_multi_track_sessions 1"));
        assert!(metrics.contains("keeppeek_webrtc_multi_tracks 3"));
        let proto = health_snapshot::proto_health_snapshot(health);
        assert_eq!(
            proto.health_contract_version,
            CAMERA_HEALTH_CONTRACT_VERSION
        );
        assert_eq!(proto.cameras[0].state, "starting");
        assert_eq!(proto.cameras[0].reason, "starting");
        let dimensions = proto.cameras[0].dimensions.as_ref().unwrap();
        assert_eq!(dimensions.session_duration_ms, Some(10_000));
        assert_eq!(dimensions.recorded_main_duration_ms, 480_000);
        assert_eq!(dimensions.recorded_sub_duration_ms, 300_000);
        assert_eq!(dimensions.recorded_total_duration_ms, 780_000);
        assert_eq!(proto.totals.unwrap().fresh_cameras, 0);
        assert_eq!(proto.operational_events.len(), 1);
        let operational = &proto.operational_events[0];
        assert_eq!(operational.event_id, "operational-1");
        assert_eq!(operational.revision, 2);
        assert_eq!(operational.event_type, "recording_interrupted");
        assert_eq!(
            operational.text.as_deref(),
            Some("Requested recording writes are not progressing")
        );
        assert!(matches!(
            operational
                .payload
                .as_ref()
                .and_then(|payload| payload.fields.get("cause"))
                .and_then(|value| value.kind.as_ref()),
            Some(prost_types::value::Kind::StringValue(cause))
                if cause == "recording_not_progressing"
        ));
    }

    fn media_request(
        kind: proto::MediaKind,
        transport: proto::DeliveryTransport,
        quality: proto::VideoQuality,
        variant_id: &str,
    ) -> proto::SubscribeMedia {
        proto::SubscribeMedia {
            subscription_id: "front-door-live".to_owned(),
            source_session_id: camera_source_session_id("127.0.0.1", 0),
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
    fn create_ignores_an_unusable_ipv4_link_local_ice_candidate() {
        let state = ServerState::empty();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let mut offer = crate::webrtc::test_api_offer().to_sdp_string();
        offer.push_str("\r\na=candidate:1234 1 udp 2122260223 169.254.116.209 54321 typ host\r\n");
        let (filtered, removed) = filter_unusable_ipv4_ice_candidates(&offer);
        assert_eq!(removed, 1);
        assert!(!filtered.contains("169.254.116.209"));
        let create = handle_request(
            &Request::fake_http(
                "POST",
                "/create",
                vec![
                    ("Content-Type".to_owned(), "application/json".to_owned()),
                    ("Content-Encoding".to_owned(), "gzip".to_owned()),
                ],
                gzip_json(&CreateRequest {
                    offer: crate::api::SdpOffer {
                        sdp_type: "offer".to_owned(),
                        sdp: offer,
                    },
                }),
            ),
            &router_tx,
            &state,
        );

        let status_code = create.status_code;
        if status_code != 201 {
            let body = String::from_utf8(response_data(create)).unwrap();
            panic!("create returned {status_code}: {body}");
        }
        state.webrtc.shutdown();
    }

    #[test]
    fn delete_rejects_oversized_request_body() {
        let state = ServerState::empty();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let body = format!(r#"{{"session_id":"{}"}}"#, "1".repeat(16 * 1_024)).into_bytes();

        let response = handle_request(
            &Request::fake_http("POST", "/delete", Vec::new(), body),
            &router_tx,
            &state,
        );

        assert_eq!(response.status_code, 413);
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
            SocketAddr::from(([0xfd00, 0, 0, 0, 0, 0, 0, 50], 42_000)),
            SocketAddr::from(([0xfe80, 0, 0, 0, 0, 0, 0, 50], 42_000)),
            SocketAddr::from(([0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0132], 42_000)),
        ] {
            let response = handle_request(
                &Request::fake_http_from(local_network, "GET", "/logs", Vec::new(), Vec::new()),
                &router_tx,
                &state,
            );
            assert_eq!(response.status_code, 503);
        }
    }

    #[test]
    fn remote_transport_is_secure_and_credentials_are_never_read_from_queries() {
        let mut state = secured_test_state();
        state.require_secure_remote = true;
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let remote = SocketAddr::from(([203, 0, 113, 7], 42_000));

        let insecure = handle_request(
            &Request::fake_http_from(
                remote,
                "GET",
                "/logs/snapshot",
                vec![bearer_header()],
                Vec::new(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(insecure.status_code, 426);

        let secure = handle_request(
            &Request::fake_https_from(
                remote,
                "GET",
                "/logs/snapshot",
                vec![bearer_header()],
                Vec::new(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(secure.status_code, 503);

        for query in [
            "access_key=550e8400-e29b-41d4-a716-446655440000",
            "%61ccess_token=550e8400-e29b-41d4-a716-446655440000",
        ] {
            let response = handle_request(
                &Request::fake_http("GET", format!("/metrics?{query}"), Vec::new(), Vec::new()),
                &router_tx,
                &state,
            );
            assert_eq!(response.status_code, 400);
        }
        let credential_query_audit = state
            .access_manager
            .list_audit(10)
            .into_iter()
            .filter(|event| event.result == "credential_in_query")
            .collect::<Vec<_>>();
        assert_eq!(credential_query_audit.len(), 2);
        assert!(
            credential_query_audit
                .iter()
                .all(|event| event.target_id.as_deref() == Some("/metrics"))
        );
        assert!(credential_query_audit.iter().all(|event| {
            !event
                .target_id
                .as_deref()
                .is_some_and(|target| target.contains("550e8400"))
        }));
    }

    #[test]
    fn remote_user_authenticates_but_cannot_run_administrator_operations() {
        let state = secured_test_state();
        let issued = state
            .access_manager
            .create_credential("Viewer", None, AccessRole::User, None, 1_000)
            .unwrap();
        let authorization = format!("Bearer {}", issued.access_key.canonical());
        let remote = SocketAddr::from(([203, 0, 113, 8], 42_000));
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let metrics = handle_request(
            &Request::fake_https_from(
                remote,
                "GET",
                "/metrics",
                vec![("Authorization".to_owned(), authorization.clone())],
                Vec::new(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(metrics.status_code, 403);

        let authenticated = state
            .access_manager
            .authenticate(remote.ip(), &[&authorization], 2_000, Instant::now())
            .unwrap();
        let session_id = SessionId::from_u64(700);
        state.api_session_owners.lock().unwrap().insert(
            session_id,
            test_session_record(
                ApiPrincipal::credential(authenticated),
                remote.ip(),
                ClientClassificationReason::DirectRemote,
            ),
        );
        let handler = test_control_handler(state);
        let identity = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 1,
                command: Some(control_request::Command::ServerCommand(
                    proto::ServerCommand {
                        action: Some(server_command::Action::GetAccessSession(
                            proto::GetAccessSession {},
                        )),
                    },
                )),
            },
        );
        assert!(matches!(
            identity.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::AccessSessionResult(_))
            }))
        ));

        let denied = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 2,
                command: Some(control_request::Command::RuntimeConfigurationCommand(
                    proto::RuntimeConfigurationCommand {
                        action: Some(runtime_configuration_command::Action::Get(
                            proto::GetRuntimeConfiguration {},
                        )),
                    },
                )),
            },
        );
        assert!(matches!(
            denied.response.result,
            Some(control_response::Result::Error(proto::Error { code, .. }))
                if code == proto::ErrorCode::Rejected as i32
        ));
        assert!(
            handler
                .state
                .access_manager
                .list_audit(10)
                .iter()
                .any(|event| {
                    event.action == "command_denied"
                        && event.target_id.as_deref() == Some("runtime_configuration")
                        && event.result == "insufficient_role"
                })
        );
    }

    #[test]
    fn central_access_policy_covers_each_control_family() {
        let user_commands = [
            control_request::Command::SubscribeMedia(proto::SubscribeMedia::default()),
            control_request::Command::SubscribeEvents(proto::SubscribeEvents::default()),
            control_request::Command::SubscribeData(proto::SubscribeData::default()),
            control_request::Command::Unsubscribe(proto::Unsubscribe::default()),
            control_request::Command::StoredMediaCommand(proto::StoredMediaCommand::default()),
            control_request::Command::GroupCommand(proto::GroupCommand::default()),
            control_request::Command::StateStoreCommand(proto::StateStoreCommand::default()),
            control_request::Command::PublicationCommand(proto::PublicationCommand::default()),
            control_request::Command::PublicationReport(proto::PublicationReport::default()),
            control_request::Command::CameraControlCommand(proto::CameraControlCommand {
                action: Some(camera_control_command::Action::GetMotionDetection(
                    proto::GetMotionDetection::default(),
                )),
            }),
            control_request::Command::EventSearchCommand(proto::EventSearchCommand {
                action: Some(event_search_command::Action::Query(
                    proto::QueryEvents::default(),
                )),
            }),
            control_request::Command::NotificationRuleCommand(proto::NotificationRuleCommand {
                action: Some(notification_rule_command::Action::GetInbox(
                    proto::GetNotificationInbox::default(),
                )),
            }),
            control_request::Command::ServerCommand(proto::ServerCommand {
                action: Some(server_command::Action::GetAccessSession(
                    proto::GetAccessSession {},
                )),
            }),
        ];
        assert!(
            user_commands
                .iter()
                .all(|command| { required_access_role(Some(command)) == AccessRole::User })
        );

        let administrator_commands = [
            control_request::Command::CameraConfigurationCommand(
                proto::CameraConfigurationCommand::default(),
            ),
            control_request::Command::RuntimeConfigurationCommand(
                proto::RuntimeConfigurationCommand::default(),
            ),
            control_request::Command::StateStoreCommand(proto::StateStoreCommand {
                action: Some(proto::state_store_command::Action::Get(proto::GetState {
                    namespace: "keeppeek.integrations.mqtt".to_owned(),
                    key: "configuration".to_owned(),
                })),
            }),
            control_request::Command::LoggingCommand(proto::LoggingCommand::default()),
            control_request::Command::ServerCommand(proto::ServerCommand::default()),
            control_request::Command::HealthCommand(proto::HealthCommand::default()),
            control_request::Command::ExportCommand(proto::ExportCommand::default()),
            control_request::Command::PublishEvent(proto::PublishEvent::default()),
            control_request::Command::EventPublicationCommand(
                proto::EventPublicationCommand::default(),
            ),
            control_request::Command::CameraControlCommand(proto::CameraControlCommand {
                action: Some(camera_control_command::Action::SetMotionDetection(
                    proto::SetMotionDetection::default(),
                )),
            }),
            control_request::Command::EventSearchCommand(proto::EventSearchCommand {
                action: Some(event_search_command::Action::ReplaceTerms(
                    proto::ReplaceEventSearchTerms::default(),
                )),
            }),
            control_request::Command::NotificationRuleCommand(proto::NotificationRuleCommand {
                action: Some(notification_rule_command::Action::SaveDraft(
                    proto::SaveNotificationRuleDraft::default(),
                )),
            }),
        ];
        assert!(
            administrator_commands.iter().all(|command| {
                required_access_role(Some(command)) == AccessRole::Administrator
            })
        );
    }

    #[test]
    fn event_publication_data_requires_an_administrator_session() {
        let state = ServerState::empty();
        let mut session = local_test_session();
        session.principal.role = AccessRole::User;
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(SessionId::from_u64(7), session);
        let handler = test_control_handler(state);

        let error = handler
            .handle_data_for_session(
                SessionId::from_u64(7),
                proto::DataChannelKind::ReliableData,
                proto::Message::default(),
            )
            .unwrap_err();

        assert_eq!(error.code, proto::ErrorCode::Rejected);
        assert_eq!(
            error.message,
            "Administrator role is required for this operation"
        );
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
                && value == "Authorization, Content-Type, Content-Encoding, Prefer"
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
    fn session_reaper_closes_idle_and_absolute_expiry_but_keeps_active_sessions() {
        let mut state = ServerState::empty();
        state.api_session_policy.idle_timeout = Duration::from_secs(1);
        let idle_id = SessionId::from_u64(801);
        let absolute_id = SessionId::from_u64(802);
        let active_id = SessionId::from_u64(803);
        let mut idle = local_test_session();
        idle.last_activity = Instant::now() - Duration::from_secs(2);
        let mut absolute = local_test_session();
        absolute.absolute_expires_at_ms = i64::try_from(unix_time_ms())
            .unwrap_or(i64::MAX)
            .saturating_sub(1);
        state.api_session_owners.lock().unwrap().extend([
            (idle_id, idle),
            (absolute_id, absolute),
            (active_id, local_test_session()),
        ]);

        expire_api_sessions(&state);

        let sessions = state.api_session_owners.lock().unwrap();
        assert!(!sessions.contains_key(&idle_id));
        assert!(!sessions.contains_key(&absolute_id));
        assert!(sessions.contains_key(&active_id));
        drop(sessions);
        let audit = state.access_manager.list_audit(10);
        assert!(
            audit
                .iter()
                .any(|event| { event.action == "session_expiry" && event.result == "idle_expiry" })
        );
        assert!(audit.iter().any(|event| {
            event.action == "session_expiry" && event.result == "absolute_expiry"
        }));
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
        state.api_session_owners.lock().unwrap().extend([
            (SessionId::from_u64(41), local_test_session()),
            (SessionId::from_u64(42), local_test_session()),
        ]);
        let handler = test_control_handler(state.clone());
        let capabilities = handler
            .initial_capabilities(SessionId::from_u64(41))
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
        let state = ServerState::empty();
        let session_id = SessionId::from_u64(19);
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(session_id, local_test_session());
        let handler = test_control_handler(state);
        let capabilities = handler
            .initial_capabilities(session_id)
            .expect("server handler must provide initial capabilities");

        assert_eq!(capabilities.revision, 2);
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
                "stored-media-keyframe-preview.v1",
                "keeppeek.identity.v1",
                "keeppeek.camera-access.v1"
            ]
        );
        assert_eq!(
            capabilities.access_session.unwrap().role,
            proto::AccessRole::Administrator as i32
        );

        let directory = std::env::temp_dir().join(format!(
            "keeppeek-layout-capability-{}",
            uuid::Uuid::new_v4()
        ));
        let persisted = test_control_handler(
            ServerState::empty().with_camera_config_path(directory.join("config.toml")),
        )
        .initial_capabilities(SessionId::from_u64(0))
        .expect("server handler must provide initial capabilities");
        assert!(
            persisted
                .capability_ids
                .iter()
                .any(|capability| capability == peek_layouts::CAPABILITY_ID)
        );
        assert!(
            persisted
                .capability_ids
                .iter()
                .any(|capability| capability == "keeppeek.configuration.v1")
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn initial_capabilities_advertise_backup_only_when_available() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-backup-capability-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        let capabilities = test_control_handler(
            ServerState::empty().with_backup_manager(BackupManager::open(config_path).unwrap()),
        )
        .initial_capabilities(SessionId::from_u64(0))
        .unwrap();

        assert!(
            capabilities
                .capability_ids
                .iter()
                .any(|capability| capability == "keeppeek.backup.v1")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn notification_rules_require_auth_and_preserve_conflict_revisions() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-notification-control-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let runtime = crate::notifications::Runtime::open(&directory.join("config.toml")).unwrap();
        let session_id = SessionId::from_u64(501);
        let state = ServerState::empty().with_notifications(runtime.handle());
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(session_id, local_test_session());
        let handler = test_control_handler(state);
        let capabilities = handler.initial_capabilities(session_id).unwrap();
        assert!(
            capabilities
                .capability_ids
                .contains(&"keeppeek.rules.v1".to_owned())
        );

        let pushover_application_token = "a23456789012345678901234567890";
        let pushover_user_key = "u23456789012345678901234567890";
        let pushover_destination = serde_json::json!({
            "application_token": pushover_application_token,
            "user_key": pushover_user_key,
            "device": "front-door-phone",
            "sound": "pushover",
            "priority": 0,
            "deep_link_base_url": "https://keeppeek.example/"
        })
        .to_string();
        let definition = serde_json::json!({
            "id": "front-door-person",
            "name": "Front door person",
            "enabled": true,
            "revision": 0,
            "owner_id": "ignored-client-owner",
            "triggers": ["event_created", "event_updated"],
            "filter": { "event_kinds": ["person"] },
            "schedule": {
                "timezone": "UTC",
                "active_windows": [],
                "quiet_hours": null
            },
            "cooldowns": [{ "scope": "event", "duration_ms": 30000 }],
            "rate_limits": [],
            "critical_bypass": null,
            "enrichment": {
                "deadline_ms": 10000,
                "maximum_revisions": 4,
                "maximum_attempts": 2,
                "maximum_attachment_bytes": 1048576,
                "wake_after_deadline": false
            },
            "actions": [
                {
                    "channel": "browser",
                    "destination": "",
                    "template": {
                        "title": "Person at {{source.id}}",
                        "body": "Open {{notification.deep_link}}"
                    },
                    "attachment": "when_available",
                    "allow_second_delivery": false
                },
                {
                    "channel": "webhook",
                    "destination": "https://hooks.example.invalid/keeppeek?token=secret-target",
                    "template": {
                        "title": "Person at {{source.id}}",
                        "body": "Open {{notification.deep_link}}"
                    },
                    "attachment": "never",
                    "allow_second_delivery": false
                },
                {
                    "channel": "push",
                    "destination": pushover_destination,
                    "template": {
                        "title": "Person at {{source.id}}",
                        "body": "Open {{notification.deep_link}}"
                    },
                    "attachment": "when_available",
                    "allow_second_delivery": false
                }
            ],
            "failure": {
                "maximum_attempts": 3,
                "maximum_retry_interval_ms": 60000,
                "expiry_ms": 3600000
            }
        })
        .to_string();
        let save = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 1,
                command: Some(control_request::Command::NotificationRuleCommand(
                    proto::NotificationRuleCommand {
                        action: Some(notification_rule_command::Action::SaveDraft(
                            proto::SaveNotificationRuleDraft {
                                definition_json: definition.clone(),
                                expected_draft_revision: 0,
                            },
                        )),
                    },
                )),
            },
        );
        let saved = match save.response.result.unwrap() {
            control_response::Result::Ok(proto::Ok {
                result:
                    Some(control_ok::Result::NotificationRuleResult(proto::NotificationRuleResult {
                        result: Some(notification_rule_result::Result::Rule(rule)),
                    })),
            }) => rule,
            other => panic!("unexpected notification save response: {other:?}"),
        };
        assert_eq!(saved.owner_id, "local-administrator");
        assert_eq!(saved.draft_revision, 1);
        assert!(!saved.draft_definition_json.contains("secret-target"));
        assert!(
            !saved
                .draft_definition_json
                .contains(pushover_application_token)
        );
        assert!(!saved.draft_definition_json.contains(pushover_user_key));
        let mut redacted: serde_json::Value =
            serde_json::from_str(&saved.draft_definition_json).unwrap();
        assert_eq!(redacted["actions"][1]["destination"], "");
        assert_eq!(redacted["actions"][1]["destination_configured"], true);
        assert_eq!(
            redacted["actions"][1]["destination_ref"]
                .as_str()
                .map(str::len),
            Some(64)
        );
        let push = redacted["actions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|action| action["channel"] == "push")
            .unwrap();
        assert_eq!(push["destination"], "");
        assert_eq!(push["destination_configured"], true);
        assert_eq!(push["pushover"]["device"], "front-door-phone");
        assert_eq!(push["pushover"]["priority"], 0);
        push["pushover"]["priority"] = 2.into();
        push["pushover"]["retry_seconds"] = 60.into();
        push["pushover"]["expire_seconds"] = 600.into();
        redacted["actions"].as_array_mut().unwrap().swap(0, 1);
        let reordered_redacted = serde_json::to_string(&redacted).unwrap();

        let activate = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 2,
                command: Some(control_request::Command::NotificationRuleCommand(
                    proto::NotificationRuleCommand {
                        action: Some(notification_rule_command::Action::Activate(
                            proto::ActivateNotificationRule {
                                rule_id: saved.rule_id.clone(),
                                expected_active_revision: 0,
                                expected_draft_revision: saved.draft_revision,
                            },
                        )),
                    },
                )),
            },
        );
        let active_revision = match activate.response.result.unwrap() {
            control_response::Result::Ok(proto::Ok {
                result:
                    Some(control_ok::Result::NotificationRuleResult(proto::NotificationRuleResult {
                        result: Some(notification_rule_result::Result::Rule(rule)),
                    })),
            }) => rule.active_revision,
            other => panic!("unexpected notification activation response: {other:?}"),
        };
        assert_eq!(active_revision, 1);

        let preserved = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 3,
                command: Some(control_request::Command::NotificationRuleCommand(
                    proto::NotificationRuleCommand {
                        action: Some(notification_rule_command::Action::SaveDraft(
                            proto::SaveNotificationRuleDraft {
                                definition_json: reordered_redacted,
                                expected_draft_revision: saved.draft_revision,
                            },
                        )),
                    },
                )),
            },
        );
        let preserved = match preserved.response.result.unwrap() {
            control_response::Result::Ok(proto::Ok {
                result:
                    Some(control_ok::Result::NotificationRuleResult(proto::NotificationRuleResult {
                        result: Some(notification_rule_result::Result::Rule(rule)),
                    })),
            }) => rule,
            other => panic!("unexpected preserved notification response: {other:?}"),
        };
        assert_eq!(preserved.draft_revision, 2);
        assert!(!preserved.draft_definition_json.contains("secret-target"));
        assert!(
            !preserved
                .draft_definition_json
                .contains(pushover_application_token)
        );
        assert!(!preserved.draft_definition_json.contains(pushover_user_key));
        let stored = runtime.handle().rules("local-administrator").unwrap();
        assert_eq!(
            stored[0]
                .draft
                .actions
                .iter()
                .find(|action| action.channel == crate::notifications::model::Channel::Webhook)
                .unwrap()
                .destination,
            "https://hooks.example.invalid/keeppeek?token=secret-target"
        );
        let push_destination = &stored[0]
            .draft
            .actions
            .iter()
            .find(|action| action.channel == crate::notifications::model::Channel::Push)
            .unwrap()
            .destination;
        let push_destination: serde_json::Value = serde_json::from_str(push_destination).unwrap();
        assert_eq!(
            push_destination["application_token"],
            pushover_application_token
        );
        assert_eq!(push_destination["user_key"], pushover_user_key);
        assert_eq!(push_destination["priority"], 2);
        assert_eq!(push_destination["retry_seconds"], 60);
        assert_eq!(push_destination["expire_seconds"], 600);

        let conflict = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 4,
                command: Some(control_request::Command::NotificationRuleCommand(
                    proto::NotificationRuleCommand {
                        action: Some(notification_rule_command::Action::SaveDraft(
                            proto::SaveNotificationRuleDraft {
                                definition_json: definition,
                                expected_draft_revision: 0,
                            },
                        )),
                    },
                )),
            },
        );
        let error = match conflict.response.result.unwrap() {
            control_response::Result::Error(error) => error,
            other => panic!("expected notification conflict: {other:?}"),
        };
        assert_eq!(error.code, proto::ErrorCode::Rejected as i32);
        assert_eq!(error.details.len(), 1);
        let details: serde_json::Value = serde_json::from_slice(&error.details[0].value).unwrap();
        assert_eq!(details["active_revision"], 1);
        assert_eq!(details["draft_revision"], 2);

        let unauthorized = handler.handle_for_session(
            SessionId::from_u64(999),
            proto::Request {
                request_id: 5,
                command: Some(control_request::Command::NotificationRuleCommand(
                    proto::NotificationRuleCommand {
                        action: Some(notification_rule_command::Action::ListRules(
                            proto::ListNotificationRules {},
                        )),
                    },
                )),
            },
        );
        assert!(matches!(
            unauthorized.response.result,
            Some(control_response::Result::Error(proto::Error {
                code,
                ..
            })) if code == proto::ErrorCode::Rejected as i32
        ));
        drop(handler);
        runtime.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn camera_access_does_not_advertise_capabilities_for_unknown_sessions() {
        let handler = test_control_handler(media_test_state());
        assert!(
            handler
                .initial_capabilities(SessionId::from_u64(999))
                .is_none()
        );
        assert!(
            !handler
                .initial_capabilities(SessionId::from_u64(0))
                .unwrap()
                .cameras
                .is_empty()
        );
    }

    #[test]
    fn camera_access_filters_capabilities_and_guards_direct_subscriptions() {
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
        let issued = restricted_test_user(&state);
        let session_id = SessionId::from_u64(707);
        bind_credential_test_session(&state, session_id, issued.access_key);
        let handler = test_control_handler(state.clone());
        let request = proto::Request {
            request_id: 1,
            command: Some(control_request::Command::SubscribeMedia(media_request(
                proto::MediaKind::Video,
                proto::DeliveryTransport::Rtp,
                proto::VideoQuality::Auto,
                "",
            ))),
        };
        let capabilities = handler.initial_capabilities(session_id).unwrap();
        assert!(capabilities.cameras.is_empty());
        assert!(capabilities.stored_media_sources.is_empty());
        assert!(
            capabilities
                .source_sessions
                .iter()
                .all(|source| source.source_id.is_empty())
        );
        assert!(
            handler
                .authorize_session_command(session_id, &request)
                .is_err()
        );
        state
            .access_manager
            .set_camera_access(
                issued.metadata.id,
                issued.metadata.revision,
                crate::access::CameraAccess {
                    all_cameras: false,
                    group_ids: Vec::new(),
                    camera_ids: vec!["127.0.0.1".to_owned()],
                },
            )
            .unwrap();
        assert!(
            handler
                .authorize_session_command(session_id, &request)
                .is_err()
        );
        bind_credential_test_session(&state, session_id, issued.access_key);
        assert_eq!(
            handler
                .initial_capabilities(session_id)
                .unwrap()
                .cameras
                .len(),
            1
        );
        handler
            .authorize_session_command(session_id, &request)
            .unwrap();
    }

    #[test]
    fn camera_access_state_store_is_admin_only_and_revokes_sessions() {
        use prost_types::{ListValue, Struct, Value, value::Kind};
        let state = media_test_state();
        let issued = restricted_test_user(&state);
        let session_id = SessionId::from_u64(712);
        bind_credential_test_session(&state, session_id, issued.access_key);
        let request = proto::Request {
            request_id: 1,
            command: Some(control_request::Command::StateStoreCommand(
                proto::StateStoreCommand {
                    action: Some(proto::state_store_command::Action::Put(proto::PutState {
                        namespace: "keeppeek.camera-access".to_owned(),
                        key: issued.metadata.id.to_string(),
                        schema: "keeppeek.camera-access.v1".to_owned(),
                        expected_revision: Some(issued.metadata.revision),
                        value: Some(Struct {
                            fields: [
                                (
                                    "all_cameras".to_owned(),
                                    Value {
                                        kind: Some(Kind::BoolValue(false)),
                                    },
                                ),
                                (
                                    "camera_ids".to_owned(),
                                    Value {
                                        kind: Some(Kind::ListValue(ListValue {
                                            values: vec![Value {
                                                kind: Some(Kind::StringValue(
                                                    "127.0.0.1".to_owned(),
                                                )),
                                            }],
                                        })),
                                    },
                                ),
                            ]
                            .into_iter()
                            .collect(),
                        }),
                        ..Default::default()
                    })),
                },
            )),
        };
        let handler = test_control_handler(state.clone());
        assert!(matches!(
            handler
                .handle_for_session(session_id, request.clone())
                .response
                .result,
            Some(control_response::Result::Error(_))
        ));
        let saved = handler.handle_for_session(SessionId::from_u64(0), request.clone());
        assert!(matches!(
            saved.response.result,
            Some(control_response::Result::Ok(_))
        ));
        assert!(
            !state
                .api_session_owners
                .lock()
                .unwrap()
                .contains_key(&session_id)
        );
        let updated = state
            .access_manager
            .list_credentials()
            .into_iter()
            .find(|credential| credential.id == issued.metadata.id)
            .unwrap();
        assert!(
            state
                .access_manager
                .camera_access(updated.id, updated.revision, 2_000)
                .unwrap()
                .allows("127.0.0.1")
        );
        let stale = handler.handle_for_session(SessionId::from_u64(0), request);
        let Some(control_response::Result::Error(error)) = stale.response.result else {
            panic!("stale permission writes must fail");
        };
        assert_eq!(error.code, proto::ErrorCode::Rejected as i32);
        let detail = proto::StateStoreError::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.code, proto::StateStoreErrorCode::Conflict as i32);
        assert_eq!(detail.current_revision, Some(updated.revision));
    }

    #[test]
    fn camera_access_inventory_failure_preserves_policy_and_sessions() {
        let state = media_test_state();
        state.cameras.write().unwrap()[0].groups =
            (0..129).map(|index| format!("group-{index}")).collect();
        let issued = restricted_test_user(&state);
        let session_id = SessionId::from_u64(719);
        bind_credential_test_session(&state, session_id, issued.access_key);
        let before = state
            .access_manager
            .camera_access_settings(issued.metadata.id)
            .unwrap();
        let handler = test_control_handler(state.clone());
        let response = handler.handle_for_session(
            SessionId::from_u64(0),
            proto::Request {
                request_id: 1,
                command: Some(control_request::Command::StateStoreCommand(
                    proto::StateStoreCommand {
                        action: Some(proto::state_store_command::Action::Put(proto::PutState {
                            namespace: "keeppeek.camera-access".to_owned(),
                            key: issued.metadata.id.to_string(),
                            schema: "keeppeek.camera-access.v1".to_owned(),
                            expected_revision: Some(issued.metadata.revision),
                            value: Some(prost_types::Struct {
                                fields: [
                                    (
                                        "all_cameras".to_owned(),
                                        prost_types::Value {
                                            kind: Some(prost_types::value::Kind::BoolValue(true)),
                                        },
                                    ),
                                    (
                                        "camera_ids".to_owned(),
                                        prost_types::Value {
                                            kind: Some(prost_types::value::Kind::ListValue(
                                                prost_types::ListValue::default(),
                                            )),
                                        },
                                    ),
                                ]
                                .into_iter()
                                .collect(),
                            }),
                            ..Default::default()
                        })),
                    },
                )),
            },
        );
        assert!(matches!(
            response.response.result,
            Some(control_response::Result::Error(_))
        ));
        let after = state
            .access_manager
            .camera_access_settings(issued.metadata.id)
            .unwrap();
        assert_eq!(after.0, before.0);
        assert_eq!(after.1.revision, before.1.revision);
        assert!(
            state
                .api_session_owners
                .lock()
                .unwrap()
                .contains_key(&session_id)
        );
    }

    #[test]
    fn camera_access_projects_grid_tiles_without_changing_saved_layouts() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-grid-access-{}", Uuid::new_v4()));
        let config_path = directory.join("config.toml");
        let state = media_test_state().with_camera_config_path(config_path.clone());
        let issued = restricted_test_user(&state);
        let session_id = SessionId::from_u64(711);
        bind_credential_test_session(&state, session_id, issued.access_key);
        let handler = test_control_handler(state);
        let result = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 1,
                command: Some(control_request::Command::StateStoreCommand(
                    proto::StateStoreCommand {
                        action: Some(proto::state_store_command::Action::Get(proto::GetState {
                            namespace: peek_layouts::NAMESPACE.to_owned(),
                            key: "registry".to_owned(),
                        })),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(proto::Ok {
            result:
                Some(control_ok::Result::StateStoreResult(proto::StateStoreResult {
                    result: Some(proto::state_store_result::Result::Entry(entry)),
                })),
        })) = result.response.result
        else {
            panic!("grid access must return an entry");
        };
        let value = entry.value.as_ref().unwrap();
        let serialized = format!("{value:?}");
        assert!(
            !serialized.contains("127.0.0.1"),
            "grid must not reveal an unauthorized camera ID"
        );
        let selected = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 2,
                command: Some(control_request::Command::StateStoreCommand(
                    proto::StateStoreCommand {
                        action: Some(proto::state_store_command::Action::Put(proto::PutState {
                            namespace: entry.namespace,
                            key: entry.key,
                            schema: entry.schema,
                            value: entry.value,
                            expected_revision: Some(entry.revision),
                            ..Default::default()
                        })),
                    },
                )),
            },
        );
        assert!(matches!(
            selected.response.result,
            Some(control_response::Result::Ok(_))
        ));
        let root = crate::config::load_configuration_table(&config_path).unwrap();
        assert_eq!(
            root["peek_layouts"]["shared_layouts"][0]["tiles"][0]["camera_id"].as_str(),
            Some("127.0.0.1")
        );
        drop(handler);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn camera_access_filters_recording_coverage_http() {
        let state = media_test_state();
        let issued = restricted_test_user(&state);
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let response = handle_request(
            &Request::fake_https_from(
                SocketAddr::from(([203, 0, 113, 7], 42000)),
                "GET",
                "/recording-coverage",
                vec![(
                    "Authorization".to_owned(),
                    format!("Bearer {}", issued.access_key.canonical()),
                )],
                Vec::new(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(response.status_code, 200);
        let (reader, _) = response.data.into_reader_and_size();
        let body: serde_json::Value = serde_json::from_reader(reader).unwrap();
        assert!(body["cameras"].as_array().unwrap().is_empty());
        assert!(body["findings"].as_array().unwrap().is_empty());
        assert_eq!(body["totals"]["cameras"], 0);
    }

    #[test]
    fn user_group_grants_combine_with_specific_camera_grants() {
        let state = media_test_state();
        {
            let mut cameras = state.cameras.write().unwrap();
            cameras[0].groups = vec!["outdoor".to_owned()];
            let mut explicit = cameras[0].clone();
            explicit.info.id = "127.0.0.2".to_owned();
            explicit.groups = vec!["indoor".to_owned()];
            let mut denied = explicit.clone();
            denied.info.id = "127.0.0.3".to_owned();
            cameras.extend([explicit, denied]);
        }
        let issued = state
            .access_manager
            .create_credential("Viewer", None, AccessRole::User, None, 1_000)
            .unwrap();
        let policy = toml::from_str::<crate::access::CameraAccess>(
            "all_cameras = false\ngroup_ids = ['outdoor']\ncamera_ids = ['127.0.0.2']\n",
        )
        .expect("a user policy must accept camera groups");
        state
            .access_manager
            .set_camera_access(issued.metadata.id, issued.metadata.revision, policy)
            .unwrap();
        let session_id = SessionId::from_u64(715);
        bind_credential_test_session(&state, session_id, issued.access_key);
        let resolved = camera_access::for_session(&state, session_id).unwrap();
        assert!(resolved.allows("127.0.0.1"));
        assert!(resolved.allows("127.0.0.2"));
        assert!(!resolved.allows("127.0.0.3"));
        let (saved, _) = state
            .access_manager
            .camera_access_settings(issued.metadata.id)
            .unwrap();
        let document = toml::Value::try_from(saved).unwrap();
        assert_eq!(document["group_ids"][0].as_str(), Some("outdoor"));
        assert_eq!(document["camera_ids"].as_array().unwrap().len(), 1);
        state.cameras.write().unwrap()[0].groups = vec!["indoor".to_owned()];
        let changed = camera_access::for_session(&state, session_id).unwrap();
        assert!(!changed.allows("127.0.0.1"));
        assert!(changed.allows("127.0.0.2"));
    }

    #[test]
    fn camera_group_changes_cancel_existing_user_work() {
        let state = media_test_state();
        state.cameras.write().unwrap()[0].groups = vec!["outdoor".to_owned()];
        let issued = restricted_test_user(&state);
        state
            .access_manager
            .set_camera_access(
                issued.metadata.id,
                issued.metadata.revision,
                crate::access::CameraAccess {
                    all_cameras: false,
                    group_ids: vec!["outdoor".to_owned()],
                    camera_ids: Vec::new(),
                },
            )
            .unwrap();
        let session_id = SessionId::from_u64(720);
        bind_credential_test_session(&state, session_id, issued.access_key);
        state
            .event_subscriptions
            .subscribe(
                &state,
                session_id,
                proto::SubscribeEvents {
                    subscription_id: "wildcard".to_owned(),
                    ..Default::default()
                },
            )
            .unwrap();
        let event = proto::Event {
            source_id: "127.0.0.1".to_owned(),
            ..Default::default()
        };
        let delivery = state.event_subscriptions.deliveries(&event).remove(0);
        let pending_cursor = (session_id, "playback".to_owned());
        state
            .stored_media_cursor_reservations
            .lock()
            .unwrap()
            .insert(pending_cursor.clone());
        let mut camera = state.camera("127.0.0.1").unwrap();
        camera.groups = vec!["indoor".to_owned()];
        state.upsert_camera(camera);
        assert!(camera_access::for_session(&state, session_id).is_err());
        assert!(!delivery.guard.is_active());
        assert!(state.event_subscriptions.deliveries(&event).is_empty());
        assert!(
            !state
                .stored_media_cursor_reservations
                .lock()
                .unwrap()
                .contains(&pending_cursor)
        );
    }

    #[test]
    fn camera_access_scopes_wildcard_camera_queries() {
        let state = media_test_state();
        let empty = crate::access::CameraAccess::default();
        assert!(
            camera_access::query_cameras(&state, &empty, &[])
                .unwrap()
                .is_empty()
        );
        let allowed = crate::access::CameraAccess {
            all_cameras: false,
            group_ids: Vec::new(),
            camera_ids: vec!["127.0.0.1".to_owned()],
        };
        let selected = camera_access::query_cameras(&state, &allowed, &[]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].info.id, "127.0.0.1");
        assert!(
            camera_access::query_cameras(&state, &allowed, &["127.0.0.2".to_owned()],).is_err()
        );
        assert_eq!(state.camera_entries().len(), 1);
    }

    #[test]
    fn camera_access_rejects_stored_media_and_camera_controls() {
        let state = media_test_state();
        let issued = restricted_test_user(&state);
        let session_id = SessionId::from_u64(708);
        bind_credential_test_session(&state, session_id, issued.access_key);
        let commands = [
            control_request::Command::CameraControlCommand(proto::CameraControlCommand {
                action: Some(camera_control_command::Action::Ptz(proto::PtzCommand {
                    source_id: "127.0.0.1".to_owned(),
                    ..Default::default()
                })),
            }),
            control_request::Command::StoredMediaCommand(proto::StoredMediaCommand {
                action: Some(proto::stored_media_command::Action::Open(
                    proto::OpenStoredMedia {
                        source_id: "127.0.0.1".to_owned(),
                        ..Default::default()
                    },
                )),
            }),
            control_request::Command::StoredMediaCommand(proto::StoredMediaCommand {
                action: Some(proto::stored_media_command::Action::QueryTimeline(
                    proto::QueryStoredMediaTimeline {
                        source_ids: vec!["127.0.0.1".to_owned()],
                        ..Default::default()
                    },
                )),
            }),
        ];
        let handler = test_control_handler(state);
        for command in commands {
            let error = handler
                .authorize_session_command(
                    session_id,
                    &proto::Request {
                        request_id: 1,
                        command: Some(command),
                    },
                )
                .expect_err("an unassigned camera must be denied before device or storage access");
            assert_eq!(error.code, proto::ErrorCode::Rejected);
        }
    }

    #[test]
    fn camera_access_rejects_event_media_and_unscoped_text_search() {
        let state = media_test_state();
        let issued = restricted_test_user(&state);
        let session_id = SessionId::from_u64(709);
        bind_credential_test_session(&state, session_id, issued.access_key);
        let mut media = proto::FetchEventSearchMedia {
            objects: vec![Default::default()],
            ..Default::default()
        };
        media.objects[0].source_id = "127.0.0.1".to_owned();
        let actions = [
            event_search_command::Action::FetchMedia(media),
            event_search_command::Action::Query(proto::QueryEvents {
                search: Some(proto::query_events::Search::Text(Default::default())),
                ..Default::default()
            }),
        ];
        let handler = test_control_handler(state);
        for action in actions {
            let request = proto::Request {
                request_id: 1,
                command: Some(control_request::Command::EventSearchCommand(
                    proto::EventSearchCommand {
                        action: Some(action),
                    },
                )),
            };
            assert!(
                handler
                    .authorize_session_command(session_id, &request)
                    .is_err()
            );
        }
    }

    #[test]
    fn camera_access_scopes_wildcard_event_subscriptions() {
        let state = ServerState::empty();
        let issued = restricted_test_user(&state);
        let session_id = SessionId::from_u64(710);
        bind_credential_test_session(&state, session_id, issued.access_key);
        let request = proto::SubscribeEvents {
            subscription_id: "events".to_owned(),
            ..Default::default()
        };
        state
            .event_subscriptions
            .subscribe(&state, session_id, request.clone())
            .unwrap();
        let allowed_event = proto::Event {
            source_id: "127.0.0.1".to_owned(),
            ..Default::default()
        };
        assert!(
            state
                .event_subscriptions
                .deliveries(&allowed_event)
                .is_empty()
        );
        state
            .access_manager
            .set_camera_access(
                issued.metadata.id,
                issued.metadata.revision,
                crate::access::CameraAccess {
                    all_cameras: false,
                    group_ids: Vec::new(),
                    camera_ids: vec!["127.0.0.1".to_owned()],
                },
            )
            .unwrap();
        bind_credential_test_session(&state, session_id, issued.access_key);
        state
            .event_subscriptions
            .subscribe(&state, session_id, request)
            .unwrap();
        assert_eq!(
            state.event_subscriptions.deliveries(&allowed_event).len(),
            1
        );
        let denied_event = proto::Event {
            source_id: "127.0.0.2".to_owned(),
            ..Default::default()
        };
        assert!(
            state
                .event_subscriptions
                .deliveries(&denied_event)
                .is_empty()
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
            .initial_capabilities(SessionId::from_u64(0))
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
    fn media_subscription_accepts_decoder_ready_h264_and_h265_data() {
        for (filename, media_type, codec, codec_name, dimensions) in [
            (
                "cc-4k-640x360-h264.mp4",
                mp4::MediaType::H264,
                crate::storage::VideoCodec::H264,
                "h264",
                (640, 368),
            ),
            (
                "cc-4k-640x360-h265.mp4",
                mp4::MediaType::H265,
                crate::storage::VideoCodec::H265,
                "h265",
                (640, 360),
            ),
        ] {
            let state = media_test_state();
            state.webrtc.live().publish(
                crate::webrtc::Source {
                    camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                    stream: StreamKind::Sub,
                },
                codec,
                true,
                Instant::now(),
                None,
                fixture_video_keyframe(filename, media_type),
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
            assert_eq!(plan.codec.name, codec_name);
            let Some(proto::media_data_format::Format::Video(format)) = plan.format.format else {
                panic!("reliable video subscription must return a video format");
            };
            assert_eq!((format.width, format.height), dimensions);
            assert!(!format.decoder_config.is_empty());
        }
    }

    #[test]
    fn media_subscription_rejects_reliable_data_without_decoder_parameters() {
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
        let source = handler
            .initial_capabilities(SessionId::from_u64(0))
            .unwrap()
            .source_sessions
            .into_iter()
            .find(|source| source.source_id == "127.0.0.1")
            .unwrap();
        let variant = source.video.unwrap().variants.remove(0);
        assert_eq!(
            variant.delivery_transports,
            [proto::DeliveryTransport::Rtp as i32]
        );

        let error = handler
            .resolve_media_subscription(&media_request(
                proto::MediaKind::Video,
                proto::DeliveryTransport::ReliableData,
                proto::VideoQuality::Low,
                "",
            ))
            .unwrap_err();

        assert_eq!(error.code, proto::ErrorCode::Unavailable);
        assert_eq!(
            error.message,
            "selected video delivery transport is unavailable"
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
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(SessionId::from_u64(7), local_test_session());
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
        for event_type in &source.event_types {
            assert_eq!(
                event_type.attachments,
                [proto::EventAttachmentCapability {
                    attachment_type: "snapshot".to_owned(),
                    content_type: "image/jpeg".to_owned(),
                    delivery_channels: vec![
                        proto::DataChannelKind::ReliableData as i32,
                        proto::DataChannelKind::UnreliableData as i32,
                    ],
                    maximum_count: 1,
                    minimum_count: 1,
                }]
            );
        }
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
            text: Some("Person waiting at the porch".to_owned()),
            payload: Some(prost_types::Struct {
                fields: std::collections::BTreeMap::from([
                    (
                        "object_class".to_owned(),
                        prost_types::Value {
                            kind: Some(prost_types::value::Kind::StringValue("person".to_owned())),
                        },
                    ),
                    (
                        "stream_id".to_owned(),
                        prost_types::Value {
                            kind: Some(prost_types::value::Kind::StringValue("sub".to_owned())),
                        },
                    ),
                ]),
            }),
            attachments: Vec::new(),
            source_session_id: Some(camera_source_session_id("127.0.0.1", 0)),
            subscription_id: None,
            canonical_attachment_id: None,
            icon_key: Some("<svg onload=alert(1)>".to_owned()),
            rejected_icon_key: None,
            bounding_box_attachment_id: None,
            image_availability: proto::EventImageAvailability::None as i32,
        };
        let request = proto::Request {
            request_id: 41,
            command: Some(control_request::Command::PublishEvent(
                proto::PublishEvent { event: Some(event) },
            )),
        };
        let subscribe = || {
            handler
                .state
                .event_subscriptions
                .subscribe(
                    &handler.state,
                    SessionId::from_u64(7),
                    proto::SubscribeEvents {
                        subscription_id: "events-1".to_owned(),
                        ..Default::default()
                    },
                )
                .unwrap();
        };
        subscribe();
        let first = handler.handle_for_session(SessionId::from_u64(7), request.clone());
        assert!(matches!(
            first.response.result,
            Some(control_response::Result::Ok(proto::Ok { result: None }))
        ));
        assert_eq!(handler.state.event_subscriptions.len(), 0);
        subscribe();
        let retry = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 43,
                ..request
            },
        );
        assert!(matches!(
            retry.response.result,
            Some(control_response::Result::Ok(proto::Ok { result: None }))
        ));
        assert_eq!(handler.state.event_subscriptions.len(), 1);
        assert!(
            handler
                .state
                .access_manager
                .list_audit(10)
                .iter()
                .any(|event| { event.action == "event_publish" && event.result == "success" })
        );

        let stored = handle.event_by_id("detector-event-1").unwrap().unwrap();
        assert_eq!(stored.source, EventSource::KeepPeek);
        assert_eq!(stored.camera_id, "127.0.0.1");
        assert_eq!(stored.stream.as_deref(), Some("sub"));
        assert_eq!(stored.kind, "person");
        assert_eq!(stored.start_time_ms, 12_345);
        assert_eq!(stored.text.as_deref(), Some("Person waiting at the porch"));
        assert_eq!(stored.payload.as_ref().unwrap()["object_class"], "person");
        assert_eq!(stored.confidence, Some(0.91));
        assert_eq!(stored.bbox, Some([0.1, 0.2, 0.3, 0.4]));
        assert_eq!(stored.icon_key, "person");
        assert_eq!(
            stored.rejected_icon_key.as_deref(),
            Some("<svg?onload=alert(1)>")
        );

        drop(handler);
        drop(handle);
        drop(catalog);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn event_publication_commits_one_snapshot_and_retries_idempotently() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-event-publication-start-{}",
            rand::random::<u64>()
        ));
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let events = EventStore::new(handle.clone(), &directory.join("thumbnails"), 0).unwrap();
        let mut state = media_test_state();
        state.events = Some(events);
        state.catalog = Some(handle.clone());
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(SessionId::from_u64(7), local_test_session());
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
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode_image(&image::DynamicImage::new_rgb8(2, 2))
            .unwrap();
        let subscription = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 49,
                command: Some(control_request::Command::SubscribeEvents(
                    proto::SubscribeEvents {
                        subscription_id: "events-1".to_owned(),
                        attachment_routes: vec![proto::EventAttachmentRoute {
                            attachment_type: "snapshot".to_owned(),
                            content_type: "image/jpeg".to_owned(),
                            channel: proto::DataChannelKind::ReliableData as i32,
                        }],
                        ..Default::default()
                    },
                )),
            },
        );
        assert!(matches!(
            subscription.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::SubscriptionResult(
                    proto::SubscriptionResult {
                        delivery: Some(proto::subscription_result::Delivery::Events(_)),
                        ..
                    }
                ))
            }))
        ));
        assert_eq!(handler.state.event_subscriptions.len(), 1);
        let event = proto::Event {
            event_id: "detector-event-attachment-1".to_owned(),
            revision: 1,
            source_id: "127.0.0.1".to_owned(),
            media_kind: Some(proto::MediaKind::Video as i32),
            origin: proto::EventOrigin::Keeppeek as i32,
            event_type: "person".to_owned(),
            start_time: Some(millis_timestamp(12_345)),
            confidence: Some(0.91),
            bounding_box: Some(proto::EventBoundingBox {
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
            }),
            payload: Some(prost_types::Struct {
                fields: std::collections::BTreeMap::from([
                    (
                        "object_class".to_owned(),
                        prost_types::Value {
                            kind: Some(prost_types::value::Kind::StringValue("person".to_owned())),
                        },
                    ),
                    (
                        "stream_id".to_owned(),
                        prost_types::Value {
                            kind: Some(prost_types::value::Kind::StringValue("sub".to_owned())),
                        },
                    ),
                ]),
            }),
            attachments: vec![proto::EventAttachmentDescriptor {
                attachment_id: "snapshot-1".to_owned(),
                attachment_type: "snapshot".to_owned(),
                content_type: "image/jpeg".to_owned(),
                byte_len: Some(jpeg.len() as u64),
                ordinal: 0,
                timestamp: Some(millis_timestamp(12_345)),
                text: None,
            }],
            source_session_id: Some(camera_source_session_id("127.0.0.1", 0)),
            canonical_attachment_id: Some("snapshot-1".to_owned()),
            bounding_box_attachment_id: Some("snapshot-1".to_owned()),
            image_availability: proto::EventImageAvailability::Available as i32,
            text: Some("Person waiting at the porch".to_owned()),
            ..Default::default()
        };
        let start = proto::StartEventPublication {
            publication_id: "publication-1".to_owned(),
            event: Some(event),
            attachment_channel: proto::DataChannelKind::ReliableData as i32,
        };
        let stale_source_start = start.clone();

        let dispatch = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 51,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Start(
                            start.clone(),
                        )),
                    },
                )),
            },
        );

        let Some(control_response::Result::Ok(proto::Ok {
            result: Some(control_ok::Result::EventPublicationState(publication)),
        })) = dispatch.response.result
        else {
            panic!("event publication start must return publication state");
        };
        assert_eq!(
            publication.status,
            proto::EventPublicationStatus::AcceptingAttachments as i32
        );
        assert_eq!(publication.publication_id, "publication-1");
        assert_eq!(publication.event_id, "detector-event-attachment-1");
        assert_eq!(publication.revision, 1);
        assert_eq!(
            publication.attachment_channel,
            proto::DataChannelKind::ReliableData as i32
        );
        assert!(publication.max_attachment_bytes >= 4);
        assert!(publication.max_event_attachment_bytes >= 4);
        assert!(publication.expires_at.is_some());
        assert!(
            handle
                .event_by_id("detector-event-attachment-1")
                .unwrap()
                .is_none()
        );
        assert_eq!(handler.state.event_subscriptions.len(), 1);

        let split = jpeg.len() / 2;
        let attachment_message = |chunk_index: u32, payload: &[u8]| proto::Message {
            message: Some(proto::message::Message::Event(proto::EventMessage {
                message: Some(proto::event_message::Message::Attachment(
                    proto::EventAttachmentChunk {
                        context: Some(proto::event_attachment_chunk::Context::PublicationId(
                            "publication-1".to_owned(),
                        )),
                        event_id: "detector-event-attachment-1".to_owned(),
                        revision: 1,
                        attachment_id: "snapshot-1".to_owned(),
                        attachment_type: "snapshot".to_owned(),
                        content_type: "image/jpeg".to_owned(),
                        ordinal: 0,
                        timestamp: Some(millis_timestamp(12_345)),
                        sequence: 1,
                        chunk_index,
                        chunk_count: 2,
                        payload: payload.to_vec(),
                    },
                )),
            })),
        };
        handler
            .handle_data_for_session(
                SessionId::from_u64(7),
                proto::DataChannelKind::ReliableData,
                attachment_message(0, &jpeg[..split]),
            )
            .unwrap();
        let incomplete = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 52,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Commit(
                            proto::CommitEventPublication {
                                publication_id: "publication-1".to_owned(),
                                wait_timeout: Some(prost_types::Duration {
                                    seconds: 0,
                                    nanos: 1_000_000,
                                }),
                            },
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Error(proto::Error { details, .. })) =
            incomplete.response.result
        else {
            panic!("incomplete publication must return an error");
        };
        let detail = proto::EventPublicationError::decode(details[0].value.as_slice()).unwrap();
        assert_eq!(
            detail.code,
            proto::EventPublicationErrorCode::AttachmentsIncomplete as i32
        );
        assert!(
            handle
                .event_by_id("detector-event-attachment-1")
                .unwrap()
                .is_none()
        );

        let dispatch = std::thread::scope(|scope| {
            let commit = scope.spawn(|| {
                handler.handle_for_session(
                    SessionId::from_u64(7),
                    proto::Request {
                        request_id: 53,
                        command: Some(control_request::Command::EventPublicationCommand(
                            proto::EventPublicationCommand {
                                action: Some(proto::event_publication_command::Action::Commit(
                                    proto::CommitEventPublication {
                                        publication_id: "publication-1".to_owned(),
                                        wait_timeout: Some(prost_types::Duration {
                                            seconds: 1,
                                            nanos: 0,
                                        }),
                                    },
                                )),
                            },
                        )),
                    },
                )
            });
            handler
                .state
                .event_publications
                .wait_until_commit_waiting(SessionId::from_u64(7), "publication-1");
            handler
                .handle_data_for_session(
                    SessionId::from_u64(7),
                    proto::DataChannelKind::ReliableData,
                    attachment_message(1, &jpeg[split..]),
                )
                .unwrap();
            commit.join().unwrap()
        });
        let Some(control_response::Result::Ok(proto::Ok {
            result: Some(control_ok::Result::EventPublicationState(publication)),
        })) = dispatch.response.result
        else {
            panic!("event publication commit must return publication state");
        };
        assert_eq!(
            publication.status,
            proto::EventPublicationStatus::Committed as i32
        );
        assert_eq!(handler.state.event_subscriptions.len(), 0);
        assert!(
            handler
                .state
                .access_manager
                .list_audit(10)
                .iter()
                .any(|event| event.action == "event_publish" && event.result == "success")
        );

        let dispatch = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 55,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Commit(
                            proto::CommitEventPublication {
                                publication_id: "publication-1".to_owned(),
                                wait_timeout: None,
                            },
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(proto::Ok {
            result: Some(control_ok::Result::EventPublicationState(publication)),
        })) = dispatch.response.result
        else {
            panic!("event publication commit must return publication state");
        };
        assert_eq!(
            publication.status,
            proto::EventPublicationStatus::Committed as i32
        );

        handler
            .state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(SessionId::from_u64(8), local_test_session());
        let reconnect_start = handler.handle_for_session(
            SessionId::from_u64(8),
            proto::Request {
                request_id: 56,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Start(
                            start.clone(),
                        )),
                    },
                )),
            },
        );
        assert!(matches!(
            reconnect_start.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::EventPublicationState(
                    proto::EventPublicationState { status, revision: 1, .. }
                ))
            })) if status == proto::EventPublicationStatus::AcceptingAttachments as i32
        ));
        handler
            .handle_data_for_session(
                SessionId::from_u64(8),
                proto::DataChannelKind::ReliableData,
                attachment_message(0, &jpeg[..split]),
            )
            .unwrap();
        handler
            .handle_data_for_session(
                SessionId::from_u64(8),
                proto::DataChannelKind::ReliableData,
                attachment_message(1, &jpeg[split..]),
            )
            .unwrap();
        let reconnect_commit = handler.handle_for_session(
            SessionId::from_u64(8),
            proto::Request {
                request_id: 58,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Commit(
                            proto::CommitEventPublication {
                                publication_id: "publication-1".to_owned(),
                                wait_timeout: None,
                            },
                        )),
                    },
                )),
            },
        );
        assert!(matches!(
            reconnect_commit.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::EventPublicationState(
                    proto::EventPublicationState { status, revision: 1, .. }
                ))
            })) if status == proto::EventPublicationStatus::Committed as i32
        ));
        assert_eq!(
            handler.state.event_publications.metrics_snapshot().commits,
            1
        );

        let stored = handle
            .event_by_id("detector-event-attachment-1")
            .unwrap()
            .unwrap();
        assert_eq!(stored.revision, 1);
        assert_eq!(stored.text.as_deref(), Some("Person waiting at the porch"));
        assert_eq!(stored.payload.as_ref().unwrap()["object_class"], "person");
        assert_eq!(stored.attachments.len(), 1);
        assert_eq!(
            stored.canonical_attachment_id.as_deref(),
            Some("snapshot-1")
        );
        let image_path = handler
            .state
            .events
            .as_ref()
            .unwrap()
            .thumbnail_path("127.0.0.1", "detector-event-attachment-1")
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(image_path).unwrap(), jpeg);
        let stored_timeline = query_stored_events(
            &handler.state,
            &handler.state.camera_entries(),
            12_000,
            13_000,
            proto::StoredMediaEventQuery {
                event_types: vec!["person".to_owned()],
                include_attachments: true,
            },
        )
        .unwrap();
        assert_eq!(stored_timeline.len(), 1);
        assert_eq!(
            stored_timeline[0].event.text.as_deref(),
            Some("Person waiting at the porch")
        );
        assert!(matches!(
            stored_timeline[0]
                .event
                .payload
                .as_ref()
                .and_then(|payload| payload.fields.get("object_class"))
                .and_then(|value| value.kind.as_ref()),
            Some(prost_types::value::Kind::StringValue(value)) if value == "person"
        ));
        let stored_attachment = stored_timeline[0].attachment.as_ref().unwrap();
        assert_eq!(stored_attachment.descriptor.id, "snapshot-1");
        assert_eq!(std::fs::read(&stored_attachment.path).unwrap(), jpeg);
        let mut search_query = EventMetadataQuery::new("sub", 12_000, 13_000);
        search_query.event_ids = vec!["detector-event-attachment-1".to_owned()];
        search_query.source_ids = vec!["127.0.0.1".to_owned()];
        search_query.text = Some("person waiting".to_owned());
        let mut hits = EventSearch::new(handle.clone())
            .search_metadata(search_query)
            .unwrap()
            .hits;
        refresh_event_search_image_availability(&handler.state, &mut hits);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].revision, 1);
        assert_eq!(hits[0].text.as_deref(), Some("Person waiting at the porch"));
        assert!(hits[0].image_available);
        assert_eq!(
            hits[0]
                .canonical_attachment
                .as_ref()
                .map(|attachment| attachment.id.as_str()),
            Some("snapshot-1")
        );
        let (_, attachment_messages) = fetch_event_search_media(
            &handler.state,
            proto::FetchEventSearchMedia {
                transfer_id: "published-snapshot".to_owned(),
                objects: vec![proto::EventSearchMediaObject {
                    object_id: "snapshot".to_owned(),
                    source_id: "127.0.0.1".to_owned(),
                    representation: proto::StoredMediaObjectRepresentation::EventAttachment as i32,
                    event_id: "detector-event-attachment-1".to_owned(),
                    event_revision: 1,
                    attachment_id: "snapshot-1".to_owned(),
                    ..Default::default()
                }],
                channel: proto::DataChannelKind::ReliableData as i32,
            },
        )
        .unwrap();
        let published_snapshot = attachment_messages
            .iter()
            .find_map(|message| {
                let Some(proto::message::Message::EventSearch(search)) = &message.message.message
                else {
                    return None;
                };
                let Some(proto::event_search_message::Message::MediaChunk(chunk)) = &search.message
                else {
                    return None;
                };
                Some(chunk.payload.as_slice())
            })
            .unwrap();
        assert_eq!(published_snapshot, jpeg);

        let retry = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 57,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Start(
                            start.clone(),
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Ok(proto::Ok {
            result: Some(control_ok::Result::EventPublicationState(publication)),
        })) = retry.response.result
        else {
            panic!("event publication start retry must return publication state");
        };
        assert_eq!(
            publication.status,
            proto::EventPublicationStatus::Committed as i32
        );

        let mut stale_start = start.clone();
        stale_start.publication_id = "publication-2".to_owned();
        let stale = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 59,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Start(stale_start)),
                    },
                )),
            },
        );
        let Some(control_response::Result::Error(proto::Error { details, .. })) =
            stale.response.result
        else {
            panic!("stale publication must return an error");
        };
        let detail = proto::EventPublicationError::decode(details[0].value.as_slice()).unwrap();
        assert_eq!(
            detail.code,
            proto::EventPublicationErrorCode::RevisionConflict as i32
        );
        assert_eq!(detail.current_revision, Some(1));

        let mut revised_jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut revised_jpeg)
            .encode_image(&image::DynamicImage::new_rgb8(3, 2))
            .unwrap();
        let mut revision_start = start.clone();
        revision_start.publication_id = "publication-revision-2".to_owned();
        let revision_event = revision_start.event.as_mut().unwrap();
        revision_event.revision = 2;
        revision_event.confidence = Some(0.97);
        revision_event.text = Some("Person entered the porch".to_owned());
        revision_event.attachments[0].byte_len = Some(revised_jpeg.len() as u64);
        revision_event.payload.as_mut().unwrap().fields.insert(
            "model_version".to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("2".to_owned())),
            },
        );
        let started = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 68,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Start(
                            revision_start,
                        )),
                    },
                )),
            },
        );
        assert!(matches!(
            started.response.result,
            Some(control_response::Result::Ok(_))
        ));
        handler
            .handle_data_for_session(
                SessionId::from_u64(7),
                proto::DataChannelKind::ReliableData,
                proto::Message {
                    message: Some(proto::message::Message::Event(proto::EventMessage {
                        message: Some(proto::event_message::Message::Attachment(
                            proto::EventAttachmentChunk {
                                context: Some(
                                    proto::event_attachment_chunk::Context::PublicationId(
                                        "publication-revision-2".to_owned(),
                                    ),
                                ),
                                event_id: "detector-event-attachment-1".to_owned(),
                                revision: 2,
                                attachment_id: "snapshot-1".to_owned(),
                                attachment_type: "snapshot".to_owned(),
                                content_type: "image/jpeg".to_owned(),
                                ordinal: 0,
                                timestamp: Some(millis_timestamp(12_345)),
                                sequence: 1,
                                chunk_index: 0,
                                chunk_count: 1,
                                payload: revised_jpeg.clone(),
                            },
                        )),
                    })),
                },
            )
            .unwrap();
        for request_id in [69, 70] {
            let committed = handler.handle_for_session(
                SessionId::from_u64(7),
                proto::Request {
                    request_id,
                    command: Some(control_request::Command::EventPublicationCommand(
                        proto::EventPublicationCommand {
                            action: Some(proto::event_publication_command::Action::Commit(
                                proto::CommitEventPublication {
                                    publication_id: "publication-revision-2".to_owned(),
                                    wait_timeout: None,
                                },
                            )),
                        },
                    )),
                },
            );
            assert!(matches!(
                committed.response.result,
                Some(control_response::Result::Ok(proto::Ok {
                    result: Some(control_ok::Result::EventPublicationState(
                        proto::EventPublicationState { status, revision: 2, .. }
                    ))
                })) if status == proto::EventPublicationStatus::Committed as i32
            ));
        }
        let revised = handle
            .event_by_id("detector-event-attachment-1")
            .unwrap()
            .unwrap();
        assert_eq!(revised.revision, 2);
        assert_eq!(revised.confidence, Some(0.97));
        assert_eq!(revised.text.as_deref(), Some("Person entered the porch"));
        assert_eq!(revised.payload.as_ref().unwrap()["model_version"], "2");
        assert!(
            !directory
                .join("thumbnails/detector-event-attachment-1--r1.jpg")
                .exists()
        );
        assert_eq!(
            std::fs::read(directory.join("thumbnails/detector-event-attachment-1--r2.jpg"))
                .unwrap(),
            revised_jpeg
        );

        let mut unknown_source = start.clone();
        unknown_source.publication_id = "reject-source".to_owned();
        unknown_source.event.as_mut().unwrap().source_id = "unknown-camera".to_owned();
        let mut unsupported_type = start.clone();
        unsupported_type.publication_id = "reject-type".to_owned();
        unsupported_type.event.as_mut().unwrap().event_type = "motion".to_owned();
        let mut wrong_channel = start.clone();
        wrong_channel.publication_id = "reject-channel".to_owned();
        wrong_channel.attachment_channel = proto::DataChannelKind::UnreliableData as i32;
        let mut wrong_count = start.clone();
        wrong_count.publication_id = "reject-count".to_owned();
        wrong_count.event.as_mut().unwrap().attachments.clear();
        let mut wrong_metadata = start.clone();
        wrong_metadata.publication_id = "reject-metadata".to_owned();
        wrong_metadata.event.as_mut().unwrap().attachments[0].content_type = "image/png".to_owned();
        let mut oversized = start.clone();
        oversized.publication_id = "reject-size".to_owned();
        oversized.event.as_mut().unwrap().attachments[0].byte_len =
            Some(event_publication::MAXIMUM_ATTACHMENT_BYTES + 1);
        for (request_id, rejected, expected) in [
            (
                60,
                unknown_source,
                proto::EventPublicationErrorCode::SourceNotFound,
            ),
            (
                61,
                unsupported_type,
                proto::EventPublicationErrorCode::EventInvalid,
            ),
            (
                62,
                wrong_channel,
                proto::EventPublicationErrorCode::AttachmentInvalid,
            ),
            (
                63,
                wrong_count,
                proto::EventPublicationErrorCode::AttachmentCountMismatch,
            ),
            (
                64,
                wrong_metadata,
                proto::EventPublicationErrorCode::AttachmentInvalid,
            ),
            (
                65,
                oversized,
                proto::EventPublicationErrorCode::SizeLimitExceeded,
            ),
        ] {
            let rejected = handler.handle_for_session(
                SessionId::from_u64(7),
                proto::Request {
                    request_id,
                    command: Some(control_request::Command::EventPublicationCommand(
                        proto::EventPublicationCommand {
                            action: Some(proto::event_publication_command::Action::Start(rejected)),
                        },
                    )),
                },
            );
            let Some(control_response::Result::Error(proto::Error { details, .. })) =
                rejected.response.result
            else {
                panic!("invalid publication start must return an error");
            };
            let detail = proto::EventPublicationError::decode(details[0].value.as_slice()).unwrap();
            assert_eq!(
                proto::EventPublicationErrorCode::try_from(detail.code),
                Ok(expected)
            );
        }

        handle
            .insert_event(TimelineEvent {
                id: "camera-owned-event".to_owned(),
                revision: 1,
                camera_id: "127.0.0.1".to_owned(),
                stream: Some("sub".to_owned()),
                source: EventSource::Camera,
                kind: "motion".to_owned(),
                start_time_ms: 12_345,
                end_time_ms: None,
                confidence: None,
                bbox: None,
                bbox_attachment_id: None,
                zone: None,
                text: None,
                payload: None,
                attachments: Vec::new(),
                canonical_attachment_id: None,
                icon_key: "motion".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: None,
            })
            .unwrap();
        let mut foreign_revision = start.clone();
        foreign_revision.publication_id = "reject-foreign-revision".to_owned();
        let foreign_event = foreign_revision.event.as_mut().unwrap();
        foreign_event.event_id = "camera-owned-event".to_owned();
        foreign_event.revision = 2;
        let rejected = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 66,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Start(
                            foreign_revision,
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Error(proto::Error { details, .. })) =
            rejected.response.result
        else {
            panic!("foreign event revision must return an error");
        };
        let detail = proto::EventPublicationError::decode(details[0].value.as_slice()).unwrap();
        assert_eq!(
            detail.code,
            proto::EventPublicationErrorCode::RevisionConflict as i32
        );
        assert_eq!(detail.current_revision, Some(1));

        let mut invalid_start = start;
        invalid_start.publication_id = "publication-invalid".to_owned();
        let invalid_event = invalid_start.event.as_mut().unwrap();
        invalid_event.event_id = "detector-event-invalid-image".to_owned();
        invalid_event.attachments[0].byte_len = Some(4);
        let started = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 61,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Start(
                            invalid_start,
                        )),
                    },
                )),
            },
        );
        assert!(matches!(
            started.response.result,
            Some(control_response::Result::Ok(_))
        ));
        handler
            .handle_data_for_session(
                SessionId::from_u64(7),
                proto::DataChannelKind::ReliableData,
                proto::Message {
                    message: Some(proto::message::Message::Event(proto::EventMessage {
                        message: Some(proto::event_message::Message::Attachment(
                            proto::EventAttachmentChunk {
                                context: Some(
                                    proto::event_attachment_chunk::Context::PublicationId(
                                        "publication-invalid".to_owned(),
                                    ),
                                ),
                                event_id: "detector-event-invalid-image".to_owned(),
                                revision: 1,
                                attachment_id: "snapshot-1".to_owned(),
                                attachment_type: "snapshot".to_owned(),
                                content_type: "image/jpeg".to_owned(),
                                ordinal: 0,
                                timestamp: Some(millis_timestamp(12_345)),
                                sequence: 1,
                                chunk_index: 0,
                                chunk_count: 1,
                                payload: vec![0, 1, 2, 3],
                            },
                        )),
                    })),
                },
            )
            .unwrap();
        let invalid = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 63,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Commit(
                            proto::CommitEventPublication {
                                publication_id: "publication-invalid".to_owned(),
                                wait_timeout: None,
                            },
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Error(proto::Error { details, .. })) =
            invalid.response.result
        else {
            panic!("invalid publication image must return an error");
        };
        let detail = proto::EventPublicationError::decode(details[0].value.as_slice()).unwrap();
        assert_eq!(
            detail.code,
            proto::EventPublicationErrorCode::AttachmentInvalid as i32
        );
        assert!(
            handle
                .event_by_id("detector-event-invalid-image")
                .unwrap()
                .is_none()
        );
        let aborted = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 65,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Abort(
                            proto::AbortEventPublication {
                                publication_id: "publication-invalid".to_owned(),
                            },
                        )),
                    },
                )),
            },
        );
        assert!(matches!(
            aborted.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::EventPublicationState(
                    proto::EventPublicationState { status, .. }
                ))
            })) if status == proto::EventPublicationStatus::Aborted as i32
        ));
        assert!(
            !directory
                .join("thumbnails/detector-event-invalid-image--r1.jpg")
                .exists()
        );
        handler
            .state
            .webrtc
            .live()
            .reset_camera(IpAddr::V4(Ipv4Addr::LOCALHOST));
        handler.state.webrtc.live().publish(
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
        let current_source = handler
            .initial_capabilities(SessionId::from_u64(7))
            .unwrap()
            .source_sessions
            .into_iter()
            .find(|source| source.source_id == "127.0.0.1")
            .unwrap();
        assert_eq!(current_source.source_session_id, "camera:127.0.0.1:1");
        let mut stale_source_start = stale_source_start;
        stale_source_start.publication_id = "publication-stale-source".to_owned();
        stale_source_start.event.as_mut().unwrap().event_id = "stale-source-event".to_owned();
        let stale_source = handler.handle_for_session(
            SessionId::from_u64(7),
            proto::Request {
                request_id: 67,
                command: Some(control_request::Command::EventPublicationCommand(
                    proto::EventPublicationCommand {
                        action: Some(proto::event_publication_command::Action::Start(
                            stale_source_start,
                        )),
                    },
                )),
            },
        );
        let Some(control_response::Result::Error(proto::Error { details, .. })) =
            stale_source.response.result
        else {
            panic!("stale source session must return an error");
        };
        let detail = proto::EventPublicationError::decode(details[0].value.as_slice()).unwrap();
        assert_eq!(
            detail.code,
            proto::EventPublicationErrorCode::SourceNotFound as i32
        );

        drop(handler);
        drop(handle);
        catalog.shutdown();
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
                revision: 1,
                camera_id: "127.0.0.1".to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::Camera,
                kind: "motion".to_owned(),
                start_time_ms: 1_500,
                end_time_ms: Some(1_700),
                confidence: Some(0.8),
                bbox: Some([0.1, 0.2, 0.3, 0.4]),
                bbox_attachment_id: None,
                zone: Some("porch".to_owned()),
                text: None,
                payload: None,
                attachments: Vec::new(),
                canonical_attachment_id: None,
                icon_key: "motion".to_owned(),
                rejected_icon_key: None,
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
    fn event_page_tokens_are_signed_and_expire() {
        let state = media_test_state();
        let token = seal_event_page_token_until(
            &state,
            "catalog-cursor".to_owned(),
            unix_time_ms().saturating_add(60_000),
        )
        .unwrap();
        assert_eq!(
            open_event_page_token(&state, &token).unwrap(),
            "catalog-cursor"
        );

        let (payload, signature) = token.split_once('.').unwrap();
        let mut tampered_payload = payload.as_bytes().to_vec();
        tampered_payload[0] = if tampered_payload[0] == b'A' {
            b'B'
        } else {
            b'A'
        };
        let tampered = format!(
            "{}.{}",
            String::from_utf8(tampered_payload).unwrap(),
            signature
        );
        let error = open_event_page_token(&state, &tampered).unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
        assert_eq!(error.message, "event search page token is invalid");

        let expired = seal_event_page_token_until(
            &state,
            "catalog-cursor".to_owned(),
            unix_time_ms().saturating_sub(1),
        )
        .unwrap();
        let error = open_event_page_token(&state, &expired).unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected);
        assert_eq!(
            error.message,
            "event search page token expired; restart the query"
        );
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
                revision: 1,
                camera_id: "127.0.0.1".to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::KeepPeek,
                kind: "face".to_owned(),
                start_time_ms: event_time_ms,
                end_time_ms: Some(event_time_ms + 200),
                confidence: Some(0.98),
                bbox: Some([0.1, 0.2, 0.3, 0.4]),
                bbox_attachment_id: Some("thumbnail".to_owned()),
                zone: Some("Porch".to_owned()),
                text: None,
                payload: None,
                attachments: vec![EventAttachment {
                    id: "thumbnail".to_owned(),
                    attachment_type: "thumbnail".to_owned(),
                    content_type: "image/jpeg".to_owned(),
                    byte_len: None,
                    ordinal: 0,
                    timestamp_ms: Some(event_time_ms),
                    text: None,
                }],
                canonical_attachment_id: Some("thumbnail".to_owned()),
                icon_key: "person".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: Some("face-1.jpg".to_owned()),
            })
            .unwrap();
        handle
            .link_event_keyframe(crate::storage::CatalogEventKeyframeLink {
                event_id: "face-1".to_owned(),
                stream_id: "sub".to_owned(),
                recording_id: fragments[0].recording_id.clone(),
                fragment_sequence: fragments[0].sequence,
            })
            .unwrap();
        let thumbnail_root = directory.join("event-thumbnails");
        std::fs::create_dir_all(&thumbnail_root).unwrap();
        let thumbnail_path = thumbnail_root.join("face-1.jpg");
        std::fs::write(&thumbnail_path, [9_u8, 8, 7]).unwrap();
        let event_store = EventStore::new(handle.clone(), &thumbnail_root, 0).unwrap();

        let mut state = media_test_state();
        state.catalog = Some(handle);
        state.events = Some(event_store);
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
        let unauthorized_page = handler.handle(proto::Request {
            request_id: 198,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::Query(proto::QueryEvents {
                        query_id: "unauthorized-metadata".to_owned(),
                        search: Some(proto::query_events::Search::Metadata(
                            proto::EventMetadataSearch {
                                source_ids: vec!["192.0.2.99".to_owned()],
                                ..Default::default()
                            },
                        )),
                        stream_id: "main".to_owned(),
                        start_time: Some(millis_timestamp(fragments[0].start_ms - 1_000)),
                        end_time: Some(millis_timestamp(fragments[1].start_ms + 2_000)),
                        page_size: 10,
                        channel: proto::DataChannelKind::ReliableData as i32,
                        ..Default::default()
                    })),
                },
            )),
        });
        assert!(matches!(
            unauthorized_page.response.result,
            Some(control_response::Result::Error(proto::Error {
                code,
                ..
            })) if code == proto::ErrorCode::NotFound as i32
        ));
        let invalid_confidence = handler.handle(proto::Request {
            request_id: 197,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::Query(proto::QueryEvents {
                        query_id: "invalid-confidence".to_owned(),
                        search: Some(proto::query_events::Search::Metadata(
                            proto::EventMetadataSearch {
                                minimum_confidence: Some(f64::NAN),
                                ..Default::default()
                            },
                        )),
                        stream_id: "main".to_owned(),
                        start_time: Some(millis_timestamp(fragments[0].start_ms - 1_000)),
                        end_time: Some(millis_timestamp(fragments[1].start_ms + 2_000)),
                        page_size: 10,
                        channel: proto::DataChannelKind::ReliableData as i32,
                        ..Default::default()
                    })),
                },
            )),
        });
        assert!(matches!(
            invalid_confidence.response.result,
            Some(control_response::Result::Error(proto::Error {
                code,
                ..
            })) if code == proto::ErrorCode::InvalidRequest as i32
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
                            terms: vec![
                                proto::EventSearchTerm {
                                    field: proto::EventSearchField::FaceName as i32,
                                    value: "Alice Example".to_owned(),
                                },
                                proto::EventSearchTerm {
                                    field: proto::EventSearchField::Text as i32,
                                    value: "Front porch visitor".to_owned(),
                                },
                            ],
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

        let metadata = handler.handle(proto::Request {
            request_id: 202,
            command: Some(control_request::Command::EventSearchCommand(
                proto::EventSearchCommand {
                    action: Some(event_search_command::Action::Query(proto::QueryEvents {
                        query_id: "metadata-query-1".to_owned(),
                        search: Some(proto::query_events::Search::Metadata(
                            proto::EventMetadataSearch {
                                event_ids: Vec::new(),
                                source_ids: vec!["127.0.0.1".to_owned()],
                                event_types: vec!["face".to_owned()],
                                origins: vec![proto::EventOrigin::Keeppeek as i32],
                                zones: vec!["porch".to_owned()],
                                minimum_confidence: Some(0.95),
                                image: proto::EventImageFilter::WithImage as i32,
                                text: Some("ali".to_owned()),
                                include_preview_keyframes: true,
                            },
                        )),
                        source_id: None,
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
        let Some(proto::message::Message::EventSearch(metadata_message)) =
            &metadata.data_messages[0].message.message
        else {
            panic!("metadata search must emit an event-search result");
        };
        let Some(proto::event_search_message::Message::Result(metadata_result)) =
            &metadata_message.message
        else {
            panic!("metadata search must emit a result first");
        };
        let metadata_hit = metadata_result.hit.as_ref().unwrap();
        assert_eq!(metadata_hit.event_id, "face-1");
        assert_eq!(metadata_hit.origin, proto::EventOrigin::Keeppeek as i32);
        assert_eq!(metadata_hit.confidence, Some(0.98));
        assert_eq!(metadata_hit.zone.as_deref(), Some("Porch"));
        assert_eq!(metadata_hit.text.as_deref(), Some("Front porch visitor"));
        assert!(metadata_hit.has_image_attachment);
        assert_eq!(
            metadata_hit.bounding_box.as_ref().map(|bbox| bbox.width),
            Some(0.3)
        );

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
        assert_eq!(hit.keyframes.len(), 1);
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
                event_id: String::new(),
                event_revision: 0,
                attachment_id: String::new(),
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

        let attachment_object = proto::EventSearchMediaObject {
            object_id: "canonical".to_owned(),
            source_id: "127.0.0.1".to_owned(),
            stream_id: String::new(),
            recording_id: String::new(),
            fragment_sequence: 0,
            representation: proto::StoredMediaObjectRepresentation::EventAttachment as i32,
            event_id: "face-1".to_owned(),
            event_revision: 1,
            attachment_id: "thumbnail".to_owned(),
        };
        let attachment_request = proto::FetchEventSearchMedia {
            transfer_id: "event-attachment".to_owned(),
            objects: vec![attachment_object],
            channel: proto::DataChannelKind::ReliableData as i32,
        };
        let (_, attachment_messages) =
            fetch_event_search_media(&handler.state, attachment_request.clone()).unwrap();
        let attachment_chunk = attachment_messages
            .iter()
            .find_map(|message| {
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
            .unwrap();
        assert_eq!(attachment_chunk.payload, [9, 8, 7]);
        assert_eq!(attachment_chunk.event_id, "face-1");
        assert_eq!(attachment_chunk.event_revision, 1);
        assert_eq!(attachment_chunk.attachment_id, "thumbnail");

        let mut stale_request = attachment_request.clone();
        stale_request.transfer_id = "event-attachment-stale".to_owned();
        stale_request.objects[0].event_revision = 2;
        let stale = fetch_event_search_media(&handler.state, stale_request).unwrap_err();
        assert_eq!(stale.code, proto::ErrorCode::Rejected);
        assert_eq!(stale.message, "event attachment revision is stale");

        std::fs::remove_file(&thumbnail_path).unwrap();
        let mut availability_query = EventMetadataQuery::new(
            "main",
            event_time_ms.saturating_sub(1),
            event_time_ms.saturating_add(1_000),
        );
        availability_query.event_ids = vec!["face-1".to_owned()];
        let mut availability_hits =
            EventSearch::new(handler.state.catalog.as_ref().unwrap().clone())
                .search_metadata(availability_query.clone())
                .unwrap()
                .hits;
        refresh_event_search_image_availability(&handler.state, &mut availability_hits);
        assert!(!availability_hits[0].image_available);
        let unavailable =
            fetch_event_search_media(&handler.state, attachment_request.clone()).unwrap_err();
        assert_eq!(unavailable.code, proto::ErrorCode::Unavailable);
        assert_eq!(
            unavailable.message,
            "canonical event attachment is unavailable"
        );
        std::fs::write(&thumbnail_path, [9_u8, 8, 7]).unwrap();
        let mut availability_hits =
            EventSearch::new(handler.state.catalog.as_ref().unwrap().clone())
                .search_metadata(availability_query)
                .unwrap()
                .hits;
        refresh_event_search_image_availability(&handler.state, &mut availability_hits);
        assert!(availability_hits[0].image_available);
        let (_, retried) = fetch_event_search_media(&handler.state, attachment_request).unwrap();
        assert!(retried.iter().any(|message| matches!(
            message.message.message,
            Some(proto::message::Message::EventSearch(
                proto::EventSearchMessage {
                    message: Some(proto::event_search_message::Message::MediaEnd(_))
                }
            ))
        )));

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

        let disconnected =
            register_event_search_task(&state, session_id, "event-search-media:disconnect")
                .unwrap();
        test_control_handler(state.clone()).session_closed(session_id);
        assert!(disconnected.load(Ordering::Acquire));
        finish_event_search_task(
            &state,
            session_id,
            "event-search-media:disconnect",
            &disconnected,
        );
    }

    #[test]
    fn camera_discovery_cancel_is_scoped_to_session() {
        let state = ServerState::empty();
        let owner = SessionId::from_u64(92);
        let other = SessionId::from_u64(93);
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(owner, local_test_session());
        let owner_task = state
            .camera_discovery_tasks
            .start(owner, "shared-discovery")
            .unwrap()
            .unwrap();
        let other_task = state
            .camera_discovery_tasks
            .start(other, "shared-discovery")
            .unwrap()
            .unwrap();

        let response = test_control_handler(state).handle_for_session(
            owner,
            proto::Request {
                request_id: 92,
                command: Some(control_request::Command::CameraConfigurationCommand(
                    proto::CameraConfigurationCommand {
                        action: Some(camera_configuration_command::Action::CancelDiscovery(
                            proto::CancelCameraDiscovery {
                                discovery_id: "shared-discovery".to_owned(),
                            },
                        )),
                    },
                )),
            },
        );

        assert!(matches!(
            response.response.result,
            Some(control_response::Result::Ok(proto::Ok {
                result: Some(control_ok::Result::CameraDiscoveryResult(
                    proto::CameraDiscoveryResult {
                        cancelled: true,
                        ..
                    }
                ))
            }))
        ));
        assert!(owner_task.is_cancelled());
        assert!(!other_task.is_cancelled());
    }

    #[test]
    fn camera_discovery_disconnect_cancels_only_owned_tasks() {
        let state = ServerState::empty();
        let owner = SessionId::from_u64(94);
        let other = SessionId::from_u64(95);
        let owner_task = state
            .camera_discovery_tasks
            .start(owner, "shared-discovery")
            .unwrap()
            .unwrap();
        let other_task = state
            .camera_discovery_tasks
            .start(other, "shared-discovery")
            .unwrap()
            .unwrap();

        test_control_handler(state.clone()).session_closed(owner);

        assert!(owner_task.is_cancelled());
        assert!(!other_task.is_cancelled());
        assert!(
            state
                .camera_discovery_tasks
                .snapshot(owner, "shared-discovery")
                .is_err()
        );
        assert!(
            state
                .camera_discovery_tasks
                .snapshot(other, "shared-discovery")
                .is_ok()
        );
    }

    #[test]
    fn missing_stored_media_cursor_state_returns_not_found() {
        let state = ServerState::empty();

        let error =
            stored_media_cursor_state(&state, SessionId::from_u64(1), "closed-during-refill")
                .unwrap_err();

        assert_eq!(error.code, proto::ErrorCode::NotFound);
        assert_eq!(error._http_status, 404);
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
        let session_id = SessionId::from_u64(77);
        let foreign_session_id = SessionId::from_u64(78);
        let disconnect_session_id = SessionId::from_u64(79);
        state.api_session_owners.lock().unwrap().extend([
            (session_id, local_test_session()),
            (foreign_session_id, local_test_session()),
            (disconnect_session_id, local_test_session()),
        ]);
        let handler = test_control_handler(state.clone());
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

        let duplicate = handler.handle_for_session(
            session_id,
            proto::Request {
                request_id: 109,
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
        assert!(matches!(
            duplicate.response.result,
            Some(control_response::Result::Error(proto::Error { code, .. }))
                if code == proto::ErrorCode::Rejected as i32
        ));
        assert!(duplicate.data_messages.is_empty());
        assert!(duplicate.notifications.is_empty());
        assert_eq!(state.recording_demand.viewer_count("front-door/sub"), 1);

        let foreign_close = handler.handle_for_session(
            foreign_session_id,
            proto::Request {
                request_id: 110,
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
        assert!(matches!(
            foreign_close.response.result,
            Some(control_response::Result::Error(proto::Error { code, .. }))
                if code == proto::ErrorCode::NotFound as i32
        ));
        assert_eq!(state.recording_demand.viewer_count("front-door/sub"), 1);

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

        let disconnect_open = handler.handle_for_session(
            disconnect_session_id,
            proto::Request {
                request_id: 111,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::Open(proto::OpenStoredMedia {
                            stored_media_id: "disconnect-review".to_owned(),
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
        assert!(matches!(
            disconnect_open.response.result,
            Some(control_response::Result::Ok(_))
        ));
        assert_eq!(state.recording_demand.viewer_count("front-door/sub"), 1);

        handler.session_closed(disconnect_session_id);
        assert_eq!(state.recording_demand.viewer_count("front-door/sub"), 0);
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(disconnect_session_id, local_test_session());

        let close_after_disconnect = handler.handle_for_session(
            disconnect_session_id,
            proto::Request {
                request_id: 112,
                command: Some(control_request::Command::StoredMediaCommand(
                    proto::StoredMediaCommand {
                        action: Some(stored_media_command::Action::Close(
                            proto::CloseStoredMedia {
                                stored_media_id: "disconnect-review".to_owned(),
                            },
                        )),
                    },
                )),
            },
        );
        assert!(matches!(
            close_after_disconnect.response.result,
            Some(control_response::Result::Error(proto::Error { code, .. }))
                if code == proto::ErrorCode::NotFound as i32
        ));

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
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata/cc-4k-640x360-h264.mp4");
        let mut source = mp4::read_mp4(File::open(fixture).unwrap()).unwrap();
        let (track_id, timescale, width, height, sps, pps, sample_count) = {
            let (&track_id, track) = source
                .tracks()
                .iter()
                .find(|(_, track)| track.media_type().ok() == Some(mp4::MediaType::H264))
                .unwrap();
            (
                track_id,
                track.timescale(),
                u32::from(track.width()),
                u32::from(track.height()),
                track.sequence_parameter_set().unwrap().to_vec(),
                track.picture_parameter_set().unwrap().to_vec(),
                track.sample_count(),
            )
        };
        let mut elapsed = Duration::ZERO;
        let mut found_keyframe = false;
        for sample_id in 1..=sample_count {
            let sample = source.read_sample(track_id, sample_id).unwrap().unwrap();
            if !found_keyframe {
                if !sample.is_sync {
                    continue;
                }
                found_keyframe = true;
            }
            let mut payload = Vec::with_capacity(sample.bytes.len() + sps.len() + pps.len() + 8);
            if sample.is_sync {
                for parameter_set in [&sps, &pps] {
                    payload.extend_from_slice(
                        &u32::try_from(parameter_set.len()).unwrap().to_be_bytes(),
                    );
                    payload.extend_from_slice(parameter_set);
                }
            }
            payload.extend_from_slice(&sample.bytes);
            writer
                .append_one(crate::storage::RecordingFrame {
                    received_at: started_at + elapsed,
                    timestamp: Some(elapsed),
                    frame: crate::storage::MediaFrame::Video(crate::storage::VideoFrame {
                        codec: crate::storage::VideoCodec::H264,
                        is_keyframe: sample.is_sync,
                        width,
                        height,
                        data: payload.into(),
                    }),
                })
                .unwrap();
            let sample_nanos =
                u128::from(sample.duration).saturating_mul(1_000_000_000) / u128::from(timescale);
            elapsed = elapsed.saturating_add(Duration::from_nanos(
                u64::try_from(sample_nanos).unwrap_or(u64::MAX).max(1),
            ));
            if elapsed >= Duration::from_secs(2) {
                break;
            }
        }
        assert!(found_keyframe && elapsed >= Duration::from_millis(900));
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
        let event_store =
            EventStore::new(handle.clone(), &directory.join("event-thumbnails"), 0).unwrap();
        event_store
            .insert(TimelineEvent {
                id: "export-event".to_owned(),
                revision: 1,
                camera_id: "127.0.0.1".to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::KeepPeek,
                kind: "person".to_owned(),
                start_time_ms: start_ms,
                end_time_ms: Some(end_ms),
                confidence: Some(0.9),
                bbox: None,
                bbox_attachment_id: None,
                zone: None,
                text: None,
                payload: None,
                attachments: vec![EventAttachment {
                    id: "snapshot-hero".to_owned(),
                    attachment_type: "snapshot".to_owned(),
                    content_type: "image/jpeg".to_owned(),
                    byte_len: None,
                    ordinal: 0,
                    timestamp_ms: Some(start_ms),
                    text: None,
                }],
                canonical_attachment_id: Some("snapshot-hero".to_owned()),
                icon_key: "person".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: None,
            })
            .unwrap();
        let mut export_event = event_store.event_by_id("export-event").unwrap().unwrap();
        export_event.revision = 2;
        event_store.insert(export_event).unwrap();
        let mut state = media_test_state();
        state.catalog = Some(handle);
        state.events = Some(event_store);
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
                            event_seed: Some(proto::EventExportSeed {
                                event_id: "export-event".to_owned(),
                                revision: 2,
                                canonical_attachment: Some(proto::EventAttachmentDescriptor {
                                    attachment_id: "snapshot-hero".to_owned(),
                                    attachment_type: "snapshot".to_owned(),
                                    content_type: "image/jpeg".to_owned(),
                                    byte_len: None,
                                    ordinal: 0,
                                    timestamp: Some(millis_timestamp(start_ms)),
                                    text: None,
                                }),
                                icon_key: Some("person".to_owned()),
                                image_availability: proto::EventImageAvailability::Unavailable
                                    as i32,
                            }),
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
        let seed = created.event_seed.as_ref().unwrap();
        assert_eq!(seed.event_id, "export-event");
        assert_eq!(seed.revision, 2);
        assert_eq!(
            seed.canonical_attachment
                .as_ref()
                .map(|attachment| attachment.attachment_id.as_str()),
            Some("snapshot-hero")
        );
        assert_eq!(
            seed.image_availability,
            proto::EventImageAvailability::Unavailable as i32
        );
        let mut stale_request = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get("export-ready")
            .unwrap()
            .request
            .clone();
        stale_request.job_id = "export-stale-event".to_owned();
        stale_request.event_seed.as_mut().unwrap().revision = 1;
        let stale = create_export_job(&state, "local-administrator", stale_request).unwrap_err();
        assert_eq!(stale.code, proto::ErrorCode::Rejected);
        assert_eq!(stale.message, "export event revision is stale");
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
        assert!(mp4::read_mp4(File::open(&downloaded).unwrap()).is_ok());
        assert_independent_player_accepts(&downloaded);

        let artifact_path = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get("export-ready")
            .and_then(|record| record.path.clone())
            .unwrap();
        std::fs::write(&artifact_path, b"tampered export").unwrap();
        let tampered = handler.handle(proto::Request {
            request_id: 126,
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
        assert!(matches!(
            tampered.response.result,
            Some(control_response::Result::Error(proto::Error { code, .. }))
                if code == proto::ErrorCode::Unavailable as i32
        ));
        assert!(!artifact_path.exists());
        assert_eq!(
            state
                .export_jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get("export-ready")
                .unwrap()
                .job
                .status,
            proto::ExportJobStatus::Failed as i32
        );

        let partial = create_export_job(
            &state,
            "local-administrator",
            proto::CreateExportJob {
                job_id: "export-partial".to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                start_time: Some(millis_timestamp(start_ms)),
                end_time: Some(millis_timestamp(end_ms + 2_000)),
                allow_partial: false,
                burn_in_timestamp: false,
                event_seed: None,
            },
        )
        .unwrap();
        assert_eq!(partial.status, proto::ExportJobStatus::Partial as i32);
        assert_eq!(partial.missing_ranges.len(), 1);

        let failed = create_export_job(
            &state,
            "local-administrator",
            proto::CreateExportJob {
                job_id: "export-burn-in".to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                start_time: Some(millis_timestamp(start_ms)),
                end_time: Some(millis_timestamp(end_ms)),
                allow_partial: false,
                burn_in_timestamp: true,
                event_seed: None,
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
            "local-administrator",
            proto::CreateExportJob {
                job_id: "export-two-minutes".to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                start_time: Some(millis_timestamp(0)),
                end_time: Some(millis_timestamp(120_000)),
                allow_partial: false,
                burn_in_timestamp: false,
                event_seed: None,
            },
        )
        .unwrap();
        assert_eq!(allowed.status, proto::ExportJobStatus::Partial as i32);

        let error = create_export_job(
            &state,
            "local-administrator",
            proto::CreateExportJob {
                job_id: "export-too-long".to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                start_time: Some(millis_timestamp(0)),
                end_time: Some(millis_timestamp(120_001)),
                allow_partial: false,
                burn_in_timestamp: false,
                event_seed: None,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
        assert_eq!(error.message, "export range exceeds 2 minutes");

        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_active_duplicates_are_requester_scoped_and_option_exact() {
        let mut state = media_test_state();
        state.catalog = None;
        let request = proto::CreateExportJob {
            job_id: "active-export".to_owned(),
            source_id: "127.0.0.1".to_owned(),
            stream_id: "main".to_owned(),
            start_time: Some(millis_timestamp(1_000)),
            end_time: Some(millis_timestamp(2_000)),
            allow_partial: false,
            burn_in_timestamp: false,
            event_seed: None,
        };
        let now_ms = i64::try_from(unix_time_ms()).unwrap();
        state.export_jobs.lock().unwrap().insert(
            request.job_id.clone(),
            ExportJobRecord {
                requester_id: "owner".to_owned(),
                artifact_id: "active-attempt".to_owned(),
                request: request.clone(),
                job: proto::ExportJob {
                    job_id: request.job_id.clone(),
                    source_id: request.source_id.clone(),
                    stream_id: request.stream_id.clone(),
                    requested_start_time: request.start_time,
                    requested_end_time: request.end_time,
                    aligned_start_time: None,
                    status: proto::ExportJobStatus::Running as i32,
                    progress_per_mille: 100,
                    bytes_written: 0,
                    estimated_bytes: None,
                    file_name: None,
                    sha256: None,
                    expires_at: None,
                    missing_ranges: Vec::new(),
                    error: None,
                    retryable: false,
                    burn_in_timestamp: false,
                    event_seed: None,
                },
                path: None,
                cancel: Arc::new(AtomicBool::new(false)),
                created_at_ms: now_ms,
                started_at_ms: Some(now_ms),
                updated_at_ms: now_ms,
                completed_at_ms: None,
                downloaded_at_ms: None,
            },
        );

        let mut duplicate = request;
        duplicate.job_id = "new-client-id".to_owned();
        let reused = create_export_job(&state, "owner", duplicate.clone()).unwrap();
        assert_eq!(reused.job_id, "active-export");

        duplicate.allow_partial = true;
        let different_options = create_export_job(&state, "owner", duplicate.clone()).unwrap_err();
        assert_eq!(different_options.code, proto::ErrorCode::Unavailable);
        duplicate.allow_partial = false;
        let different_requester = create_export_job(&state, "other", duplicate).unwrap_err();
        assert_eq!(different_requester.code, proto::ErrorCode::Unavailable);
        assert_eq!(
            export_job(&state, "other", "active-export")
                .unwrap_err()
                .code,
            proto::ErrorCode::NotFound
        );
        assert_eq!(export_jobs(&state, "owner").len(), 1);
        assert!(export_jobs(&state, "other").is_empty());
        assert_eq!(
            download_export(
                &state,
                "other",
                proto::DownloadExport {
                    job_id: "active-export".to_owned(),
                    channel: proto::DataChannelKind::ReliableData as i32,
                },
            )
            .unwrap_err()
            .code,
            proto::ErrorCode::NotFound
        );
        assert!(validate_export_job_id("../recordings").is_err());
        assert!(validate_export_job_id("safe-export_1.2").is_ok());
    }

    #[test]
    fn export_monitor_terminalizes_stall_and_worker_panic() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-export-monitor-{}", rand::random::<u64>()));
        let mut state = media_test_state();
        state.storage_config.long_term_path = directory.clone();
        let now_ms = i64::try_from(unix_time_ms()).unwrap();
        let insert_running = |state: &ServerState, job_id: &str| {
            let request = proto::CreateExportJob {
                job_id: job_id.to_owned(),
                source_id: "127.0.0.1".to_owned(),
                stream_id: "main".to_owned(),
                start_time: Some(millis_timestamp(1_000)),
                end_time: Some(millis_timestamp(2_000)),
                allow_partial: false,
                burn_in_timestamp: false,
                event_seed: None,
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let artifact_id = Uuid::new_v4().simple().to_string();
            state.export_jobs.lock().unwrap().insert(
                job_id.to_owned(),
                ExportJobRecord {
                    requester_id: "owner".to_owned(),
                    artifact_id: artifact_id.clone(),
                    request: request.clone(),
                    job: proto::ExportJob {
                        job_id: job_id.to_owned(),
                        source_id: request.source_id,
                        stream_id: request.stream_id,
                        requested_start_time: request.start_time,
                        requested_end_time: request.end_time,
                        aligned_start_time: None,
                        status: proto::ExportJobStatus::Running as i32,
                        progress_per_mille: 100,
                        bytes_written: 0,
                        estimated_bytes: None,
                        file_name: None,
                        sha256: None,
                        expires_at: None,
                        missing_ranges: Vec::new(),
                        error: None,
                        retryable: false,
                        burn_in_timestamp: false,
                        event_seed: None,
                    },
                    path: None,
                    cancel: cancel.clone(),
                    created_at_ms: now_ms,
                    started_at_ms: Some(now_ms),
                    updated_at_ms: now_ms,
                    completed_at_ms: None,
                    downloaded_at_ms: None,
                },
            );
            (cancel, artifact_id)
        };

        let (stalled_cancel, stalled_artifact_id) = insert_running(&state, "stalled");
        let stalled_directory = export_attempt_directory(&state, "stalled", &stalled_artifact_id);
        std::fs::create_dir_all(&stalled_directory).unwrap();
        std::fs::write(stalled_directory.join("partial.active"), b"partial").unwrap();
        let (_sender, receiver) = mpsc::sync_channel(1);
        monitor_export_worker(
            &state,
            "stalled",
            &stalled_cancel,
            ExportArtifactTarget {
                path: &stalled_directory.join("final.mp4"),
                file_name: "final.mp4",
                artifact_id: &stalled_artifact_id,
            },
            receiver,
            ExportDeadlines {
                no_progress: Duration::from_millis(5),
                total_runtime: Duration::from_secs(1),
            },
        );
        let jobs = state.export_jobs.lock().unwrap();
        let stalled = jobs.get("stalled").unwrap();
        assert_eq!(stalled.job.status, proto::ExportJobStatus::Failed as i32);
        assert!(stalled.job.retryable);
        assert!(
            stalled
                .job
                .error
                .as_deref()
                .is_some_and(|error| error.contains("no progress"))
        );
        drop(jobs);
        assert!(!stalled_directory.exists());

        let (panic_cancel, panic_artifact_id) = insert_running(&state, "panic");
        let panic_directory = export_attempt_directory(&state, "panic", &panic_artifact_id);
        let (sender, receiver) = mpsc::sync_channel(1);
        let panicked = std::thread::spawn(move || {
            drop(sender);
            panic!("injected export worker panic");
        });
        assert!(panicked.join().is_err());
        monitor_export_worker(
            &state,
            "panic",
            &panic_cancel,
            ExportArtifactTarget {
                path: &panic_directory.join("final.mp4"),
                file_name: "final.mp4",
                artifact_id: &panic_artifact_id,
            },
            receiver,
            ExportDeadlines {
                no_progress: Duration::from_secs(1),
                total_runtime: Duration::from_secs(1),
            },
        );
        let jobs = state.export_jobs.lock().unwrap();
        let panicked = jobs.get("panic").unwrap();
        assert_eq!(panicked.job.status, proto::ExportJobStatus::Failed as i32);
        assert!(
            panicked
                .job
                .error
                .as_deref()
                .is_some_and(|error| error.contains("stopped unexpectedly"))
        );
        drop(jobs);

        let (total_cancel, total_artifact_id) = insert_running(&state, "total-runtime");
        let total_directory = export_attempt_directory(&state, "total-runtime", &total_artifact_id);
        let (sender, receiver) = mpsc::sync_channel(1);
        let heartbeat = std::thread::spawn(move || {
            while sender.send(ExportWorkerEvent::Heartbeat).is_ok() {
                std::thread::yield_now();
            }
        });
        monitor_export_worker(
            &state,
            "total-runtime",
            &total_cancel,
            ExportArtifactTarget {
                path: &total_directory.join("final.mp4"),
                file_name: "final.mp4",
                artifact_id: &total_artifact_id,
            },
            receiver,
            ExportDeadlines {
                no_progress: Duration::from_secs(1),
                total_runtime: Duration::from_millis(5),
            },
        );
        heartbeat.join().unwrap();
        let jobs = state.export_jobs.lock().unwrap();
        assert!(
            jobs.get("total-runtime")
                .unwrap()
                .job
                .error
                .as_deref()
                .is_some_and(|error| error.contains("runtime deadline"))
        );
        drop(jobs);

        let (disk_cancel, disk_artifact_id) = insert_running(&state, "disk-full");
        let disk_directory = export_attempt_directory(&state, "disk-full", &disk_artifact_id);
        let disk_path = disk_directory.join("final.mp4");
        std::fs::create_dir_all(&disk_directory).unwrap();
        std::fs::write(disk_directory.join("partial.active"), b"partial").unwrap();
        finish_export_worker(
            &state,
            "disk-full",
            &disk_cancel,
            ExportArtifactTarget {
                path: &disk_path,
                file_name: "final.mp4",
                artifact_id: &disk_artifact_id,
            },
            Err(anyhow::anyhow!("No space left on device")),
        );
        let jobs = state.export_jobs.lock().unwrap();
        let disk_full = jobs.get("disk-full").unwrap();
        assert_eq!(disk_full.job.status, proto::ExportJobStatus::Failed as i32);
        assert!(
            disk_full
                .job
                .error
                .as_deref()
                .is_some_and(|error| error.contains("No space left"))
        );
        drop(jobs);
        assert!(!disk_directory.exists());

        let (stale_cancel, stale_artifact_id) = insert_running(&state, "retry-race");
        stale_cancel.store(true, Ordering::Release);
        let (retry_cancel, retry_artifact_id) = insert_running(&state, "retry-race");
        let stale_directory = export_attempt_directory(&state, "retry-race", &stale_artifact_id);
        let retry_directory = export_attempt_directory(&state, "retry-race", &retry_artifact_id);
        let retry_artifact = retry_directory.join("new-worker.active");
        std::fs::create_dir_all(&retry_directory).unwrap();
        std::fs::write(&retry_artifact, b"new worker").unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        monitor_export_worker(
            &state,
            "retry-race",
            &stale_cancel,
            ExportArtifactTarget {
                path: &stale_directory.join("final.mp4"),
                file_name: "final.mp4",
                artifact_id: &stale_artifact_id,
            },
            receiver,
            ExportDeadlines {
                no_progress: Duration::from_secs(1),
                total_runtime: Duration::from_secs(1),
            },
        );
        assert!(retry_artifact.exists());
        assert!(Arc::ptr_eq(
            &state
                .export_jobs
                .lock()
                .unwrap()
                .get("retry-race")
                .unwrap()
                .cancel,
            &retry_cancel
        ));

        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn export_cleanup_expires_artifacts_then_prunes_metadata() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-export-expiry-{}", rand::random::<u64>()));
        let mut state = media_test_state();
        state.storage_config.long_term_path = directory.clone();
        let job_id = "expired-export";
        let artifact_id = "expiry-attempt";
        let artifact = export_attempt_directory(&state, job_id, artifact_id).join("evidence.mp4");
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, b"evidence").unwrap();
        let now_ms = i64::try_from(unix_time_ms()).unwrap();
        let request = proto::CreateExportJob {
            job_id: job_id.to_owned(),
            source_id: "127.0.0.1".to_owned(),
            stream_id: "main".to_owned(),
            start_time: Some(millis_timestamp(1_000)),
            end_time: Some(millis_timestamp(2_000)),
            allow_partial: false,
            burn_in_timestamp: false,
            event_seed: None,
        };
        state.export_jobs.lock().unwrap().insert(
            job_id.to_owned(),
            ExportJobRecord {
                requester_id: "owner".to_owned(),
                artifact_id: artifact_id.to_owned(),
                request: request.clone(),
                job: proto::ExportJob {
                    job_id: job_id.to_owned(),
                    source_id: request.source_id,
                    stream_id: request.stream_id,
                    requested_start_time: request.start_time,
                    requested_end_time: request.end_time,
                    aligned_start_time: Some(millis_timestamp(1_000)),
                    status: proto::ExportJobStatus::Ready as i32,
                    progress_per_mille: 1_000,
                    bytes_written: 8,
                    estimated_bytes: Some(8),
                    file_name: Some("evidence.mp4".to_owned()),
                    sha256: Some(encode_lower_hex(Sha256::digest(b"evidence"))),
                    expires_at: Some(millis_timestamp(now_ms.saturating_sub(1))),
                    missing_ranges: Vec::new(),
                    error: None,
                    retryable: false,
                    burn_in_timestamp: false,
                    event_seed: None,
                },
                path: Some(artifact.clone()),
                cancel: Arc::new(AtomicBool::new(false)),
                created_at_ms: now_ms,
                started_at_ms: Some(now_ms),
                updated_at_ms: now_ms,
                completed_at_ms: Some(now_ms),
                downloaded_at_ms: None,
            },
        );

        cleanup_expired_exports(&state);
        assert!(!artifact.exists());
        let mut jobs = state.export_jobs.lock().unwrap();
        let expired = jobs.get_mut(job_id).unwrap();
        assert_eq!(expired.job.status, proto::ExportJobStatus::Expired as i32);
        expired.updated_at_ms = now_ms.saturating_sub(
            i64::try_from(EXPORT_METADATA_RETENTION.as_millis()).unwrap_or(i64::MAX) + 1,
        );
        drop(jobs);

        cleanup_expired_exports(&state);
        assert!(!state.export_jobs.lock().unwrap().contains_key(job_id));
        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn export_checksum_honors_cancellation() {
        let path = std::env::temp_dir().join(format!(
            "keeppeek-export-checksum-{}",
            rand::random::<u64>()
        ));
        std::fs::write(&path, vec![1u8; 128 * 1_024]).unwrap();
        let cancelled = AtomicBool::new(true);

        let error = sha256_file_with_progress(&path, &cancelled, 128 * 1_024, |_| {}).unwrap_err();

        assert!(error.to_string().contains("cancelled"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn export_history_recovers_ready_and_interrupted_jobs() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-export-history-{}", rand::random::<u64>()));
        let history_path = directory.join(EXPORT_HISTORY_FILE);
        let ready_path = directory
            .join("ready-job")
            .join("attempt")
            .join("ready.mp4");
        std::fs::create_dir_all(ready_path.parent().unwrap()).unwrap();
        std::fs::write(&ready_path, b"ready export").unwrap();
        let now_ms = i64::try_from(unix_time_ms()).unwrap();
        let request = |job_id: &str| proto::CreateExportJob {
            job_id: job_id.to_owned(),
            source_id: "front-door".to_owned(),
            stream_id: "main".to_owned(),
            start_time: Some(millis_timestamp(1_000)),
            end_time: Some(millis_timestamp(2_000)),
            allow_partial: false,
            burn_in_timestamp: false,
            event_seed: None,
        };
        let job = |job_id: &str, status: proto::ExportJobStatus, file_name: Option<&str>| {
            proto::ExportJob {
                job_id: job_id.to_owned(),
                source_id: "front-door".to_owned(),
                stream_id: "main".to_owned(),
                requested_start_time: Some(millis_timestamp(1_000)),
                requested_end_time: Some(millis_timestamp(2_000)),
                aligned_start_time: Some(millis_timestamp(1_000)),
                status: status as i32,
                progress_per_mille: 500,
                bytes_written: 12,
                estimated_bytes: Some(12),
                file_name: file_name.map(str::to_owned),
                sha256: Some("checksum".to_owned()),
                expires_at: Some(millis_timestamp(now_ms.saturating_add(60_000))),
                missing_ranges: Vec::new(),
                error: None,
                retryable: false,
                burn_in_timestamp: false,
                event_seed: None,
            }
        };
        let record =
            |request: proto::CreateExportJob, job: proto::ExportJob, path| ExportJobRecord {
                requester_id: "local-administrator".to_owned(),
                artifact_id: "attempt".to_owned(),
                request,
                job,
                path,
                cancel: Arc::new(AtomicBool::new(false)),
                created_at_ms: now_ms,
                started_at_ms: Some(now_ms),
                updated_at_ms: now_ms,
                completed_at_ms: None,
                downloaded_at_ms: None,
            };
        let mut jobs = HashMap::new();
        jobs.insert(
            "ready-job".to_owned(),
            record(
                request("ready-job"),
                job(
                    "ready-job",
                    proto::ExportJobStatus::Ready,
                    Some("ready.mp4"),
                ),
                Some(ready_path.clone()),
            ),
        );
        jobs.insert(
            "running-job".to_owned(),
            record(
                request("running-job"),
                job("running-job", proto::ExportJobStatus::Running, None),
                None,
            ),
        );
        jobs.insert(
            "missing-ready".to_owned(),
            record(
                request("missing-ready"),
                job(
                    "missing-ready",
                    proto::ExportJobStatus::Ready,
                    Some("missing.mp4"),
                ),
                None,
            ),
        );
        let interrupted_path = directory
            .join("running-job")
            .join("attempt")
            .join("partial.mp4.active");
        std::fs::create_dir_all(interrupted_path.parent().unwrap()).unwrap();
        std::fs::write(&interrupted_path, b"partial").unwrap();

        persist_export_jobs(&history_path, &jobs).unwrap();
        let recovered = load_export_jobs(&history_path).unwrap();

        let ready = recovered.get("ready-job").unwrap();
        assert_eq!(ready.job.status, proto::ExportJobStatus::Ready as i32);
        assert_eq!(ready.path.as_deref(), Some(ready_path.as_path()));
        let interrupted = recovered.get("running-job").unwrap();
        assert_eq!(
            interrupted.job.status,
            proto::ExportJobStatus::Failed as i32
        );
        assert!(interrupted.job.retryable);
        assert!(
            interrupted
                .job
                .error
                .as_deref()
                .is_some_and(|error| error.contains("restarted"))
        );
        assert!(interrupted.completed_at_ms.is_some());
        assert!(!interrupted_path.exists());
        let missing = recovered.get("missing-ready").unwrap();
        assert_eq!(missing.job.status, proto::ExportJobStatus::Failed as i32);
        assert!(missing.job.retryable);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_history_rejects_oversized_input_before_reading() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-export-history-limit-{}",
            rand::random::<u64>()
        ));
        let history_path = directory.join(EXPORT_HISTORY_FILE);
        std::fs::create_dir_all(&directory).unwrap();
        let file = File::create(&history_path).unwrap();
        file.set_len(MAX_EXPORT_HISTORY_BYTES + 1).unwrap();

        let error = match load_export_jobs(&history_path) {
            Ok(_) => panic!("oversized export history must be rejected"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("export history exceeds"));
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
        let state = ServerState::empty()
            .with_access_manager(AccessManager::open(&config_path, access_key).unwrap())
            .with_camera_config_path(config_path.clone());
        *state.access_key.write().unwrap() = access_key;
        let loopback_session = SessionId::from_u64(81);
        let remote_session = SessionId::from_u64(82);
        let authorization = format!("Bearer {}", access_key.canonical());
        let remote_credential = state
            .access_manager
            .authenticate(
                "203.0.113.7".parse().unwrap(),
                &[&authorization],
                i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
                Instant::now(),
            )
            .unwrap();
        state.api_session_owners.lock().unwrap().extend([
            (loopback_session, local_test_session()),
            (
                remote_session,
                test_session_record(
                    ApiPrincipal::credential(remote_credential),
                    "203.0.113.7".parse().unwrap(),
                    ClientClassificationReason::DirectRemote,
                ),
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
        assert!(
            state
                .api_session_owners
                .lock()
                .unwrap()
                .get(&remote_session)
                .is_none()
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
            .insert(loopback_session, local_test_session());
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
                                expected_configuration_revision: String::new(),
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
                                expected_configuration_revision: String::new(),
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
                                expected_configuration_revision: String::new(),
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
                                expected_configuration_revision: String::new(),
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
    fn runtime_configuration_distinguishes_omitted_safety_fields_from_zero() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-runtime-optional-storage-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        let recordings = directory.join("recordings");
        let config = Config {
            storage: StorageToml {
                medium_term_path: Some(recordings.to_string_lossy().into_owned()),
                long_term_path: Some(recordings.to_string_lossy().into_owned()),
                long_term_max_gb: 24,
                minimum_free_gb: 8,
                maximum_used_percent: Some(85),
                warning_free_gb: 12,
                critical_free_gb: 8,
                cleanup_hysteresis_gb: 2,
                ..StorageToml::default()
            },
            ..Config::default()
        };
        crate::config::write_private_file(
            &config_path,
            toml::to_string_pretty(&config).unwrap().as_bytes(),
        )
        .unwrap();
        let storage_config = StorageConfig::from_toml(&config.storage);
        let state = ServerState::new(
            &config,
            &HashMap::new(),
            &HashMap::new(),
            &storage_config,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        )
        .with_camera_config_path(config_path);
        let handler = test_control_handler(state);
        let storage_update = |safety: Option<u64>| proto::RuntimeStorageConfiguration {
            medium_term_path: recordings.to_string_lossy().into_owned(),
            long_term_path: recordings.to_string_lossy().into_owned(),
            recording_catalog_path: recordings
                .join("recordings.db")
                .to_string_lossy()
                .into_owned(),
            event_thumbnail_path: recordings
                .join(".event-thumbnails")
                .to_string_lossy()
                .into_owned(),
            event_thumbnail_max_mb: 1_024,
            short_term_secs: 120,
            medium_term_secs: 1_800,
            flush_interval_secs: 60,
            write_buffer_bytes: 8_192,
            long_term_max_gb: 24,
            minimum_free_gb: safety,
            maximum_used_percent: safety.map(|_| 0),
            warning_free_gb: safety,
            critical_free_gb: safety,
            cleanup_hysteresis_gb: safety,
        };
        let update = |request_id, revision: String, storage| proto::Request {
            request_id,
            command: Some(control_request::Command::RuntimeConfigurationCommand(
                proto::RuntimeConfigurationCommand {
                    action: Some(runtime_configuration_command::Action::Update(
                        proto::UpdateRuntimeConfiguration {
                            host: "0.0.0.0".to_owned(),
                            port: 8081,
                            storage: Some(storage),
                            move_existing_recordings: false,
                            expected_configuration_revision: revision,
                        },
                    )),
                },
            )),
        };

        let preserved = handler.handle(update(86, String::new(), storage_update(None)));
        let Some(control_response::Result::Ok(ok)) = preserved.response.result else {
            panic!("legacy runtime update must succeed");
        };
        let Some(control_ok::Result::RuntimeConfigurationResult(result)) = ok.result else {
            panic!("legacy runtime update must return configuration");
        };
        let preserved = result.config.unwrap();
        let preserved_storage = preserved.storage.unwrap();
        assert_eq!(preserved_storage.minimum_free_gb, Some(8));
        assert_eq!(preserved_storage.maximum_used_percent, Some(85));
        assert_eq!(preserved_storage.warning_free_gb, Some(12));
        assert_eq!(preserved_storage.critical_free_gb, Some(8));
        assert_eq!(preserved_storage.cleanup_hysteresis_gb, Some(2));

        let disabled = handler.handle(update(
            87,
            preserved.configuration_revision,
            storage_update(Some(0)),
        ));
        let Some(control_response::Result::Ok(ok)) = disabled.response.result else {
            panic!("explicit zero runtime update must succeed");
        };
        let Some(control_ok::Result::RuntimeConfigurationResult(result)) = ok.result else {
            panic!("explicit zero runtime update must return configuration");
        };
        let disabled_storage = result.config.unwrap().storage.unwrap();
        assert_eq!(disabled_storage.minimum_free_gb, Some(0));
        assert_eq!(disabled_storage.maximum_used_percent, Some(0));
        assert_eq!(disabled_storage.warning_free_gb, Some(0));
        assert_eq!(disabled_storage.critical_free_gb, Some(0));
        assert_eq!(disabled_storage.cleanup_hysteresis_gb, Some(0));

        std::fs::remove_dir_all(directory).unwrap();
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
    fn config_export_http_returns_only_the_two_toml_files_without_retention() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-config-export-http-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let config_path = directory.join("config.toml");
        let config = "[storage]\nlong_term_max_gb = 10\n";
        let secrets = "CAMERA_PASSWORD = \"private\"\n";
        std::fs::write(&config_path, config).unwrap();
        std::fs::write(crate::config::secrets_path(&config_path), secrets).unwrap();
        std::fs::write(directory.join("recordings.db"), "not configuration").unwrap();
        let state =
            ServerState::empty().with_backup_manager(BackupManager::open(config_path).unwrap());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        let response = handle_request(
            &Request::fake_http("GET", "/config/export", Vec::new(), Vec::new()),
            &router_tx,
            &state,
        );

        assert_eq!(response.status_code, 200);
        assert!(response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Content-Type") && value == "application/zip"
        }));
        let mut archive = zip::ZipArchive::new(Cursor::new(response_data(response))).unwrap();
        assert_eq!(archive.len(), 2);
        let mut archived_config = String::new();
        archive
            .by_name("config.toml")
            .unwrap()
            .read_to_string(&mut archived_config)
            .unwrap();
        let mut archived_secrets = String::new();
        archive
            .by_name("secrets.toml")
            .unwrap()
            .read_to_string(&mut archived_secrets)
            .unwrap();
        assert_eq!(archived_config, config);
        assert_eq!(archived_secrets, secrets);
        assert!(archive.by_name("recordings.db").is_err());
        assert!(
            state
                .backup_manager
                .as_ref()
                .unwrap()
                .list()
                .unwrap()
                .backups
                .is_empty()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn config_apply_http_stages_both_toml_files_and_preserves_recording_storage() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-config-apply-http-{}",
            uuid::Uuid::new_v4()
        ));
        let source = directory.join("source");
        let target = directory.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let source_config = source.join("config.toml");
        let target_config = target.join("config.toml");
        let target_recordings = target.join(r"recordings\archive");
        let source_contents = format!(
            "host = \"source.example\"\n[storage]\nlong_term_path = {:?}\n",
            source.join("recordings")
        );
        let target_contents = format!(
            "host = \"target.example\"\n[storage]\nlong_term_path = {target_recordings:?}\n"
        );
        std::fs::write(&source_config, source_contents).unwrap();
        std::fs::write(source.join("secrets.toml"), "TOKEN = \"source-secret\"\n").unwrap();
        std::fs::write(&target_config, &target_contents).unwrap();
        std::fs::write(target.join("secrets.toml"), "TOKEN = \"target-secret\"\n").unwrap();
        std::fs::write(target.join("recordings.db"), "catalog-must-remain").unwrap();
        let (bundle, _) = crate::backup::create_bundle(
            Cursor::new(Vec::new()),
            crate::backup::CreateBundleOptions {
                config_path: &source_config,
                sections: &[],
                created_at_unix_ms: 1_788_000_000_000,
            },
        )
        .unwrap();
        let bytes = bundle.into_inner();
        let state = ServerState::empty()
            .with_backup_manager(BackupManager::open(target_config.clone()).unwrap());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        let response = handle_request(
            &Request::fake_http(
                "POST",
                "/config/apply",
                vec![
                    ("Content-Type".to_owned(), "application/zip".to_owned()),
                    ("Content-Length".to_owned(), bytes.len().to_string()),
                ],
                bytes,
            ),
            &router_tx,
            &state,
        );

        assert_eq!(response.status_code, 202);
        let staged: crate::api::backup_proto::RestoreRecord =
            serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(
            staged.state,
            crate::api::backup_proto::RestoreState::AwaitingRestart as i32
        );
        assert_eq!(
            std::fs::read_to_string(&target_config).unwrap(),
            target_contents
        );
        assert!(
            state
                .backup_manager
                .as_ref()
                .unwrap()
                .list()
                .unwrap()
                .backups
                .is_empty()
        );

        crate::backup::recover_pending_restore(&target_config, 1_788_000_000_001).unwrap();
        let applied = std::fs::read_to_string(&target_config).unwrap();
        let applied: toml::Value = toml::from_str(&applied).unwrap();
        assert_eq!(applied["host"].as_str().unwrap(), "source.example");
        assert_eq!(
            applied["storage"]["long_term_path"].as_str().unwrap(),
            target_recordings.to_str().unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(target.join("secrets.toml")).unwrap(),
            "TOKEN = \"source-secret\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("recordings.db")).unwrap(),
            "catalog-must-remain"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_backup_http_routes_are_not_exposed() {
        let state = ServerState::empty();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        for (method, path) in [
            ("GET", "/api/backups"),
            ("POST", "/api/backups"),
            ("GET", "/api/backups/capabilities"),
            ("POST", "/api/backups/uploads"),
            ("POST", "/api/backups/restores"),
        ] {
            let response = handle_request(
                &Request::fake_http(method, path, Vec::new(), Vec::new()),
                &router_tx,
                &state,
            );
            assert_eq!(response.status_code, 404, "{method} {path}");
        }
    }

    #[test]
    fn config_http_rejects_invalid_archives_without_mutating_live_files() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-config-http-invalid-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        let original_config = "host = \"localhost\"\n";
        let original_secrets = "TOKEN = \"original\"\n";
        std::fs::write(&config_path, original_config).unwrap();
        std::fs::write(directory.join("secrets.toml"), original_secrets).unwrap();
        let state = ServerState::empty()
            .with_backup_manager(BackupManager::open(config_path.clone()).unwrap());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let bytes = b"provider-token at /private/path is not a ZIP";

        let response = handle_request(
            &Request::fake_http(
                "POST",
                "/config/apply",
                vec![
                    ("Content-Type".to_owned(), "application/zip".to_owned()),
                    ("Content-Length".to_owned(), bytes.len().to_string()),
                ],
                bytes.to_vec(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(response.status_code, 400);
        let error: crate::api::backup_proto::BackupError =
            serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(
            error.code,
            crate::api::backup_proto::BackupErrorCode::InvalidRequest as i32
        );
        assert_eq!(error.field, "archive");
        assert!(!error.message.contains("provider-token"));
        assert!(!error.message.contains("/private/path"));
        assert_eq!(
            std::fs::read_to_string(config_path).unwrap(),
            original_config
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("secrets.toml")).unwrap(),
            original_secrets
        );
        assert_eq!(
            std::fs::read_dir(directory.join(".backups"))
                .unwrap()
                .count(),
            0
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn config_http_rejects_empty_oversized_and_truncated_uploads() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-config-http-length-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "host = \"localhost\"\n").unwrap();
        let state =
            ServerState::empty().with_backup_manager(BackupManager::open(config_path).unwrap());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        for (length, expected_status) in [(0, 400), (1_073_741_825, 413), (10, 400), (1, 400)] {
            let response = handle_request(
                &Request::fake_http(
                    "POST",
                    "/config/apply",
                    vec![
                        ("Content-Type".to_owned(), "application/zip".to_owned()),
                        ("Content-Length".to_owned(), length.to_string()),
                    ],
                    b"no".to_vec(),
                ),
                &router_tx,
                &state,
            );
            assert_eq!(response.status_code, expected_status, "length {length}");
        }
        assert_eq!(
            std::fs::read_dir(directory.join(".backups"))
                .unwrap()
                .count(),
            0
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn config_http_rejects_remote_user_role() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-backup-http-user-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        let mut state =
            ServerState::empty().with_backup_manager(BackupManager::open(config_path).unwrap());
        state.require_secure_remote = false;
        let issued = state
            .access_manager
            .create_credential("Viewer", None, AccessRole::User, None, 1_000)
            .unwrap();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let remote = SocketAddr::from(([203, 0, 113, 8], 42_000));
        for (method, path) in [("GET", "/config/export"), ("POST", "/config/apply")] {
            let response = handle_request(
                &Request::fake_http_from(
                    remote,
                    method,
                    path,
                    vec![(
                        "Authorization".to_owned(),
                        format!("Bearer {}", issued.access_key.canonical()),
                    )],
                    Vec::new(),
                ),
                &router_tx,
                &state,
            );
            assert_eq!(response.status_code, 403, "{method} {path}");
        }
        assert!(
            state
                .backup_manager
                .as_ref()
                .unwrap()
                .list()
                .unwrap()
                .backups
                .is_empty()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn config_http_preflight_advertises_one_complete_method_set() {
        let state = ServerState::empty();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let response = handle_request(
            &Request::fake_http(
                "OPTIONS",
                "/config/apply",
                vec![("Origin".to_owned(), "http://localhost".to_owned())],
                Vec::new(),
            ),
            &router_tx,
            &state,
        );

        let methods = response
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("Access-Control-Allow-Methods"))
            .map(|(_, value)| value.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(methods, ["GET, POST, OPTIONS"]);
    }

    #[test]
    fn config_http_audits_rejected_zip_requests() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-backup-http-audit-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        let state =
            ServerState::empty().with_backup_manager(BackupManager::open(config_path).unwrap());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        let response = handle_request(
            &Request::fake_http(
                "POST",
                "/config/apply",
                vec![("Content-Type".to_owned(), "text/plain".to_owned())],
                b"not a ZIP".to_vec(),
            ),
            &router_tx,
            &state,
        );

        assert_eq!(response.status_code, 415);
        assert!(
            state
                .access_manager
                .list_audit(10)
                .iter()
                .any(|event| { event.action == "config_apply" && event.result == "failed" })
        );
        let response = handle_request(
            &Request::fake_http(
                "POST",
                "/config/apply",
                vec![("Content-Type".to_owned(), "application/zip".to_owned())],
                b"not a ZIP".to_vec(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(response.status_code, 411);
        assert!(
            state
                .access_manager
                .list_audit(10)
                .iter()
                .any(|event| { event.action == "config_apply" && event.result == "failed" })
        );
        let metrics = state
            .backup_manager
            .as_ref()
            .unwrap()
            .metric_snapshot(1_000)
            .unwrap();
        assert_eq!(metrics.operation_successes, 0);
        assert_eq!(metrics.operation_failures, 2);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn config_http_errors_are_typed_and_hide_internal_details() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-backup-http-errors-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        let state =
            ServerState::empty().with_backup_manager(BackupManager::open(config_path).unwrap());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let response = handle_request(
            &Request::fake_http(
                "POST",
                "/config/apply",
                vec![("Content-Type".to_owned(), "text/plain".to_owned())],
                b"private/path/token".to_vec(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(response.status_code, 415);
        let error: crate::api::backup_proto::BackupError =
            serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(
            error.code,
            crate::api::backup_proto::BackupErrorCode::InvalidRequest as i32
        );
        assert!(error.field.is_empty());
        assert!(!error.message.contains("private"));

        let response = backup_error_from_anyhow(anyhow::anyhow!(
            "database /private/path contains provider-token"
        ));
        assert_eq!(response.status_code, 500);
        let body = String::from_utf8(response_data(response)).unwrap();
        assert!(!body.contains("/private/path"));
        assert!(!body.contains("provider-token"));
        std::fs::remove_dir_all(directory).unwrap();
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
        assert!(storage.minimum_free_bytes > 0);
        let safety = storage
            .safety
            .expect("health must include storage safety evidence");
        assert_eq!(safety.pressure, "normal");
        assert_eq!(safety.recording_state, "active");
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
        assert!(body.contains("keeppeek_storage_minimum_free_bytes"));
        assert!(body.contains("keeppeek_storage_pressure_state 0"));
        assert!(body.contains("keeppeek_storage_recording_paused 0"));
        assert!(body.contains("keeppeek_storage_last_cleanup_files_removed 0"));
        assert!(body.contains("keeppeek_external_analysis_sessions_active 0"));
        assert!(body.contains("keeppeek_external_analysis_media_subscriptions_active 0"));
        assert!(body.contains("keeppeek_external_analysis_event_subscriptions_active 0"));
        assert!(body.contains("keeppeek_external_analysis_event_delivery_queue_depth 0"));
        assert!(body.contains("keeppeek_external_analysis_event_delivery_pending_bytes 0"));
        assert!(body.contains("keeppeek_external_analysis_event_delivery_drops_total 0"));
        assert!(body.contains("keeppeek_external_analysis_event_publications_active 0"));
        assert!(body.contains("keeppeek_external_analysis_event_publication_staged_bytes 0"));
        assert!(body.contains("keeppeek_external_analysis_event_publication_starts_total 0"));
        assert!(body.contains("keeppeek_external_analysis_event_publication_commits_total 0"));
        assert!(body.contains("keeppeek_external_analysis_event_publication_aborts_total 0"));
        assert!(body.contains("keeppeek_external_analysis_event_publication_expirations_total 0"));
        assert!(body.contains("keeppeek_external_analysis_event_publication_rejections_total 0"));
        assert!(
            body.contains("keeppeek_external_analysis_event_publication_storage_failures_total 0")
        );
        assert!(body.contains(
            "keeppeek_external_analysis_event_publication_commit_latency_milliseconds{quantile=\"p50\"} 0"
        ));
        assert!(body.contains(
            "keeppeek_external_analysis_event_publication_commit_latency_milliseconds{quantile=\"p95\"} 0"
        ));
        assert_eq!(router_thread.join().unwrap(), 1);
    }

    #[test]
    fn recording_coverage_route_pages_one_catalog_revision() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-recording-coverage-route-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let catalog =
            crate::storage::RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let catalog_handle = catalog.handle();
        let camera = |ip: &str, name: &str| CameraConfig {
            ip: ip.parse().unwrap(),
            name: Some(name.to_owned()),
            display_name: Some(name.replace('-', " ")),
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
            recording_mode: CameraRecordingMode::Main,
            event_recording_duration_secs: 60,
        };
        let camera_configs = HashMap::from([
            (
                "Exterior".to_owned(),
                vec![camera("192.0.2.10", "front-door")],
            ),
            (
                "Interior".to_owned(),
                vec![camera("192.0.2.11", "workshop")],
            ),
        ]);
        let state = ServerState::new(
            &Config::default(),
            &camera_configs,
            &HashMap::new(),
            &StorageConfig::default(),
            RecordingDemand::new(TEST_RECORDING_DEMAND_GRACE),
            WebRtc::new(),
        )
        .with_recording_catalog(catalog_handle.clone());
        let end_ms = unix_time_ms();
        let start_ms = end_ms.saturating_sub(60_000);
        let request = Request::fake_http(
            "GET",
            format!("/recording-coverage?start_ms={start_ms}&end_ms={end_ms}&page_size=1"),
            Vec::new(),
            Vec::new(),
        );
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        assert_eq!(router_thread.join().unwrap(), 1);
        let first: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(first["cameras"].as_array().unwrap().len(), 1);
        assert_eq!(first["groups"], serde_json::json!(["Exterior", "Interior"]));
        let first_camera_id = first["cameras"][0]["camera_id"].as_str().unwrap();
        assert_eq!(first["cameras"][0]["streams"][0]["coverage_percent"], 0.0);
        let token = first["next_page_token"].as_str().unwrap();

        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let metrics = handle_request(
            &Request::fake_http("GET", "/metrics", Vec::new(), Vec::new()),
            &router_tx,
            &state,
        );
        assert_eq!(metrics.status_code, 200);
        assert_eq!(router_thread.join().unwrap(), 1);
        let metrics = String::from_utf8(response_data(metrics)).unwrap();
        assert!(metrics.contains("keeppeek_recording_coverage_snapshot_available 1"));
        assert!(metrics.contains(&format!(
            "keeppeek_recording_coverage_ratio{{camera_id=\"{first_camera_id}\""
        )));
        assert!(metrics.contains("stream=\"main\"} 0"));

        let request = Request::fake_http(
            "GET",
            format!("/recording-coverage?page_token={token}"),
            Vec::new(),
            Vec::new(),
        );
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 200);
        assert_eq!(router_thread.join().unwrap(), 1);
        let second: serde_json::Value = serde_json::from_slice(&response_data(response)).unwrap();
        assert_eq!(second["cameras"].as_array().unwrap().len(), 1);
        assert!(second["next_page_token"].is_null());

        catalog_handle
            .upsert_recording(crate::storage::CatalogRecording {
                id: "revision-change".to_owned(),
                stream_id: "front-door/main".to_owned(),
                source_id: Some("192.0.2.10".to_owned()),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: i64::try_from(start_ms).unwrap_or(i64::MAX),
                ended_at_ms: None,
                path: directory
                    .join("revision-change.mp4.active")
                    .to_string_lossy()
                    .into_owned(),
                init_offset: 0,
                init_len: 8,
                finalized: false,
            })
            .unwrap();
        let request = Request::fake_http(
            "GET",
            format!("/recording-coverage?page_token={token}"),
            Vec::new(),
            Vec::new(),
        );
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });
        let response = handle_request(&request, &router_tx, &state);
        assert_eq!(response.status_code, 409);
        assert_eq!(router_thread.join().unwrap(), 1);

        drop(state);
        drop(catalog_handle);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
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
            .initial_capabilities(SessionId::from_u64(0))
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
            .initial_capabilities(SessionId::from_u64(0))
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
            groups: vec!["cameras".to_owned()],
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
    fn camera_entries_preserve_configuration_groups() {
        let front = CameraConfig {
            ip: "192.0.2.10".parse().unwrap(),
            name: Some("front".to_owned()),
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
            recording_mode: CameraRecordingMode::Main,
            event_recording_duration_secs: 60,
        };
        let camera_configs = HashMap::from([
            ("Exterior".to_owned(), vec![front.clone()]),
            ("Entrances".to_owned(), vec![front]),
        ]);
        let state = ServerState::new(
            &Config::default(),
            &camera_configs,
            &HashMap::new(),
            &StorageConfig::default(),
            RecordingDemand::new(TEST_RECORDING_DEMAND_GRACE),
            WebRtc::new(),
        );

        assert_eq!(
            state.camera_entries()[0].groups,
            vec!["Entrances".to_owned(), "Exterior".to_owned()]
        );
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

        delete_camera_settings(&state, "192.0.2.77", "").unwrap();
        assert!(
            crate::config::load_cameras(&config_path)
                .unwrap()
                .get("cameras")
                .is_none_or(Vec::is_empty)
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn camera_settings_reject_stale_revision_without_overwriting_current_configuration() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-settings-camera-conflict-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        crate::config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "operator"
                password = "password"

                [cameras.front]
                ip = "192.0.2.77"
                display_name = "Front"
            "#,
        )
        .unwrap();
        let edit_start = configuration_revision(&crate::config::load_config(&config_path).unwrap());
        let mut current = crate::config::load_configuration_table(&config_path).unwrap();
        current.insert(
            "future_server_setting".to_owned(),
            toml::Value::String("keep-current".to_owned()),
        );
        crate::config::write_configuration_table(&config_path, &current).unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        let Err(error) = save_camera_settings(
            CameraSettingsUpdate {
                expected_configuration_revision: edit_start.clone(),
                display_name: Some(Some("Stale name".to_owned())),
                ..CameraSettingsUpdate::default()
            },
            &router_tx,
            &state,
            "192.0.2.77",
        ) else {
            panic!("a stale camera update must fail");
        };

        assert_eq!(error.code, proto::ErrorCode::Rejected);
        let detail = proto::ConfigurationError::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.code, proto::ConfigurationErrorCode::Conflict as i32);
        assert_ne!(detail.current_configuration_revision, edit_start);
        let saved = crate::config::load_configuration_table(&config_path).unwrap();
        assert_eq!(
            saved["future_server_setting"].as_str(),
            Some("keep-current")
        );
        assert_eq!(
            saved["cameras"]["front"]["display_name"].as_str(),
            Some("Front")
        );

        let Err(remove_error) = delete_camera_settings(&state, "192.0.2.77", &edit_start) else {
            panic!("a stale camera removal must fail");
        };
        let remove_detail =
            proto::ConfigurationError::decode(remove_error.details[0].value.as_slice()).unwrap();
        assert_eq!(
            remove_detail.code,
            proto::ConfigurationErrorCode::Conflict as i32
        );
        assert!(
            crate::config::load_configuration_table(&config_path).unwrap()["cameras"]["front"]
                .is_table()
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn camera_edit_uses_persisted_defaults_when_live_configuration_is_stale() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-settings-camera-stale-runtime-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        crate::config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "operator"
                password = "password"

                [cameras.front]
                ip = "192.0.2.77"
            "#,
        )
        .unwrap();
        let config = crate::config::load_config(&config_path).unwrap();
        let configured = crate::config::load_cameras(&config_path).unwrap();
        let state = ServerState::new(
            &config,
            &configured,
            &HashMap::new(),
            &StorageConfig::default(),
            RecordingDemand::new(TEST_RECORDING_DEMAND_GRACE),
            WebRtc::new(),
        )
        .with_camera_config_path(config_path.clone());
        let mut current = crate::config::load_configuration_table(&config_path).unwrap();
        current
            .get_mut("camera_defaults")
            .and_then(toml::Value::as_table_mut)
            .unwrap()
            .insert(
                "backend".to_owned(),
                toml::Value::String("reo-proto".to_owned()),
            );
        crate::config::write_configuration_table(&config_path, &current).unwrap();
        let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
        let router_thread = std::thread::spawn(move || {
            router.wait_and_drain(Some(Duration::from_secs(2))).unwrap()
        });

        let saved = save_camera_settings(
            CameraSettingsUpdate {
                display_name: Some(Some("Front entrance".to_owned())),
                ..CameraSettingsUpdate::default()
            },
            &router_tx,
            &state,
            "192.0.2.77",
        )
        .unwrap();

        assert_eq!(saved.camera.backend, "reo-proto");
        let raw = crate::config::load_configuration_table(&config_path).unwrap();
        assert!(
            !raw["cameras"]["front"]
                .as_table()
                .unwrap()
                .contains_key("backend")
        );
        assert_eq!(router_thread.join().unwrap(), 1);

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
        assert_eq!(camera.groups, ["cameras"]);
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

        let issued = state
            .access_manager
            .create_credential("Group viewer", None, AccessRole::User, None, 1_000)
            .unwrap();
        state
            .access_manager
            .set_camera_access(
                issued.metadata.id,
                issued.metadata.revision,
                crate::access::CameraAccess {
                    all_cameras: false,
                    group_ids: vec!["cameras".to_owned()],
                    camera_ids: Vec::new(),
                },
            )
            .unwrap();
        let session_id = SessionId::from_u64(719);
        bind_credential_test_session(&state, session_id, issued.access_key);
        assert!(
            camera_access::for_session(&state, session_id)
                .unwrap()
                .allows("192.0.2.79")
        );

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
    fn server_listener_can_bind_before_serving_starts() {
        let listener = bind_server_listener("127.0.0.1", 0).unwrap();

        assert_eq!(listener.local_addr().unwrap().ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(listener.local_addr().unwrap().port(), 0);
    }

    #[test]
    fn runtime_settings_update_persists_and_reflects_pending_configuration() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-runtime-{}", rand::random::<u64>()));
        let config_path = directory.join("config.toml");
        let current_storage = directory.join("current-storage");
        let medium_term_path = directory.join("new-medium");
        let long_term_path = directory.join("new-archive");
        let recording_catalog_path = directory.join("metadata/new-recordings.db");
        let event_thumbnail_path = directory.join("metadata/new-thumbnails");
        crate::config::write_private_file(
            &config_path,
            format!(
                r#"
                host = "0.0.0.0"
                port = 3000

                [storage]
                medium_term_path = {current_storage:?}
                long_term_path = {current_storage:?}

                [cameras.front]
                ip = "192.0.2.44"
                username = "operator"
                password = "not-in-the-response"
            "#
            )
            .as_bytes(),
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let saved = save_runtime_settings(
            RuntimeSettingsUpdate {
                host: "127.0.0.1".to_owned(),
                port: 3200,
                expected_configuration_revision: String::new(),
                storage: RuntimeStorageSettingsUpdate {
                    medium_term_path: medium_term_path.to_string_lossy().into_owned(),
                    long_term_path: long_term_path.to_string_lossy().into_owned(),
                    recording_catalog_path: recording_catalog_path.to_string_lossy().into_owned(),
                    event_thumbnail_path: event_thumbnail_path.to_string_lossy().into_owned(),
                    event_thumbnail_max_mb: 512,
                    short_term_secs: 30,
                    medium_term_secs: 120,
                    flush_interval_secs: 15,
                    write_buffer_bytes: 16_384,
                    long_term_max_gb: 24,
                    minimum_free_gb: 8,
                    maximum_used_percent: Some(85),
                    warning_free_gb: 12,
                    critical_free_gb: 8,
                    cleanup_hysteresis_gb: 2,
                },
                move_existing_recordings: false,
            },
            &state,
        )
        .unwrap();

        assert_eq!(saved.config.host, "127.0.0.1");
        assert_eq!(saved.config.port, 3200);
        assert_eq!(
            saved.config.storage.medium_term_path,
            medium_term_path.to_string_lossy()
        );
        assert_eq!(
            saved.config.storage.long_term_path,
            long_term_path.to_string_lossy()
        );
        assert_eq!(
            saved.config.storage.recording_catalog_path,
            recording_catalog_path.to_string_lossy()
        );
        assert_eq!(
            saved.config.storage.event_thumbnail_path,
            event_thumbnail_path.to_string_lossy()
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
        assert_eq!(storage.medium_term_path, medium_term_path.to_string_lossy());
        assert_eq!(storage.long_term_path, long_term_path.to_string_lossy());
        assert_eq!(
            storage.recording_catalog_path,
            recording_catalog_path.to_string_lossy()
        );
        assert_eq!(
            storage.event_thumbnail_path,
            event_thumbnail_path.to_string_lossy()
        );
        assert_eq!(storage.event_thumbnail_max_mb, 512);
        assert_eq!(storage.minimum_free_gb, Some(8));
        assert_eq!(storage.maximum_used_percent, Some(85));
        assert_eq!(persisted.camera_count, 1);

        let Err(error) = save_runtime_settings(
            RuntimeSettingsUpdate {
                host: "127.0.0.1".to_owned(),
                port: 0,
                expected_configuration_revision: String::new(),
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
                    minimum_free_gb: 8,
                    maximum_used_percent: Some(85),
                    warning_free_gb: 12,
                    critical_free_gb: 8,
                    cleanup_hysteresis_gb: 2,
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
    fn runtime_storage_validation_failures_leave_configuration_unchanged() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-runtime-storage-validation-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        crate::config::write_private_file(
            &config_path,
            b"host = \"0.0.0.0\"\nport = 3000\n[storage]\nlong_term_max_gb = 24\n",
        )
        .unwrap();
        let original = std::fs::read(&config_path).unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let writable = directory.join("writable").to_string_lossy().into_owned();

        let Err(invalid_thresholds) = save_runtime_settings(
            RuntimeSettingsUpdate {
                host: "127.0.0.1".to_owned(),
                port: 3200,
                expected_configuration_revision: String::new(),
                storage: RuntimeStorageSettingsUpdate {
                    medium_term_path: writable.clone(),
                    long_term_path: writable.clone(),
                    recording_catalog_path: directory
                        .join("recordings.db")
                        .to_string_lossy()
                        .into_owned(),
                    event_thumbnail_path: directory
                        .join("thumbnails")
                        .to_string_lossy()
                        .into_owned(),
                    event_thumbnail_max_mb: 512,
                    short_term_secs: 30,
                    medium_term_secs: 120,
                    flush_interval_secs: 15,
                    write_buffer_bytes: 16_384,
                    long_term_max_gb: 24,
                    minimum_free_gb: 8,
                    maximum_used_percent: Some(85),
                    warning_free_gb: 7,
                    critical_free_gb: 8,
                    cleanup_hysteresis_gb: 2,
                },
                move_existing_recordings: false,
            },
            &state,
        ) else {
            panic!("contradictory storage thresholds must be rejected");
        };
        assert_eq!(invalid_thresholds.code, proto::ErrorCode::InvalidRequest);
        assert!(invalid_thresholds.message.contains("warning free space"));
        assert_eq!(std::fs::read(&config_path).unwrap(), original);

        let inaccessible_path = if cfg!(windows) {
            "Z:\\keeppeek-unavailable\\recordings"
        } else {
            "/dev/null/recordings"
        };
        let Err(invalid_path) = save_runtime_settings(
            RuntimeSettingsUpdate {
                host: "127.0.0.1".to_owned(),
                port: 3200,
                expected_configuration_revision: String::new(),
                storage: RuntimeStorageSettingsUpdate {
                    medium_term_path: inaccessible_path.to_owned(),
                    long_term_path: inaccessible_path.to_owned(),
                    recording_catalog_path: directory
                        .join("recordings.db")
                        .to_string_lossy()
                        .into_owned(),
                    event_thumbnail_path: directory
                        .join("thumbnails")
                        .to_string_lossy()
                        .into_owned(),
                    event_thumbnail_max_mb: 512,
                    short_term_secs: 30,
                    medium_term_secs: 120,
                    flush_interval_secs: 15,
                    write_buffer_bytes: 16_384,
                    long_term_max_gb: 24,
                    minimum_free_gb: 0,
                    maximum_used_percent: None,
                    warning_free_gb: 0,
                    critical_free_gb: 0,
                    cleanup_hysteresis_gb: 0,
                },
                move_existing_recordings: false,
            },
            &state,
        ) else {
            panic!("inaccessible storage paths must be rejected");
        };
        assert_eq!(invalid_path.code, proto::ErrorCode::InvalidRequest);
        assert!(invalid_path.message.contains("not writable"));
        assert_eq!(std::fs::read(&config_path).unwrap(), original);

        let stale_revision = current_config(&state).configuration_revision;
        let mut externally_changed = original;
        externally_changed.extend_from_slice(b"unrelated_setting = true\n");
        crate::config::write_private_file(&config_path, &externally_changed).unwrap();
        let Err(conflict) = save_runtime_settings(
            RuntimeSettingsUpdate {
                host: "127.0.0.1".to_owned(),
                port: 3200,
                expected_configuration_revision: stale_revision,
                storage: RuntimeStorageSettingsUpdate {
                    medium_term_path: writable.clone(),
                    long_term_path: writable,
                    recording_catalog_path: directory
                        .join("recordings.db")
                        .to_string_lossy()
                        .into_owned(),
                    event_thumbnail_path: directory
                        .join("thumbnails")
                        .to_string_lossy()
                        .into_owned(),
                    event_thumbnail_max_mb: 512,
                    short_term_secs: 30,
                    medium_term_secs: 120,
                    flush_interval_secs: 15,
                    write_buffer_bytes: 16_384,
                    long_term_max_gb: 24,
                    minimum_free_gb: 0,
                    maximum_used_percent: None,
                    warning_free_gb: 0,
                    critical_free_gb: 0,
                    cleanup_hysteresis_gb: 0,
                },
                move_existing_recordings: false,
            },
            &state,
        ) else {
            panic!("a stale runtime editor must be rejected");
        };
        assert_eq!(conflict.code, proto::ErrorCode::Rejected);
        assert!(
            conflict
                .message
                .contains("changed after this editor was opened")
        );
        assert_eq!(std::fs::read(&config_path).unwrap(), externally_changed);

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
                expected_configuration_revision: String::new(),
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
                    minimum_free_gb: 0,
                    maximum_used_percent: None,
                    warning_free_gb: 0,
                    critical_free_gb: 0,
                    cleanup_hysteresis_gb: 0,
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
                expected_configuration_revision: String::new(),
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
                    minimum_free_gb: 0,
                    maximum_used_percent: None,
                    warning_free_gb: 0,
                    critical_free_gb: 0,
                    cleanup_hysteresis_gb: 0,
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
        let subnets = (0_u32..33).collect::<Vec<_>>();
        let state = ServerState::empty();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let session_id = SessionId::from_u64(96);

        let Err(error) = camera_discovery::discover(
            &state,
            &router_tx,
            session_id,
            proto::DiscoverCameras {
                discovery_id: "excessive-subnets".to_owned(),
                subnets,
                ..Default::default()
            },
        ) else {
            panic!("excessive discovery subnets must be rejected");
        };

        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
        assert!(
            state
                .camera_discovery_tasks
                .snapshot(session_id, "excessive-subnets")
                .is_err()
        );
    }
}
