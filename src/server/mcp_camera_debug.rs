//! Administrator-only MCP handler and backend contract for raw camera debugging.
//!
//! Enforces fail-closed `[mcp].allow_camera_debugging` gating, `debug_camera_ids`
//! allowlist scoping, read-only header restrictions, bounded argument parsing,
//! live `inspect_pipeline` telemetry, and explicit redaction-boundary control.

use super::{
    ApiPrincipal, CameraEntry, ControlCommandError, ServerState, query_router, record_access_audit,
    unix_time_ms,
};
use crate::{
    access::{AccessRole, ClientClassificationReason},
    api::{CameraStatus, proto},
    camera_debug::{
        protocols::{
            self as debug_protocols, CaptureArtifacts, CaptureProtocol,
            CaptureRequest as EngineCaptureRequest, CaptureResult, TransportComparisonEntry,
        },
        repro::{self, ExportReproOptions, TrimMediaMode as ReproTrimMediaMode},
        store::{
            CaptureMetadata, CaptureStore, DEFAULT_CAPTURE_TTL_SECS, StoreLimits,
            validate_artifact_name as validate_store_artifact_name,
            validate_capture_id as validate_store_capture_id,
        },
    },
    cameras::{BAICHUAN_PORT, CameraTransport},
    config::mcp::McpConfig,
    logging::{LoggingService, TRACE_FILTER_MAX_BYTES},
    mcp::protocol::ToolCallResult,
    runtime::{FacadeSender, RouterMessage, RouterQuery, RouterResponse},
    stats::StreamHealthReport,
    storage::health::RecordingStreamHealthSnapshot,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt::{self, Write as _},
    fs::File,
    io::{Read, Seek, SeekFrom},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tracing_subscriber::EnvFilter;
use url::Url;
use zeroize::Zeroize;

const CAMERA_ID_MAX_BYTES: usize = 128;
const DURATION_SECONDS_DEFAULT: u64 = 10;
const DURATION_SECONDS_MIN: u64 = 1;
const DURATION_SECONDS_MAX: u64 = 60;
const CAPTURE_BYTES_MIN: usize = 65_536;
const CAPTURE_BYTES_MAX: usize = 268_435_456;
const ARTIFACTS_MAX: usize = 6;
const PROTOCOLS_MAX: usize = 6;
const INSPECT_CAMERAS_MAX: usize = 256;
const PIPELINE_METRICS_CACHE_MAX: usize = 256;
const DEADLINE_MARGIN_MS: u64 = 250;
const MIN_CAPTURE_DURATION_MS: u64 = 100;
const DEFAULT_CAPTURE_FPS: u64 = 60;
const READ_ARTIFACT_LENGTH_DEFAULT: usize = 65_536;
const READ_ARTIFACT_LENGTH_MIN: usize = 1;
const READ_ARTIFACT_LENGTH_MAX: usize = 131_072;
const SHA256_STREAM_BUF_BYTES: usize = 65_536;

const _: () = {
    assert!(DURATION_SECONDS_MIN <= DURATION_SECONDS_DEFAULT);
    assert!(DURATION_SECONDS_DEFAULT <= DURATION_SECONDS_MAX);
    assert!(CAPTURE_BYTES_MIN <= CAPTURE_BYTES_MAX);
    assert!(ARTIFACTS_MAX == 6);
    assert!(PROTOCOLS_MAX == 6);
    assert!(READ_ARTIFACT_LENGTH_MIN <= READ_ARTIFACT_LENGTH_DEFAULT);
    assert!(READ_ARTIFACT_LENGTH_DEFAULT <= READ_ARTIFACT_LENGTH_MAX);
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DebugAction {
    InspectPipeline,
    Capture,
    CompareTransports,
    ExportReproFixture,
    ListCaptures,
    DeleteCapture,
    ReadArtifact,
}

impl DebugAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InspectPipeline => "inspect_pipeline",
            Self::Capture => "capture",
            Self::CompareTransports => "compare_transports",
            Self::ExportReproFixture => "export_repro_fixture",
            Self::ListCaptures => "list_captures",
            Self::DeleteCapture => "delete_capture",
            Self::ReadArtifact => "read_artifact",
        }
    }

    const fn blocked_in_read_only_mode(self) -> bool {
        matches!(
            self,
            Self::Capture | Self::CompareTransports | Self::DeleteCapture
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DebugStream {
    Main,
    Sub,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DebugArtifact {
    Pcapng,
    VideoBitstream,
    AudioBitstream,
    MuxedMp4,
    ProtocolTranscript,
    Traces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ArtifactEncoding {
    Auto,
    Base64,
    Utf8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DebugProtocol {
    Rtsp,
    Audio,
    Onvif,
    Isapi,
    ReolinkHttp,
    ReoProto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CompareMode {
    MediaTransports,
    EventChannels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TrimMediaMode {
    HeadersAndMinimalFailingSlice,
    SyntheticPayloads,
    KeepRawFrames,
}

/// Zeroized camera credentials passed only to authorized raw debug actions.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct CameraDebugCredentials {
    pub(super) username: String,
    pub(super) password: String,
    pub(super) uid: Option<String>,
}

impl CameraDebugCredentials {
    fn from_camera(camera: &CameraEntry) -> Self {
        Self {
            username: camera.configuration.username.clone(),
            password: camera.configuration.password.clone(),
            uid: camera.configuration.uid.clone(),
        }
    }
}

impl fmt::Debug for CameraDebugCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CameraDebugCredentials")
            .field("has_username", &(!self.username.is_empty()))
            .field("has_password", &(!self.password.is_empty()))
            .field("has_uid", &self.uid.is_some())
            .finish()
    }
}

impl Drop for CameraDebugCredentials {
    fn drop(&mut self) {
        self.username.zeroize();
        self.password.zeroize();
        if let Some(uid) = &mut self.uid {
            uid.zeroize();
        }
    }
}

/// Resolved target camera metadata supplied to [`CameraDebugService`].
#[derive(Debug, Clone, Serialize)]
pub(super) struct DebugCameraTarget {
    pub(super) camera_id: String,
    pub(super) ip: String,
    pub(super) recording_label: String,
    pub(super) backend: String,
    pub(super) transport: String,
    pub(super) http_port: Option<u16>,
    pub(super) onvif_port: Option<u16>,
    pub(super) main_rtsp_url: Option<String>,
    pub(super) sub_rtsp_url: Option<String>,
}

impl DebugCameraTarget {
    fn from_camera(camera: &CameraEntry, include_raw_urls: bool) -> Self {
        Self {
            camera_id: camera.info.id.clone(),
            ip: camera.info.ip.clone(),
            recording_label: camera.recording_label.clone(),
            backend: camera.info.backend.clone(),
            transport: camera.info.transport.clone(),
            http_port: camera.info.ports.http,
            onvif_port: camera.info.ports.onvif,
            main_rtsp_url: include_raw_urls
                .then(|| camera.configuration.main_rtsp_url.clone())
                .flatten(),
            sub_rtsp_url: include_raw_urls
                .then(|| camera.configuration.sub_rtsp_url.clone())
                .flatten(),
        }
    }
}

/// Internal connection parameters passed to the capture engine without serialization.
#[derive(Clone)]
pub(super) struct CameraDebugConnection {
    pub(super) credentials: CameraDebugCredentials,
    pub(super) main_rtsp_url: Option<String>,
    pub(super) sub_rtsp_url: Option<String>,
}

impl CameraDebugConnection {
    fn from_camera(camera: &CameraEntry) -> Self {
        Self {
            credentials: CameraDebugCredentials::from_camera(camera),
            main_rtsp_url: camera.configuration.main_rtsp_url.clone(),
            sub_rtsp_url: camera.configuration.sub_rtsp_url.clone(),
        }
    }
}

impl fmt::Debug for CameraDebugConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CameraDebugConnection")
            .field("credentials", &self.credentials)
            .field("has_main_rtsp_url", &self.main_rtsp_url.is_some())
            .field("has_sub_rtsp_url", &self.sub_rtsp_url.is_some())
            .finish()
    }
}

impl Drop for CameraDebugConnection {
    fn drop(&mut self) {
        if let Some(url) = &mut self.main_rtsp_url {
            url.zeroize();
        }
        if let Some(url) = &mut self.sub_rtsp_url {
            url.zeroize();
        }
    }
}

/// Zeroized secret strings passed to repro scrubbing without serialization.
#[derive(Clone, Default)]
pub(super) struct ZeroizedSecretList(pub(super) Vec<String>);

impl fmt::Debug for ZeroizedSecretList {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ZeroizedSecretList")
            .field("count", &self.0.len())
            .finish()
    }
}

impl Drop for ZeroizedSecretList {
    fn drop(&mut self) {
        for item in &mut self.0 {
            item.zeroize();
        }
    }
}

/// Bounded inputs for a `capture` request.
#[derive(Debug, Clone, Serialize)]
pub(super) struct CaptureRequest {
    pub(super) target: DebugCameraTarget,
    pub(super) stream: DebugStream,
    pub(super) artifacts: Vec<DebugArtifact>,
    pub(super) protocols: Vec<DebugProtocol>,
    pub(super) protocols_explicit: bool,
    pub(super) duration_seconds: u64,
    pub(super) max_bytes: usize,
    pub(super) max_storage_bytes: u64,
    pub(super) include_raw_auth: bool,
    pub(super) trigger_probe: bool,
    pub(super) trace_filter: Option<String>,
    pub(super) config_dir: Option<PathBuf>,
    #[serde(skip_serializing)]
    pub(super) logging: Option<LoggingService>,
    #[serde(skip_serializing)]
    pub(super) credentials: Option<CameraDebugCredentials>,
    #[serde(skip_serializing)]
    pub(super) connection: CameraDebugConnection,
    #[serde(skip_serializing)]
    pub(super) deadline: Instant,
}

/// Bounded inputs for a `compare_transports` request.
#[derive(Debug, Clone, Serialize)]
pub(super) struct CompareTransportsRequest {
    pub(super) target: DebugCameraTarget,
    pub(super) stream: DebugStream,
    pub(super) mode: CompareMode,
    pub(super) duration_seconds: u64,
    pub(super) max_bytes: usize,
    pub(super) max_storage_bytes: u64,
    pub(super) include_raw_auth: bool,
    pub(super) trace_filter: Option<String>,
    pub(super) config_dir: Option<PathBuf>,
    #[serde(skip_serializing)]
    pub(super) logging: Option<LoggingService>,
    #[serde(skip_serializing)]
    pub(super) credentials: Option<CameraDebugCredentials>,
    #[serde(skip_serializing)]
    pub(super) connection: CameraDebugConnection,
    #[serde(skip_serializing)]
    pub(super) deadline: Instant,
}

/// Bounded inputs for an `export_repro_fixture` request.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ExportReproFixtureRequest {
    pub(super) capture_id: String,
    pub(super) camera_id: Option<String>,
    pub(super) allowed_camera_ids: Option<Vec<String>>,
    pub(super) strip_credentials: bool,
    pub(super) anonymize_network: bool,
    pub(super) trim_media: TrimMediaMode,
    pub(super) max_bytes: usize,
    pub(super) config_dir: Option<PathBuf>,
    #[serde(skip_serializing)]
    pub(super) extra_secrets: ZeroizedSecretList,
    #[serde(skip_serializing)]
    pub(super) deadline: Instant,
}

/// Bounded inputs for a `list_captures` request.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ListCapturesRequest {
    pub(super) camera_id: Option<String>,
    pub(super) allowed_camera_ids: Option<Vec<String>>,
    pub(super) config_dir: Option<PathBuf>,
}

/// Bounded inputs for a `delete_capture` request.
#[derive(Debug, Clone, Serialize)]
pub(super) struct DeleteCaptureRequest {
    pub(super) capture_id: Option<String>,
    pub(super) camera_id: Option<String>,
    pub(super) delete_all: bool,
    pub(super) allowed_camera_ids: Option<Vec<String>>,
    pub(super) config_dir: Option<PathBuf>,
}

/// Bounded inputs for a `read_artifact` request.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ReadArtifactRequest {
    pub(super) capture_id: String,
    pub(super) artifact: String,
    pub(super) offset: u64,
    pub(super) length: usize,
    pub(super) encoding: ArtifactEncoding,
    pub(super) allowed_camera_ids: Option<Vec<String>>,
    pub(super) config_dir: Option<PathBuf>,
}

/// Backend interface implemented by the camera debug capture engine.
pub(super) trait CameraDebugService: Send + Sync {
    fn capture(&self, request: CaptureRequest) -> Result<Value, ControlCommandError>;

    fn compare_transports(
        &self,
        request: CompareTransportsRequest,
    ) -> Result<Value, ControlCommandError>;

    fn export_repro_fixture(
        &self,
        request: ExportReproFixtureRequest,
    ) -> Result<Value, ControlCommandError>;

    fn list_captures(&self, request: ListCapturesRequest) -> Result<Value, ControlCommandError>;

    fn delete_capture(&self, request: DeleteCaptureRequest) -> Result<Value, ControlCommandError>;

    fn read_artifact(&self, request: ReadArtifactRequest) -> Result<Value, ControlCommandError>;

    fn pipeline_debug_metrics(&self, _camera_id: &str) -> Option<Value> {
        None
    }
}

/// Live camera debug backend backed by [`CaptureStore`] and `crate::camera_debug`.
#[derive(Debug, Default)]
pub(super) struct EngineCameraDebugService {
    latest_metrics: Mutex<HashMap<String, Value>>,
}

impl EngineCameraDebugService {
    fn record_metrics(&self, meta: &CaptureMetadata) {
        let mut cache = self
            .latest_metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !cache.contains_key(&meta.camera_id) && cache.len() >= PIPELINE_METRICS_CACHE_MAX {
            cache.clear();
        }
        cache.insert(
            meta.camera_id.clone(),
            json!({
                "capture_id": meta.capture_id,
                "protocol": meta.protocol,
                "transport": meta.transport,
                "created_at_unix_secs": meta.created_at_unix_secs,
                "duration_ms": meta.duration_ms,
                "total_bytes": meta.total_bytes,
                "packet_count": meta.packet_count,
                "anomalies": meta.anomalies,
            }),
        );
    }

    fn clear_metrics_for_deleted(&self, deleted_ids: &[String]) {
        if deleted_ids.is_empty() {
            return;
        }
        let mut cache = self
            .latest_metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|_, value| {
            value
                .get("capture_id")
                .and_then(Value::as_str)
                .is_none_or(|id| !deleted_ids.iter().any(|deleted| deleted == id))
        });
    }
}

impl CameraDebugService for EngineCameraDebugService {
    fn capture(&self, request: CaptureRequest) -> Result<Value, ControlCommandError> {
        remaining_timeout(request.deadline)?;
        let store = open_capture_store(
            request.config_dir.as_deref(),
            u64::try_from(request.max_bytes).unwrap_or(u64::MAX),
            request.max_storage_bytes,
        )?;
        let targets = resolve_capture_targets(&request)?;
        let total_targets = targets.len().max(1);
        let frames_max = request
            .duration_seconds
            .saturating_mul(DEFAULT_CAPTURE_FPS)
            .clamp(60, 1800);
        let artifacts = map_capture_artifacts(&request.artifacts);
        let mut results = Vec::with_capacity(targets.len());
        for (idx, (protocol, stream_label, target_url)) in targets.into_iter().enumerate() {
            let remaining = remaining_timeout(request.deadline)?;
            let remaining_targets = u64::try_from(total_targets.saturating_sub(idx))
                .unwrap_or(1)
                .max(1);
            let duration_max_ms = bounded_capture_duration_ms(
                request.duration_seconds,
                remaining,
                remaining_targets,
            )?;
            let capture_id = generate_capture_id(
                &request.target.camera_id,
                protocol.as_str(),
                stream_label,
                idx,
            );
            let mut engine_req = EngineCaptureRequest::new(
                capture_id,
                request.target.camera_id.clone(),
                protocol,
                target_url,
            );
            let creds = request
                .credentials
                .as_ref()
                .unwrap_or(&request.connection.credentials);
            engine_req.username = creds.username.clone();
            engine_req.password = creds.password.clone();
            engine_req.transport = parse_camera_transport(&request.target.transport);
            engine_req.include_raw_auth = request.include_raw_auth;
            engine_req.trigger_probe = request.trigger_probe;
            engine_req.artifacts = artifacts;
            engine_req.duration_max_ms = duration_max_ms;
            engine_req.frames_max = frames_max;
            engine_req.logging = request.logging.clone();
            engine_req.trace_filter = request.trace_filter.clone();
            let result = debug_protocols::run_capture(&store, &engine_req)
                .map_err(|err| engine_error("capture", err))?;
            self.record_metrics(&result.metadata);
            results.push(result);
        }
        Ok(build_capture_response(&request, &results))
    }

    fn compare_transports(
        &self,
        request: CompareTransportsRequest,
    ) -> Result<Value, ControlCommandError> {
        let remaining = remaining_timeout(request.deadline)?;
        let store = open_capture_store(
            request.config_dir.as_deref(),
            u64::try_from(request.max_bytes).unwrap_or(u64::MAX),
            request.max_storage_bytes,
        )?;
        match request.mode {
            CompareMode::MediaTransports => {
                self.compare_media_transports(&store, &request, remaining)
            }
            CompareMode::EventChannels => self.compare_event_channels(&store, &request, remaining),
        }
    }

    fn export_repro_fixture(
        &self,
        request: ExportReproFixtureRequest,
    ) -> Result<Value, ControlCommandError> {
        remaining_timeout(request.deadline)?;
        let store = open_existing_store(request.config_dir.as_deref())?;
        let meta = store.load_metadata(&request.capture_id).map_err(|err| {
            ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                format!("capture '{}' was not found: {err}", request.capture_id),
            )
        })?;
        check_capture_scope(
            &meta,
            request.allowed_camera_ids.as_deref(),
            request.camera_id.as_deref(),
        )?;
        let options = ExportReproOptions {
            strip_credentials: request.strip_credentials,
            anonymize_network: request.anonymize_network,
            trim_media: map_trim_media(request.trim_media),
            packet_range: None,
            nalu_range: None,
            extra_secrets: if request.strip_credentials {
                request.extra_secrets.0.clone()
            } else {
                Vec::new()
            },
        };
        let fixture = repro::export_repro_fixture(&store, &request.capture_id, &options)
            .map_err(|err| engine_error("export_repro_fixture", err))?;
        let encoded_len = serde_json::to_vec(&fixture)
            .map_err(|err| engine_error("export_repro_fixture", err.into()))?
            .len();
        if encoded_len > request.max_bytes {
            return Err(invalid(&format!(
                "exported repro fixture ({encoded_len} bytes) exceeds max_bytes ({})",
                request.max_bytes
            )));
        }
        Ok(json!({
            "action": "export_repro_fixture",
            "capture_id": request.capture_id,
            "camera_id": meta.camera_id,
            "strip_credentials": request.strip_credentials,
            "anonymize_network": request.anonymize_network,
            "sharing_safety_report": fixture.sharing_safety_report,
            "fixture": fixture,
        }))
    }

    fn list_captures(&self, request: ListCapturesRequest) -> Result<Value, ControlCommandError> {
        let root_dir = resolve_store_root(request.config_dir.as_deref());
        if !root_dir.exists() {
            return Ok(json!({
                "action": "list_captures",
                "count": 0,
                "captures": [],
            }));
        }
        let store = open_store_at(&root_dir, StoreLimits::default())?;
        let captures = store
            .list_captures()
            .map_err(|err| engine_error("list_captures", err))?;
        let filtered: Vec<CaptureMetadata> = captures
            .into_iter()
            .filter(|meta| {
                capture_matches_scope(
                    meta,
                    request.allowed_camera_ids.as_deref(),
                    request.camera_id.as_deref(),
                )
            })
            .collect();
        Ok(json!({
            "action": "list_captures",
            "count": filtered.len(),
            "captures": filtered,
        }))
    }

    fn delete_capture(&self, request: DeleteCaptureRequest) -> Result<Value, ControlCommandError> {
        let root_dir = resolve_store_root(request.config_dir.as_deref());
        if request.delete_all {
            return self.delete_all_captures(&root_dir, &request);
        }
        let capture_id = request
            .capture_id
            .as_deref()
            .ok_or_else(|| invalid("capture_id is required when delete_all is false"))?;
        if !root_dir.exists() {
            return Err(not_found_capture(capture_id));
        }
        let store = open_store_at(&root_dir, StoreLimits::default())?;
        if !store
            .capture_dir(capture_id)
            .map_err(|err| engine_error("delete_capture", err))?
            .exists()
        {
            return Err(not_found_capture(capture_id));
        }
        ensure_delete_capture_allowed(&store, capture_id, &request)?;
        let existed = store
            .delete_capture(capture_id)
            .map_err(|err| engine_error("delete_capture", err))?;
        if !existed {
            return Err(not_found_capture(capture_id));
        }
        let deleted_ids = vec![capture_id.to_owned()];
        self.clear_metrics_for_deleted(&deleted_ids);
        Ok(json!({
            "action": "delete_capture",
            "capture_id": capture_id,
            "deleted": 1,
            "deleted_capture_ids": deleted_ids,
        }))
    }

    fn read_artifact(&self, request: ReadArtifactRequest) -> Result<Value, ControlCommandError> {
        let store = open_existing_store(request.config_dir.as_deref())?;
        let meta = store.load_metadata(&request.capture_id).map_err(|err| {
            ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                format!("capture '{}' was not found: {err}", request.capture_id),
            )
        })?;
        check_capture_scope(&meta, request.allowed_camera_ids.as_deref(), None)?;
        validate_store_artifact_name(&request.artifact)
            .map_err(|err| invalid(&format!("invalid artifact filename: {err}")))?;
        let capture_dir = store
            .capture_dir(&request.capture_id)
            .map_err(|err| engine_error("read_artifact", err))?;
        let artifact_path = resolve_regular_artifact_path(&capture_dir, &request.artifact)?;
        read_artifact_chunk(&request, &meta.camera_id, &artifact_path)
    }

    fn pipeline_debug_metrics(&self, camera_id: &str) -> Option<Value> {
        self.latest_metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(camera_id)
            .cloned()
    }
}

impl EngineCameraDebugService {
    fn compare_media_transports(
        &self,
        store: &CaptureStore,
        request: &CompareTransportsRequest,
        remaining: Duration,
    ) -> Result<Value, ControlCommandError> {
        let (protocol, target_url) =
            resolve_primary_media_target(&request.target, &request.connection, request.stream);
        let duration_max_ms = bounded_capture_duration_ms(request.duration_seconds, remaining, 2)?;
        let base_id = generate_capture_id(&request.target.camera_id, "cmp", "media", 0);
        let mut base_req = EngineCaptureRequest::new(
            base_id,
            request.target.camera_id.clone(),
            protocol,
            target_url,
        );
        let creds = request
            .credentials
            .as_ref()
            .unwrap_or(&request.connection.credentials);
        base_req.username = creds.username.clone();
        base_req.password = creds.password.clone();
        base_req.include_raw_auth = request.include_raw_auth;
        base_req.duration_max_ms = duration_max_ms;
        base_req.frames_max = request
            .duration_seconds
            .saturating_mul(DEFAULT_CAPTURE_FPS)
            .clamp(30, 600);
        base_req.logging = request.logging.clone();
        base_req.trace_filter = request.trace_filter.clone();
        let report = debug_protocols::compare_transports(
            store,
            &base_req,
            &[CameraTransport::Tcp, CameraTransport::Udp],
        )
        .map_err(|err| engine_error("compare_transports", err))?;
        for entry in report.entries.iter().filter(|entry| entry.success) {
            if let Ok(meta) = store.load_metadata(&entry.capture_id) {
                self.record_metrics(&meta);
            }
        }
        Ok(json!({
            "action": "compare_transports",
            "mode": "media_transports",
            "camera_id": report.camera_id,
            "protocol": report.protocol,
            "recommended_transport": report.recommended_transport,
            "entries": report.entries,
        }))
    }

    fn compare_event_channels(
        &self,
        store: &CaptureStore,
        request: &CompareTransportsRequest,
        _remaining: Duration,
    ) -> Result<Value, ControlCommandError> {
        let candidates = resolve_event_channel_targets(&request.target);
        if candidates.is_empty() {
            return Err(invalid(
                "camera does not have an ONVIF, HTTP, or Reolink event channel endpoint configured",
            ));
        }
        let total_candidates = candidates.len().max(1);
        let mut entries = Vec::with_capacity(candidates.len());
        for (idx, (protocol, url)) in candidates.into_iter().enumerate() {
            let remaining_budget = remaining_timeout(request.deadline)?;
            let remaining_candidates = u64::try_from(total_candidates.saturating_sub(idx))
                .unwrap_or(1)
                .max(1);
            let duration_max_ms = bounded_capture_duration_ms(
                request.duration_seconds,
                remaining_budget,
                remaining_candidates,
            )?;
            let capture_id =
                generate_capture_id(&request.target.camera_id, "evt", protocol.as_str(), idx);
            let mut req = EngineCaptureRequest::new(
                capture_id,
                request.target.camera_id.clone(),
                protocol,
                url,
            );
            req.username = request.connection.credentials.username.clone();
            req.password = request.connection.credentials.password.clone();
            req.include_raw_auth = request.include_raw_auth;
            req.trigger_probe = true;
            req.duration_max_ms = duration_max_ms;
            req.logging = request.logging.clone();
            req.trace_filter = request.trace_filter.clone();
            entries.push(self.run_comparison_candidate(store, &req, protocol.as_str()));
        }
        let recommended = entries
            .iter()
            .find(|entry| entry.success)
            .map(|entry| entry.transport.clone());
        Ok(json!({
            "action": "compare_transports",
            "mode": "event_channels",
            "camera_id": request.target.camera_id,
            "recommended_transport": recommended,
            "entries": entries,
        }))
    }

    fn run_comparison_candidate(
        &self,
        store: &CaptureStore,
        req: &EngineCaptureRequest,
        label: &str,
    ) -> TransportComparisonEntry {
        let started = Instant::now();
        match debug_protocols::run_capture(store, req) {
            Ok(result) => {
                self.record_metrics(&result.metadata);
                TransportComparisonEntry {
                    transport: label.to_owned(),
                    capture_id: result.metadata.capture_id,
                    success: true,
                    error: None,
                    duration_ms: result.metadata.duration_ms,
                    time_to_first_frame_ms: None,
                    total_bytes: result.metadata.total_bytes,
                    anomalies: result.bitstream.anomalies,
                }
            }
            Err(err) => TransportComparisonEntry {
                transport: label.to_owned(),
                capture_id: req.capture_id.clone(),
                success: false,
                error: Some(err.to_string()),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                time_to_first_frame_ms: None,
                total_bytes: 0,
                anomalies: Default::default(),
            },
        }
    }

    fn delete_all_captures(
        &self,
        root_dir: &Path,
        request: &DeleteCaptureRequest,
    ) -> Result<Value, ControlCommandError> {
        if !root_dir.exists() {
            return Ok(json!({
                "action": "delete_capture",
                "deleted": 0,
                "deleted_capture_ids": [],
            }));
        }
        let store = open_store_at(root_dir, StoreLimits::default())?;
        let captures = store
            .list_captures()
            .map_err(|err| engine_error("delete_capture", err))?;
        let mut deleted_ids = Vec::new();
        for meta in captures {
            if !capture_matches_scope(
                &meta,
                request.allowed_camera_ids.as_deref(),
                request.camera_id.as_deref(),
            ) {
                continue;
            }
            if store.is_camera_capture_active(&meta.camera_id) {
                return Err(active_capture_conflict(&meta.capture_id, &meta.camera_id));
            }
            if store
                .delete_capture(&meta.capture_id)
                .map_err(|err| engine_error("delete_capture", err))?
            {
                deleted_ids.push(meta.capture_id);
            }
        }
        self.clear_metrics_for_deleted(&deleted_ids);
        Ok(json!({
            "action": "delete_capture",
            "deleted": deleted_ids.len(),
            "deleted_capture_ids": deleted_ids,
        }))
    }
}

fn resolve_regular_artifact_path(
    capture_dir: &Path,
    artifact: &str,
) -> Result<PathBuf, ControlCommandError> {
    let path = capture_dir.join(artifact);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::NotFound,
                404,
                format!("artifact '{artifact}' was not found"),
            ));
        }
        Err(error) => return Err(engine_error("read_artifact", error.into())),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            format!("artifact '{artifact}' must be a regular file inside capture directory"),
        ));
    }
    Ok(path)
}

fn read_artifact_chunk(
    request: &ReadArtifactRequest,
    camera_id: &str,
    path: &Path,
) -> Result<Value, ControlCommandError> {
    let mut file = File::open(path).map_err(|err| engine_error("read_artifact", err.into()))?;
    let total_bytes = file
        .metadata()
        .map_err(|err| engine_error("read_artifact", err.into()))?
        .len();
    if request.offset > total_bytes {
        return Err(invalid(&format!(
            "offset ({}) exceeds artifact size ({total_bytes} bytes)",
            request.offset
        )));
    }
    let sha256 = compute_file_sha256(&mut file)?;
    let remaining =
        usize::try_from(total_bytes.saturating_sub(request.offset)).unwrap_or(usize::MAX);
    let to_read = request.length.min(remaining);
    let mut chunk = vec![0_u8; to_read];
    file.seek(SeekFrom::Start(request.offset))
        .map_err(|err| engine_error("read_artifact", err.into()))?;
    file.read_exact(&mut chunk)
        .map_err(|err| engine_error("read_artifact", err.into()))?;
    let chunk_len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
    let next_offset = request.offset.saturating_add(chunk_len);
    let (encoding, data) = encode_artifact_chunk(chunk, request.encoding)?;
    Ok(json!({
        "action": "read_artifact",
        "capture_id": request.capture_id,
        "camera_id": camera_id,
        "artifact": request.artifact,
        "offset": request.offset,
        "length": chunk_len,
        "total_bytes": total_bytes,
        "sha256": sha256,
        "eof": next_offset >= total_bytes,
        "next_offset": next_offset,
        "encoding": encoding,
        "data": data,
    }))
}

fn compute_file_sha256(file: &mut File) -> Result<String, ControlCommandError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|err| engine_error("read_artifact", err.into()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; SHA256_STREAM_BUF_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| engine_error("read_artifact", err.into()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    Ok(hex)
}

fn encode_artifact_chunk(
    chunk: Vec<u8>,
    encoding: ArtifactEncoding,
) -> Result<(&'static str, String), ControlCommandError> {
    match encoding {
        ArtifactEncoding::Base64 => Ok(("base64", BASE64.encode(&chunk))),
        ArtifactEncoding::Utf8 => {
            let text = String::from_utf8(chunk).map_err(|_| {
                invalid("artifact chunk is not valid UTF-8; use encoding='base64' or 'auto'")
            })?;
            Ok(("utf8", text))
        }
        ArtifactEncoding::Auto => {
            if !chunk.contains(&0)
                && let Ok(text) = std::str::from_utf8(&chunk)
            {
                return Ok(("utf8", text.to_owned()));
            }
            Ok(("base64", BASE64.encode(&chunk)))
        }
    }
}

fn map_capture_artifacts(artifacts: &[DebugArtifact]) -> CaptureArtifacts {
    CaptureArtifacts {
        pcapng: artifacts.contains(&DebugArtifact::Pcapng),
        video_bitstream: artifacts.contains(&DebugArtifact::VideoBitstream),
        audio_bitstream: artifacts.contains(&DebugArtifact::AudioBitstream),
        muxed_mp4: artifacts.contains(&DebugArtifact::MuxedMp4),
        protocol_transcript: artifacts.contains(&DebugArtifact::ProtocolTranscript),
        traces: artifacts.contains(&DebugArtifact::Traces),
    }
}

fn active_capture_conflict(capture_id: &str, camera_id: &str) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        409,
        format!(
            "cannot delete capture '{capture_id}' while capture for camera '{camera_id}' is active"
        ),
    )
}

fn ensure_delete_capture_allowed(
    store: &CaptureStore,
    capture_id: &str,
    request: &DeleteCaptureRequest,
) -> Result<(), ControlCommandError> {
    if let Ok(meta) = store.load_metadata(capture_id) {
        check_capture_scope(
            &meta,
            request.allowed_camera_ids.as_deref(),
            request.camera_id.as_deref(),
        )?;
        if store.is_camera_capture_active(&meta.camera_id) {
            return Err(active_capture_conflict(capture_id, &meta.camera_id));
        }
        return Ok(());
    }
    if request.allowed_camera_ids.is_some() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            format!(
                "cannot delete capture '{capture_id}' without readable metadata while [mcp].debug_camera_ids is active"
            ),
        ));
    }
    if request.camera_id.is_some() {
        return Err(invalid(&format!(
            "cannot delete capture '{capture_id}' without readable metadata while camera_id filter is set"
        )));
    }
    Ok(())
}

fn resolve_store_root(config_dir: Option<&Path>) -> PathBuf {
    config_dir
        .map_or_else(crate::config::config_dir, Path::to_path_buf)
        .join("camera-debug")
}

fn open_capture_store(
    config_dir: Option<&Path>,
    max_capture_bytes: u64,
    max_storage_bytes: u64,
) -> Result<CaptureStore, ControlCommandError> {
    if max_capture_bytes > max_storage_bytes {
        return Err(invalid(&format!(
            "max_bytes ({max_capture_bytes}) exceeds [mcp].max_debug_storage_bytes ({max_storage_bytes})"
        )));
    }
    let limits = StoreLimits {
        max_capture_bytes,
        max_storage_bytes,
        ttl_secs: DEFAULT_CAPTURE_TTL_SECS,
    };
    open_store_at(&resolve_store_root(config_dir), limits)
}

fn open_existing_store(config_dir: Option<&Path>) -> Result<CaptureStore, ControlCommandError> {
    let root_dir = resolve_store_root(config_dir);
    if !root_dir.exists() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "no camera debug captures exist on disk",
        ));
    }
    open_store_at(&root_dir, StoreLimits::default())
}

fn open_store_at(
    root_dir: &Path,
    limits: StoreLimits,
) -> Result<CaptureStore, ControlCommandError> {
    CaptureStore::new(root_dir, limits).map_err(|err| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            500,
            format!("unable to open camera debug store: {err}"),
        )
    })
}

fn check_capture_scope(
    meta: &CaptureMetadata,
    allowed_camera_ids: Option<&[String]>,
    requested_camera_id: Option<&str>,
) -> Result<(), ControlCommandError> {
    if let Some(allowed) = allowed_camera_ids
        && !allowed.iter().any(|id| id == &meta.camera_id)
    {
        return Err(allowlist_denied(&meta.camera_id));
    }
    if let Some(camera_id) = requested_camera_id
        && meta.camera_id != camera_id
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            format!(
                "capture '{}' belongs to camera '{}', not '{camera_id}'",
                meta.capture_id, meta.camera_id
            ),
        ));
    }
    Ok(())
}

fn capture_matches_scope(
    meta: &CaptureMetadata,
    allowed_camera_ids: Option<&[String]>,
    requested_camera_id: Option<&str>,
) -> bool {
    let allowed = allowed_camera_ids.is_none_or(|ids| ids.iter().any(|id| id == &meta.camera_id));
    let matches_filter = requested_camera_id.is_none_or(|id| id == meta.camera_id);
    allowed && matches_filter
}

fn bounded_capture_duration_ms(
    duration_seconds: u64,
    remaining: Duration,
    capture_count: u64,
) -> Result<u64, ControlCommandError> {
    let remaining_ms = u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX);
    let usable_ms = remaining_ms.saturating_sub(DEADLINE_MARGIN_MS);
    if usable_ms < MIN_CAPTURE_DURATION_MS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Timeout,
            504,
            "remaining MCP timeout budget is too small for camera capture",
        ));
    }
    let per_capture_budget = (usable_ms / capture_count.max(1)).max(MIN_CAPTURE_DURATION_MS);
    let requested_ms = duration_seconds.saturating_mul(1_000);
    Ok(requested_ms.min(per_capture_budget))
}

fn generate_capture_id(
    camera_id: &str,
    proto_label: &str,
    stream_label: &str,
    idx: usize,
) -> String {
    let mut prefix: String = camera_id
        .bytes()
        .take(28)
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                byte as char
            } else {
                '-'
            }
        })
        .collect();
    if prefix.is_empty() {
        prefix.push_str("cam");
    }
    let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
    let ts = unix_time_ms() / 1_000;
    format!("{prefix}-{proto_label}-{stream_label}-{idx}-{ts}-{short_id}")
}

fn resolve_capture_targets(
    request: &CaptureRequest,
) -> Result<Vec<(CaptureProtocol, &'static str, String)>, ControlCommandError> {
    if !request.protocols_explicit {
        let (protocol, stream_label, url) = resolve_primary_capture_with_label(
            &request.target,
            &request.connection,
            request.stream,
        );
        return Ok(vec![(protocol, stream_label, url)]);
    }
    let mut targets = Vec::new();
    let mut rtsp_added = false;
    for proto in &request.protocols {
        match proto {
            DebugProtocol::Rtsp | DebugProtocol::Audio => {
                if rtsp_added {
                    continue;
                }
                rtsp_added = true;
                push_rtsp_stream_targets(
                    &request.target,
                    &request.connection,
                    request.stream,
                    &mut targets,
                );
            }
            DebugProtocol::Onvif => {
                let port = request.target.onvif_port.unwrap_or(8000);
                targets.push((
                    CaptureProtocol::Onvif,
                    "onvif",
                    format!("http://{}:{port}/onvif/device_service", request.target.ip),
                ));
            }
            DebugProtocol::Isapi => {
                let port = request.target.http_port.unwrap_or(80);
                targets.push((
                    CaptureProtocol::Isapi,
                    "isapi",
                    format!("http://{}:{port}", request.target.ip),
                ));
            }
            DebugProtocol::ReolinkHttp => {
                let port = request.target.http_port.unwrap_or(80);
                targets.push((
                    CaptureProtocol::ReolinkHttp,
                    "http",
                    format!("http://{}:{port}/cgi-bin/api.cgi", request.target.ip),
                ));
            }
            DebugProtocol::ReoProto => {
                targets.push((
                    CaptureProtocol::ReoProto,
                    "main",
                    format!("reo://{}:{BAICHUAN_PORT}", request.target.ip),
                ));
            }
        }
    }
    if targets.is_empty() {
        return Err(invalid("no capture protocol targets could be resolved"));
    }
    Ok(targets)
}

fn push_rtsp_stream_targets(
    target: &DebugCameraTarget,
    conn: &CameraDebugConnection,
    stream: DebugStream,
    out: &mut Vec<(CaptureProtocol, &'static str, String)>,
) {
    let main_url = conn
        .main_rtsp_url
        .clone()
        .unwrap_or_else(|| format!("rtsp://{}:554/", target.ip));
    let sub_url = conn
        .sub_rtsp_url
        .clone()
        .unwrap_or_else(|| main_url.clone());
    match stream {
        DebugStream::Main => out.push((CaptureProtocol::Rtsp, "main", main_url)),
        DebugStream::Sub => out.push((CaptureProtocol::Rtsp, "sub", sub_url)),
        DebugStream::Both => {
            out.push((CaptureProtocol::Rtsp, "main", main_url.clone()));
            if sub_url != main_url {
                out.push((CaptureProtocol::Rtsp, "sub", sub_url));
            }
        }
    }
}

fn resolve_primary_capture_with_label(
    target: &DebugCameraTarget,
    conn: &CameraDebugConnection,
    stream: DebugStream,
) -> (CaptureProtocol, &'static str, String) {
    let is_reo = matches!(target.backend.as_str(), "reo-proto" | "reo_proto");
    if is_reo && conn.main_rtsp_url.is_none() && conn.sub_rtsp_url.is_none() {
        return (
            CaptureProtocol::ReoProto,
            "main",
            format!("reo://{}:{BAICHUAN_PORT}", target.ip),
        );
    }
    let mut rtsp = Vec::new();
    push_rtsp_stream_targets(target, conn, stream, &mut rtsp);
    rtsp.into_iter().next().unwrap_or_else(|| {
        (
            CaptureProtocol::Rtsp,
            "main",
            format!("rtsp://{}:554/", target.ip),
        )
    })
}

fn resolve_primary_media_target(
    target: &DebugCameraTarget,
    conn: &CameraDebugConnection,
    stream: DebugStream,
) -> (CaptureProtocol, String) {
    let (proto, _, url) = resolve_primary_capture_with_label(target, conn, stream);
    (proto, url)
}

fn resolve_event_channel_targets(target: &DebugCameraTarget) -> Vec<(CaptureProtocol, String)> {
    let mut out = Vec::new();
    if let Some(port) = target.onvif_port {
        out.push((
            CaptureProtocol::Onvif,
            format!("http://{}:{port}/onvif/device_service", target.ip),
        ));
    }
    if let Some(port) = target.http_port {
        if matches!(target.backend.as_str(), "reo-proto" | "reo_proto") {
            out.push((
                CaptureProtocol::ReolinkHttp,
                format!("http://{}:{port}/cgi-bin/api.cgi", target.ip),
            ));
        } else {
            out.push((
                CaptureProtocol::Isapi,
                format!("http://{}:{port}", target.ip),
            ));
        }
    }
    if matches!(target.backend.as_str(), "reo-proto" | "reo_proto") {
        out.push((
            CaptureProtocol::ReoProto,
            format!("reo://{}:{BAICHUAN_PORT}", target.ip),
        ));
    }
    out
}

const fn parse_camera_transport(raw: &str) -> CameraTransport {
    if raw.eq_ignore_ascii_case("udp") {
        CameraTransport::Udp
    } else {
        CameraTransport::Tcp
    }
}

const fn map_trim_media(mode: TrimMediaMode) -> ReproTrimMediaMode {
    match mode {
        TrimMediaMode::HeadersAndMinimalFailingSlice => {
            ReproTrimMediaMode::HeadersAndMinimalFailingSlice
        }
        TrimMediaMode::SyntheticPayloads => ReproTrimMediaMode::SyntheticPayloads,
        TrimMediaMode::KeepRawFrames => ReproTrimMediaMode::KeepRawFrames,
    }
}

fn build_capture_response(request: &CaptureRequest, results: &[CaptureResult]) -> Value {
    let primary = &results[0];
    let summaries: Vec<Value> = results.iter().map(capture_summary_json).collect();
    json!({
        "action": "capture",
        "camera_id": request.target.camera_id,
        "capture_id": primary.metadata.capture_id,
        "capture_dir": primary.capture_dir,
        "protocol": primary.metadata.protocol,
        "transport": primary.metadata.transport,
        "duration_ms": primary.metadata.duration_ms,
        "total_bytes": primary.metadata.total_bytes,
        "packet_count": primary.metadata.packet_count,
        "include_raw_auth": primary.metadata.include_raw_auth,
        "tls_decrypted": primary.metadata.tls_decrypted,
        "files": primary.metadata.files,
        "bitstream": primary.bitstream,
        "anomalies": primary.metadata.anomalies,
        "warnings": primary.metadata.warnings,
        "requested_artifacts": request.artifacts,
        "captures": summaries,
    })
}

fn capture_summary_json(result: &CaptureResult) -> Value {
    json!({
        "capture_id": result.metadata.capture_id,
        "capture_dir": result.capture_dir,
        "protocol": result.metadata.protocol,
        "transport": result.metadata.transport,
        "duration_ms": result.metadata.duration_ms,
        "total_bytes": result.metadata.total_bytes,
        "packet_count": result.metadata.packet_count,
        "files": result.metadata.files,
        "anomalies": result.metadata.anomalies,
        "warnings": result.metadata.warnings,
    })
}

fn engine_error(action: &str, err: anyhow::Error) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        502,
        format!("camera_debugging '{action}' failed: {err:#}"),
    )
}

fn not_found_capture(capture_id: &str) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::NotFound,
        404,
        format!("capture '{capture_id}' was not found"),
    )
}

/// Output from executing `camera_debugging`, including its redaction boundary decision.
#[derive(Debug)]
pub(super) struct CameraDebugExecution {
    pub(super) result: ToolCallResult,
    pub(super) bypass_redaction: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CameraDebuggingArgs {
    action: DebugAction,
    camera_id: Option<String>,
    capture_id: Option<String>,
    artifact: Option<String>,
    offset: Option<u64>,
    length: Option<usize>,
    encoding: Option<ArtifactEncoding>,
    stream: Option<DebugStream>,
    artifacts: Option<Vec<DebugArtifact>>,
    protocols: Option<Vec<DebugProtocol>>,
    duration_seconds: Option<u64>,
    max_bytes: Option<usize>,
    include_raw_auth: Option<bool>,
    trigger_probe: Option<bool>,
    trace_filter: Option<String>,
    mode: Option<CompareMode>,
    strip_credentials: Option<bool>,
    anonymize_network: Option<bool>,
    trim_media: Option<TrimMediaMode>,
    delete_all: Option<bool>,
}

/// Executes one `camera_debugging` MCP tool call under strict policy gates.
pub(super) fn execute(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    classification: ClientClassificationReason,
    read_only_header: bool,
    deadline: Instant,
    raw_args: Value,
) -> Result<CameraDebugExecution, ControlCommandError> {
    let args = check_policy_and_parse(
        state,
        principal,
        classification,
        read_only_header,
        deadline,
        raw_args,
    )?;
    let action = args.action;
    let outcome = dispatch_action(
        state,
        router_tx,
        principal,
        classification,
        &state.mcp.config(),
        deadline,
        args,
    );
    match outcome {
        Ok((payload, bypass_redaction)) => {
            let target = payload.get("camera_id").and_then(Value::as_str);
            audit_debug(
                state,
                principal,
                classification,
                action.as_str(),
                target,
                "success",
            );
            Ok(CameraDebugExecution {
                result: ToolCallResult::json(payload),
                bypass_redaction,
            })
        }
        Err(error) => {
            audit_debug(
                state,
                principal,
                classification,
                action.as_str(),
                None,
                "failed",
            );
            Err(error)
        }
    }
}

fn check_policy_and_parse(
    state: &ServerState,
    principal: &ApiPrincipal,
    classification: ClientClassificationReason,
    read_only_header: bool,
    deadline: Instant,
    raw_args: Value,
) -> Result<CameraDebuggingArgs, ControlCommandError> {
    require_admin(principal)?;
    if !state.mcp.config().allow_camera_debugging {
        audit_debug(
            state,
            principal,
            classification,
            "gate",
            None,
            "denied_disabled",
        );
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "MCP camera debugging is disabled in [mcp].allow_camera_debugging",
        ));
    }
    let args: CameraDebuggingArgs = serde_json::from_value(raw_args)
        .map_err(|error| invalid(&format!("invalid camera_debugging params: {error}")))?;
    if read_only_header && args.action.blocked_in_read_only_mode() {
        let action_name = args.action.as_str();
        audit_debug(
            state,
            principal,
            classification,
            action_name,
            args.camera_id.as_deref(),
            "denied_read_only",
        );
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            format!(
                "camera_debugging action '{action_name}' is blocked while X-KeepPeek-MCP-Read-Only is active"
            ),
        ));
    }
    remaining_timeout(deadline)?;
    Ok(args)
}

struct ActionContext<'a> {
    state: &'a ServerState,
    principal: &'a ApiPrincipal,
    classification: ClientClassificationReason,
    cfg: &'a McpConfig,
    config_dir: Option<PathBuf>,
    deadline: Instant,
}

fn dispatch_action(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    classification: ClientClassificationReason,
    cfg: &McpConfig,
    deadline: Instant,
    args: CameraDebuggingArgs,
) -> Result<(Value, bool), ControlCommandError> {
    let ctx = ActionContext {
        state,
        principal,
        classification,
        cfg,
        config_dir: config_directory(state),
        deadline,
    };
    match args.action {
        DebugAction::InspectPipeline => inspect_pipeline(&ctx, router_tx, &args),
        DebugAction::Capture => {
            let include_raw_auth = args.include_raw_auth.unwrap_or(true);
            let request = build_capture_request(&ctx, &args, include_raw_auth)?;
            let payload = state.mcp.camera_debug().capture(request)?;
            Ok((payload, include_raw_auth))
        }
        DebugAction::CompareTransports => {
            let include_raw_auth = args.include_raw_auth.unwrap_or(true);
            let request = build_compare_request(&ctx, &args, include_raw_auth)?;
            let payload = state.mcp.camera_debug().compare_transports(request)?;
            Ok((payload, include_raw_auth))
        }
        DebugAction::ExportReproFixture => {
            let strip_credentials = args.strip_credentials.unwrap_or(true);
            let request = build_export_request(&ctx, &args, strip_credentials)?;
            let payload = state.mcp.camera_debug().export_repro_fixture(request)?;
            Ok((payload, !strip_credentials))
        }
        DebugAction::ListCaptures => {
            let camera_id = optional_allowed_camera_id(
                ctx.state,
                ctx.principal,
                ctx.classification,
                ctx.cfg,
                args.camera_id.as_deref(),
            )?;
            let request = ListCapturesRequest {
                camera_id,
                allowed_camera_ids: allowed_camera_scope(ctx.state, ctx.cfg),
                config_dir: ctx.config_dir,
            };
            let payload = state.mcp.camera_debug().list_captures(request)?;
            Ok((payload, false))
        }
        DebugAction::DeleteCapture => {
            let request = build_delete_request(&ctx, &args)?;
            let payload = state.mcp.camera_debug().delete_capture(request)?;
            Ok((payload, false))
        }
        DebugAction::ReadArtifact => {
            let request = build_read_artifact_request(&ctx, &args)?;
            let payload = state.mcp.camera_debug().read_artifact(request)?;
            Ok((payload, true))
        }
    }
}

fn build_read_artifact_request(
    ctx: &ActionContext<'_>,
    args: &CameraDebuggingArgs,
) -> Result<ReadArtifactRequest, ControlCommandError> {
    let capture_id = validate_capture_id(args.capture_id.as_deref())?.to_owned();
    let artifact = validate_artifact_name(args.artifact.as_deref())?.to_owned();
    let offset = args.offset.unwrap_or(0);
    let length = parse_read_artifact_length(args.length)?;
    let encoding = args.encoding.unwrap_or(ArtifactEncoding::Auto);
    Ok(ReadArtifactRequest {
        capture_id,
        artifact,
        offset,
        length,
        encoding,
        allowed_camera_ids: allowed_camera_scope(ctx.state, ctx.cfg),
        config_dir: ctx.config_dir.clone(),
    })
}

fn build_capture_request(
    ctx: &ActionContext<'_>,
    args: &CameraDebuggingArgs,
    include_raw_auth: bool,
) -> Result<CaptureRequest, ControlCommandError> {
    let camera_id = required_camera_id(args.camera_id.as_deref())?;
    let camera = resolve_allowed_camera(
        ctx.state,
        ctx.principal,
        ctx.classification,
        ctx.cfg,
        camera_id,
    )?;
    let stream = args.stream.unwrap_or(DebugStream::Main);
    let artifacts = parse_artifacts(args.artifacts.as_deref())?;
    let protocols_explicit = args.protocols.is_some();
    let protocols = parse_protocols(args.protocols.as_deref())?;
    let duration_seconds = parse_duration_seconds(args.duration_seconds)?;
    let max_bytes = parse_max_bytes(args.max_bytes, ctx.cfg)?;
    let trace_filter = parse_trace_filter(args.trace_filter.as_deref())?;
    let credentials = include_raw_auth.then(|| CameraDebugCredentials::from_camera(&camera));
    Ok(CaptureRequest {
        target: DebugCameraTarget::from_camera(&camera, include_raw_auth),
        stream,
        artifacts,
        protocols,
        protocols_explicit,
        duration_seconds,
        max_bytes,
        max_storage_bytes: ctx.cfg.max_debug_storage_bytes,
        include_raw_auth,
        trigger_probe: args.trigger_probe.unwrap_or(false),
        trace_filter,
        config_dir: ctx.config_dir.clone(),
        logging: ctx.state.logging.clone(),
        credentials,
        connection: CameraDebugConnection::from_camera(&camera),
        deadline: ctx.deadline,
    })
}

fn build_compare_request(
    ctx: &ActionContext<'_>,
    args: &CameraDebuggingArgs,
    include_raw_auth: bool,
) -> Result<CompareTransportsRequest, ControlCommandError> {
    let camera_id = required_camera_id(args.camera_id.as_deref())?;
    let camera = resolve_allowed_camera(
        ctx.state,
        ctx.principal,
        ctx.classification,
        ctx.cfg,
        camera_id,
    )?;
    let stream = args.stream.unwrap_or(DebugStream::Main);
    let mode = args.mode.unwrap_or(CompareMode::MediaTransports);
    let duration_seconds = parse_duration_seconds(args.duration_seconds)?;
    let max_bytes = parse_max_bytes(args.max_bytes, ctx.cfg)?;
    let trace_filter = parse_trace_filter(args.trace_filter.as_deref())?;
    let credentials = include_raw_auth.then(|| CameraDebugCredentials::from_camera(&camera));
    Ok(CompareTransportsRequest {
        target: DebugCameraTarget::from_camera(&camera, include_raw_auth),
        stream,
        mode,
        duration_seconds,
        max_bytes,
        max_storage_bytes: ctx.cfg.max_debug_storage_bytes,
        include_raw_auth,
        trace_filter,
        config_dir: ctx.config_dir.clone(),
        logging: ctx.state.logging.clone(),
        credentials,
        connection: CameraDebugConnection::from_camera(&camera),
        deadline: ctx.deadline,
    })
}

fn build_export_request(
    ctx: &ActionContext<'_>,
    args: &CameraDebuggingArgs,
    strip_credentials: bool,
) -> Result<ExportReproFixtureRequest, ControlCommandError> {
    let capture_id = validate_capture_id(args.capture_id.as_deref())?.to_owned();
    let camera_id = optional_allowed_camera_id(
        ctx.state,
        ctx.principal,
        ctx.classification,
        ctx.cfg,
        args.camera_id.as_deref(),
    )?;
    let max_bytes = parse_max_bytes(args.max_bytes, ctx.cfg)?;
    let extra_secrets = if strip_credentials {
        collect_camera_secrets(ctx.state)
    } else {
        ZeroizedSecretList::default()
    };
    Ok(ExportReproFixtureRequest {
        capture_id,
        camera_id,
        allowed_camera_ids: allowed_camera_scope(ctx.state, ctx.cfg),
        strip_credentials,
        anonymize_network: args.anonymize_network.unwrap_or(true),
        trim_media: args
            .trim_media
            .unwrap_or(TrimMediaMode::HeadersAndMinimalFailingSlice),
        max_bytes,
        config_dir: ctx.config_dir.clone(),
        extra_secrets,
        deadline: ctx.deadline,
    })
}

fn build_delete_request(
    ctx: &ActionContext<'_>,
    args: &CameraDebuggingArgs,
) -> Result<DeleteCaptureRequest, ControlCommandError> {
    let delete_all = args.delete_all.unwrap_or(false);
    let capture_id = args
        .capture_id
        .as_deref()
        .map(|raw| validate_capture_id(Some(raw)).map(ToOwned::to_owned))
        .transpose()?;
    if delete_all && capture_id.is_some() {
        return Err(invalid(
            "specify either capture_id or delete_all=true, not both",
        ));
    }
    if !delete_all && capture_id.is_none() {
        return Err(invalid(
            "delete_capture requires capture_id or delete_all=true",
        ));
    }
    let camera_id = optional_allowed_camera_id(
        ctx.state,
        ctx.principal,
        ctx.classification,
        ctx.cfg,
        args.camera_id.as_deref(),
    )?;
    Ok(DeleteCaptureRequest {
        capture_id,
        camera_id,
        delete_all,
        allowed_camera_ids: allowed_camera_scope(ctx.state, ctx.cfg),
        config_dir: ctx.config_dir.clone(),
    })
}

fn inspect_pipeline(
    ctx: &ActionContext<'_>,
    router_tx: &FacadeSender<RouterMessage>,
    args: &CameraDebuggingArgs,
) -> Result<(Value, bool), ControlCommandError> {
    let include_raw_auth = args.include_raw_auth.unwrap_or(false);
    let cameras = resolve_inspect_cameras(ctx, args.camera_id.as_deref())?;
    let (router_statuses, router_available) = read_router_statuses(router_tx);
    let pipelines =
        collect_camera_pipelines(ctx.state, &cameras, &router_statuses, include_raw_auth);
    let single_camera_id = (cameras.len() == 1).then(|| cameras[0].info.id.clone());
    let payload = json!({
        "action": "inspect_pipeline",
        "camera_id": single_camera_id,
        "router_available": router_available,
        "scoped_debug_camera_ids": ctx.cfg.debug_camera_ids,
        "include_raw_auth": include_raw_auth,
        "count": pipelines.len(),
        "cameras": pipelines,
    });
    Ok((payload, include_raw_auth))
}

fn resolve_inspect_cameras(
    ctx: &ActionContext<'_>,
    raw_camera_id: Option<&str>,
) -> Result<Vec<CameraEntry>, ControlCommandError> {
    match raw_camera_id {
        Some(raw_id) => {
            let camera_id = validate_camera_id(raw_id)?;
            Ok(vec![resolve_allowed_camera(
                ctx.state,
                ctx.principal,
                ctx.classification,
                ctx.cfg,
                camera_id,
            )?])
        }
        None => Ok(ctx
            .state
            .camera_entries()
            .into_iter()
            .filter(|camera| camera_entry_allowed(ctx.cfg, camera))
            .take(INSPECT_CAMERAS_MAX)
            .collect()),
    }
}

fn collect_camera_pipelines(
    state: &ServerState,
    cameras: &[CameraEntry],
    router_statuses: &HashMap<String, CameraStatus>,
    include_raw_auth: bool,
) -> Vec<Value> {
    let mut ingress_by_ip = state
        .health
        .snapshot()
        .into_iter()
        .map(|report| (report.ip, report))
        .collect::<HashMap<_, _>>();
    let recording_by_stream = state
        .recording_health
        .snapshot()
        .streams
        .into_iter()
        .map(|stream| (stream.stream_id.clone(), stream))
        .collect::<HashMap<_, _>>();
    let mut pipelines = Vec::with_capacity(cameras.len());
    for camera in cameras {
        let ingress = camera
            .info
            .ip
            .parse::<IpAddr>()
            .ok()
            .and_then(|ip| ingress_by_ip.remove(&ip))
            .map_or_else(Vec::new, |report| report.streams);
        pipelines.push(build_camera_pipeline_json(
            state,
            camera,
            router_statuses.get(&camera.recording_label),
            &ingress,
            &recording_by_stream,
            include_raw_auth,
        ));
    }
    pipelines
}

fn build_camera_pipeline_json(
    state: &ServerState,
    camera: &CameraEntry,
    router_status: Option<&CameraStatus>,
    ingress: &[StreamHealthReport],
    recording_by_stream: &HashMap<String, RecordingStreamHealthSnapshot>,
    include_raw_auth: bool,
) -> Value {
    let info = state.camera_info(camera);
    let ip = camera.info.ip.parse::<IpAddr>().ok();
    let event_evidence = ip.and_then(|addr| state.health.events.snapshot(addr));
    let privacy = state
        .privacy
        .decision(&camera.info.id, chrono::Utc::now())
        .ok()
        .map(|(active, epoch)| json!({ "active": active, "epoch": epoch }));
    let main_rec_id = format!("{}/main", camera.recording_label);
    let sub_rec_id = format!("{}/sub", camera.recording_label);
    let recording_streams = [main_rec_id, sub_rec_id]
        .into_iter()
        .filter_map(|stream_id| recording_by_stream.get(&stream_id))
        .collect::<Vec<_>>();
    let debug_metrics = state
        .mcp
        .camera_debug()
        .pipeline_debug_metrics(&camera.info.id);
    let auth_snapshot = if include_raw_auth {
        json!({
            "username": camera.configuration.username,
            "password": camera.configuration.password,
            "uid": camera.configuration.uid,
            "main_rtsp_url": camera.configuration.main_rtsp_url,
            "sub_rtsp_url": camera.configuration.sub_rtsp_url,
        })
    } else {
        json!({
            "username_configured": !camera.configuration.username.is_empty(),
            "password_configured": !camera.configuration.password.is_empty(),
            "uid_configured": camera.configuration.uid.is_some(),
            "main_rtsp_url": camera.configuration.main_rtsp_url,
            "sub_rtsp_url": camera.configuration.sub_rtsp_url,
        })
    };
    json!({
        "camera_id": info.id,
        "ip": info.ip,
        "name": info.name,
        "recording_label": camera.recording_label,
        "manufacturer": info.manufacturer,
        "model": info.model,
        "firmware_version": info.firmware_version,
        "backend": info.backend,
        "transport": info.transport,
        "recording_mode": format!("{:?}", camera.configuration.recording_mode).to_lowercase(),
        "ports": info.ports,
        "capabilities": info.capabilities,
        "profiles": info.profiles,
        "router_status": router_status,
        "ingress_streams": ingress,
        "recording_streams": recording_streams,
        "event_channel": event_evidence,
        "privacy": privacy,
        "auth": auth_snapshot,
        "debug_metrics": debug_metrics,
    })
}

fn read_router_statuses(
    router_tx: &FacadeSender<RouterMessage>,
) -> (HashMap<String, CameraStatus>, bool) {
    match query_router(router_tx, RouterQuery::ListCameras) {
        Ok(RouterResponse::Cameras(statuses)) => (
            statuses
                .into_iter()
                .map(|status| (status.id.to_string(), status))
                .collect(),
            true,
        ),
        Ok(RouterResponse::Camera(_)) | Err(_) => (HashMap::new(), false),
    }
}

fn require_admin(principal: &ApiPrincipal) -> Result<(), ControlCommandError> {
    if principal.role == AccessRole::Administrator {
        return Ok(());
    }
    Err(ControlCommandError::new(
        proto::ErrorCode::Rejected,
        403,
        "Administrator access is required for camera_debugging",
    ))
}

fn resolve_allowed_camera(
    state: &ServerState,
    principal: &ApiPrincipal,
    classification: ClientClassificationReason,
    cfg: &McpConfig,
    camera_id: &str,
) -> Result<CameraEntry, ControlCommandError> {
    if let Some(camera) = state.camera(camera_id) {
        if !camera_entry_allowed(cfg, &camera) {
            audit_debug(
                state,
                principal,
                classification,
                "allowlist",
                Some(camera_id),
                "denied_allowlist",
            );
            return Err(allowlist_denied(camera_id));
        }
        return Ok(camera);
    }
    if !cfg.allows_debug_camera(camera_id) {
        audit_debug(
            state,
            principal,
            classification,
            "allowlist",
            Some(camera_id),
            "denied_allowlist",
        );
        return Err(allowlist_denied(camera_id));
    }
    Err(ControlCommandError::new(
        proto::ErrorCode::NotFound,
        404,
        format!("camera '{camera_id}' was not found"),
    ))
}

fn optional_allowed_camera_id(
    state: &ServerState,
    principal: &ApiPrincipal,
    classification: ClientClassificationReason,
    cfg: &McpConfig,
    raw_id: Option<&str>,
) -> Result<Option<String>, ControlCommandError> {
    let Some(raw) = raw_id else {
        return Ok(None);
    };
    let camera_id = validate_camera_id(raw)?;
    if let Some(camera) = state.camera(camera_id) {
        if !camera_entry_allowed(cfg, &camera) {
            audit_debug(
                state,
                principal,
                classification,
                "allowlist",
                Some(camera_id),
                "denied_allowlist",
            );
            return Err(allowlist_denied(camera_id));
        }
        return Ok(Some(camera.info.id));
    }
    if !cfg.allows_debug_camera(camera_id) {
        audit_debug(
            state,
            principal,
            classification,
            "allowlist",
            Some(camera_id),
            "denied_allowlist",
        );
        return Err(allowlist_denied(camera_id));
    }
    Ok(Some(camera_id.to_owned()))
}

fn camera_entry_allowed(cfg: &McpConfig, camera: &CameraEntry) -> bool {
    cfg.debug_camera_ids.is_empty()
        || cfg
            .debug_camera_ids
            .iter()
            .any(|allowed| allowed == &camera.info.id || allowed == &camera.recording_label)
}

fn allowed_camera_scope(state: &ServerState, cfg: &McpConfig) -> Option<Vec<String>> {
    if cfg.debug_camera_ids.is_empty() {
        return None;
    }
    let mut scope = cfg.debug_camera_ids.clone();
    for camera in state.camera_entries() {
        if camera_entry_allowed(cfg, &camera) && !scope.contains(&camera.info.id) {
            scope.push(camera.info.id);
        }
    }
    Some(scope)
}

fn push_unique_secret(secrets: &mut Vec<String>, candidate: &str) {
    if !candidate.is_empty() && !secrets.iter().any(|existing| existing == candidate) {
        secrets.push(candidate.to_owned());
    }
}

fn push_url_credentials(secrets: &mut Vec<String>, raw_url: Option<&str>) {
    let Some(raw) = raw_url else {
        return;
    };
    let Ok(parsed) = Url::parse(raw) else {
        return;
    };
    push_unique_secret(secrets, parsed.username());
    if let Some(password) = parsed.password() {
        push_unique_secret(secrets, password);
    }
}

fn collect_camera_secrets(state: &ServerState) -> ZeroizedSecretList {
    let mut secrets = Vec::new();
    for secret in state
        .mcp
        .secrets_snapshot(state.camera_config_path.as_deref())
        .redaction_values()
    {
        push_unique_secret(&mut secrets, &secret);
    }
    for camera in state.camera_entries() {
        push_unique_secret(&mut secrets, &camera.configuration.username);
        push_unique_secret(&mut secrets, &camera.configuration.password);
        if let Some(uid) = camera.configuration.uid.as_deref() {
            push_unique_secret(&mut secrets, uid);
        }
        push_url_credentials(&mut secrets, camera.configuration.main_rtsp_url.as_deref());
        push_url_credentials(&mut secrets, camera.configuration.sub_rtsp_url.as_deref());
    }
    ZeroizedSecretList(secrets)
}

fn allowlist_denied(camera_id: &str) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        403,
        format!("camera '{camera_id}' is not permitted by [mcp].debug_camera_ids"),
    )
}

fn required_camera_id(raw: Option<&str>) -> Result<&str, ControlCommandError> {
    let raw = raw.ok_or_else(|| invalid("camera_id is required for this action"))?;
    validate_camera_id(raw)
}

fn validate_camera_id(raw: &str) -> Result<&str, ControlCommandError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > CAMERA_ID_MAX_BYTES
        || trimmed.chars().any(char::is_control)
    {
        return Err(invalid(
            "camera_id must contain 1 to 128 printable characters",
        ));
    }
    Ok(trimmed)
}

fn validate_capture_id(raw: Option<&str>) -> Result<&str, ControlCommandError> {
    let raw = raw.ok_or_else(|| invalid("capture_id is required for this action"))?;
    let trimmed = raw.trim();
    if validate_store_capture_id(trimmed).is_err() {
        return Err(invalid(
            "capture_id must be 1 to 128 safe ASCII characters ([A-Za-z0-9_-])",
        ));
    }
    Ok(trimmed)
}

fn validate_artifact_name(raw: Option<&str>) -> Result<&str, ControlCommandError> {
    let raw = raw.ok_or_else(|| invalid("artifact is required for read_artifact"))?;
    let trimmed = raw.trim();
    if validate_store_artifact_name(trimmed).is_err() {
        return Err(invalid(
            "artifact must be a single safe filename up to 128 bytes without /, \\, .., or NUL",
        ));
    }
    Ok(trimmed)
}

fn parse_read_artifact_length(raw: Option<usize>) -> Result<usize, ControlCommandError> {
    let value = raw.unwrap_or(READ_ARTIFACT_LENGTH_DEFAULT);
    if !(READ_ARTIFACT_LENGTH_MIN..=READ_ARTIFACT_LENGTH_MAX).contains(&value) {
        return Err(invalid(&format!(
            "length must be between {READ_ARTIFACT_LENGTH_MIN} and {READ_ARTIFACT_LENGTH_MAX}"
        )));
    }
    Ok(value)
}

fn parse_trace_filter(raw: Option<&str>) -> Result<Option<String>, ControlCommandError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > TRACE_FILTER_MAX_BYTES {
        return Err(invalid(&format!(
            "trace_filter must be between 1 and {TRACE_FILTER_MAX_BYTES} bytes"
        )));
    }
    EnvFilter::try_new(trimmed)
        .map_err(|error| invalid(&format!("invalid trace_filter: {error}")))?;
    Ok(Some(trimmed.to_owned()))
}

fn parse_artifacts(
    raw: Option<&[DebugArtifact]>,
) -> Result<Vec<DebugArtifact>, ControlCommandError> {
    let Some(items) = raw else {
        return Ok(vec![
            DebugArtifact::Pcapng,
            DebugArtifact::VideoBitstream,
            DebugArtifact::AudioBitstream,
            DebugArtifact::MuxedMp4,
            DebugArtifact::ProtocolTranscript,
            DebugArtifact::Traces,
        ]);
    };
    if items.is_empty() || items.len() > ARTIFACTS_MAX {
        return Err(invalid("artifacts must contain 1 to 6 entries"));
    }
    let mut deduped = Vec::with_capacity(items.len());
    for item in items {
        if !deduped.contains(item) {
            deduped.push(*item);
        }
    }
    Ok(deduped)
}

fn parse_protocols(
    raw: Option<&[DebugProtocol]>,
) -> Result<Vec<DebugProtocol>, ControlCommandError> {
    let Some(items) = raw else {
        return Ok(vec![
            DebugProtocol::Rtsp,
            DebugProtocol::Audio,
            DebugProtocol::Onvif,
            DebugProtocol::Isapi,
            DebugProtocol::ReolinkHttp,
            DebugProtocol::ReoProto,
        ]);
    };
    if items.is_empty() || items.len() > PROTOCOLS_MAX {
        return Err(invalid("protocols must contain 1 to 6 entries"));
    }
    let mut deduped = Vec::with_capacity(items.len());
    for item in items {
        if !deduped.contains(item) {
            deduped.push(*item);
        }
    }
    Ok(deduped)
}

fn parse_duration_seconds(raw: Option<u64>) -> Result<u64, ControlCommandError> {
    let value = raw.unwrap_or(DURATION_SECONDS_DEFAULT);
    if !(DURATION_SECONDS_MIN..=DURATION_SECONDS_MAX).contains(&value) {
        return Err(invalid(&format!(
            "duration_seconds must be between {DURATION_SECONDS_MIN} and {DURATION_SECONDS_MAX}"
        )));
    }
    Ok(value)
}

fn parse_max_bytes(raw: Option<usize>, cfg: &McpConfig) -> Result<usize, ControlCommandError> {
    let default_limit = cfg
        .max_debug_capture_bytes
        .clamp(CAPTURE_BYTES_MIN, CAPTURE_BYTES_MAX);
    let value = raw.unwrap_or(default_limit);
    if !(CAPTURE_BYTES_MIN..=CAPTURE_BYTES_MAX).contains(&value) {
        return Err(invalid(&format!(
            "max_bytes must be between {CAPTURE_BYTES_MIN} and {CAPTURE_BYTES_MAX}"
        )));
    }
    if value > cfg.max_debug_capture_bytes {
        return Err(invalid(&format!(
            "max_bytes ({value}) exceeds configured [mcp].max_debug_capture_bytes ({})",
            cfg.max_debug_capture_bytes
        )));
    }
    Ok(value)
}

fn remaining_timeout(deadline: Instant) -> Result<Duration, ControlCommandError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Timeout,
            504,
            "camera debugging request exceeded its timeout deadline",
        ));
    }
    Ok(deadline.saturating_duration_since(now))
}

fn config_directory(state: &ServerState) -> Option<PathBuf> {
    state
        .camera_config_path
        .as_deref()
        .and_then(Path::parent)
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

fn audit_debug(
    state: &ServerState,
    principal: &ApiPrincipal,
    classification: ClientClassificationReason,
    action: &str,
    target_id: Option<&str>,
    result: &str,
) {
    let event = format!("mcp_camera_debugging_{action}");
    record_access_audit(
        state,
        i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
        Some(&principal.id()),
        Some(principal.role),
        &event,
        target_id,
        result,
        classification,
    );
}

fn invalid(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cameras::{CameraBackend, CameraConfig, CameraRecordingMode},
        config::Config,
        runtime::Router,
        storage::{RecordingDemand, StorageConfig},
        webrtc::WebRtc,
    };
    use std::sync::Arc;

    struct MockCameraDebugService {
        last_capture: Mutex<Option<CaptureRequest>>,
        last_compare: Mutex<Option<CompareTransportsRequest>>,
        last_export: Mutex<Option<ExportReproFixtureRequest>>,
        last_list: Mutex<Option<ListCapturesRequest>>,
        last_delete: Mutex<Option<DeleteCaptureRequest>>,
        metrics: Option<Value>,
    }

    impl MockCameraDebugService {
        fn new(metrics: Option<Value>) -> Self {
            Self {
                last_capture: Mutex::new(None),
                last_compare: Mutex::new(None),
                last_export: Mutex::new(None),
                last_list: Mutex::new(None),
                last_delete: Mutex::new(None),
                metrics,
            }
        }
    }

    impl CameraDebugService for MockCameraDebugService {
        fn capture(&self, request: CaptureRequest) -> Result<Value, ControlCommandError> {
            let camera_id = request.target.camera_id.clone();
            let raw_pass = request.credentials.as_ref().map(|c| c.password.clone());
            *self.last_capture.lock().unwrap() = Some(request);
            Ok(json!({
                "action": "capture",
                "camera_id": camera_id,
                "capture_id": "cap-001",
                "raw_password": raw_pass,
            }))
        }

        fn compare_transports(
            &self,
            request: CompareTransportsRequest,
        ) -> Result<Value, ControlCommandError> {
            let camera_id = request.target.camera_id.clone();
            *self.last_compare.lock().unwrap() = Some(request);
            Ok(json!({
                "action": "compare_transports",
                "camera_id": camera_id,
            }))
        }

        fn export_repro_fixture(
            &self,
            request: ExportReproFixtureRequest,
        ) -> Result<Value, ControlCommandError> {
            let capture_id = request.capture_id.clone();
            let strip_credentials = request.strip_credentials;
            *self.last_export.lock().unwrap() = Some(request);
            Ok(json!({
                "action": "export_repro_fixture",
                "capture_id": capture_id,
                "strip_credentials": strip_credentials,
                "password": "CAMERA-SECRET-VALUE",
            }))
        }

        fn list_captures(
            &self,
            request: ListCapturesRequest,
        ) -> Result<Value, ControlCommandError> {
            *self.last_list.lock().unwrap() = Some(request);
            Ok(json!({
                "action": "list_captures",
                "captures": [],
            }))
        }

        fn delete_capture(
            &self,
            request: DeleteCaptureRequest,
        ) -> Result<Value, ControlCommandError> {
            *self.last_delete.lock().unwrap() = Some(request);
            Ok(json!({
                "action": "delete_capture",
                "deleted": 1,
            }))
        }

        fn read_artifact(
            &self,
            request: ReadArtifactRequest,
        ) -> Result<Value, ControlCommandError> {
            Ok(json!({
                "action": "read_artifact",
                "capture_id": request.capture_id,
                "artifact": request.artifact,
                "offset": request.offset,
                "length": 0,
                "total_bytes": 0,
                "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "eof": true,
                "next_offset": request.offset,
                "encoding": "utf8",
                "data": "",
            }))
        }

        fn pipeline_debug_metrics(&self, camera_id: &str) -> Option<Value> {
            self.metrics
                .as_ref()
                .map(|m| json!({ "camera_id": camera_id, "metrics": m }))
        }
    }

    fn test_state(
        mcp: McpConfig,
        service: Arc<dyn CameraDebugService>,
    ) -> (ServerState, FacadeSender<RouterMessage>) {
        let cam_a = CameraConfig {
            events: Default::default(),
            ip: "192.0.2.10".parse().unwrap(),
            name: Some("front-door".to_owned()),
            display_name: Some("Front Door".to_owned()),
            manufacturer: None,
            username: "admin".to_owned(),
            password: "CAMERA-SECRET-VALUE".to_owned(),
            onvif_port: Some(8000),
            http_port: Some(80),
            main_rtsp_url: Some("rtsp://admin:CAMERA-SECRET-VALUE@192.0.2.10:554/main".to_owned()),
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Retina,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: CameraRecordingMode::Off,
            event_recording_duration_secs: 60,
            event_pre_recording_duration_secs: 0,
            event_recording_stream: Default::default(),
            recorded_audio: Default::default(),
            live_audio: Default::default(),
        };
        let cam_b = CameraConfig {
            ip: "192.0.2.11".parse().unwrap(),
            name: Some("backyard".to_owned()),
            display_name: Some("Backyard".to_owned()),
            ..cam_a.clone()
        };
        let config = Config {
            mcp: mcp.clone(),
            ..Config::default()
        };
        let storage = StorageConfig::default();
        let cameras = HashMap::from([("cameras".to_owned(), vec![cam_a, cam_b])]);
        let state = ServerState::new(
            &config,
            &cameras,
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        )
        .with_mcp_config(mcp)
        .with_camera_debug_service(service);
        let (_router, router_tx) = Router::new().unwrap();
        (state, router_tx)
    }

    fn admin() -> ApiPrincipal {
        ApiPrincipal::local(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
    }

    fn user() -> ApiPrincipal {
        let mut principal = ApiPrincipal::local(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        principal.role = AccessRole::User;
        principal
    }

    #[test]
    fn rejects_non_admin_and_disabled_gate_fail_closed() {
        let mock = Arc::new(MockCameraDebugService::new(None));
        let (state, router_tx) = test_state(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                ..McpConfig::default()
            },
            mock.clone(),
        );
        let err = execute(
            &state,
            &router_tx,
            &user(),
            ClientClassificationReason::DirectLocal,
            false,
            Instant::now() + Duration::from_secs(5),
            json!({ "action": "inspect_pipeline" }),
        )
        .expect_err("User role must be denied");
        assert_eq!(err._http_status, 403);

        let (disabled_state, router_tx) = test_state(
            McpConfig {
                enabled: true,
                allow_camera_debugging: false,
                ..McpConfig::default()
            },
            mock,
        );
        let err = execute(
            &disabled_state,
            &router_tx,
            &admin(),
            ClientClassificationReason::DirectLocal,
            false,
            Instant::now() + Duration::from_secs(5),
            json!({ "action": "inspect_pipeline" }),
        )
        .expect_err("disabled allow_camera_debugging must fail closed");
        assert_eq!(err._http_status, 403);
    }

    #[test]
    fn enforces_read_only_header_only_on_mutating_or_active_actions() {
        let mock = Arc::new(MockCameraDebugService::new(None));
        let (state, router_tx) = test_state(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                ..McpConfig::default()
            },
            mock,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        for blocked in [
            json!({ "action": "capture", "camera_id": "192.0.2.10" }),
            json!({ "action": "compare_transports", "camera_id": "192.0.2.10" }),
            json!({ "action": "delete_capture", "capture_id": "cap-001" }),
        ] {
            let err = execute(
                &state,
                &router_tx,
                &admin(),
                ClientClassificationReason::DirectLocal,
                true,
                deadline,
                blocked,
            )
            .expect_err("read-only header must reject action");
            assert_eq!(err._http_status, 403);
        }

        for allowed in [
            json!({ "action": "inspect_pipeline", "camera_id": "192.0.2.10" }),
            json!({ "action": "list_captures" }),
            json!({ "action": "export_repro_fixture", "capture_id": "cap-001" }),
        ] {
            assert!(
                execute(
                    &state,
                    &router_tx,
                    &admin(),
                    ClientClassificationReason::DirectLocal,
                    true,
                    deadline,
                    allowed,
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn enforces_debug_camera_ids_allowlist_and_bounds() {
        let mock = Arc::new(MockCameraDebugService::new(Some(json!({ "packets": 42 }))));
        let (state, router_tx) = test_state(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                max_debug_capture_bytes: 2_097_152,
                debug_camera_ids: vec!["192.0.2.10".to_owned()],
                ..McpConfig::default()
            },
            mock,
        );
        let deadline = Instant::now() + Duration::from_secs(5);

        let denied = execute(
            &state,
            &router_tx,
            &admin(),
            ClientClassificationReason::DirectLocal,
            false,
            deadline,
            json!({ "action": "capture", "camera_id": "192.0.2.11" }),
        )
        .expect_err("unlisted camera must be denied");
        assert_eq!(denied._http_status, 403);

        let inspected = execute(
            &state,
            &router_tx,
            &admin(),
            ClientClassificationReason::DirectLocal,
            false,
            deadline,
            json!({ "action": "inspect_pipeline" }),
        )
        .expect("inspect_pipeline should filter to allowlist");
        let content = inspected.result.structured_content.unwrap();
        assert_eq!(content["count"], 1);
        assert_eq!(content["cameras"][0]["camera_id"], "192.0.2.10");
        assert_eq!(
            content["cameras"][0]["debug_metrics"]["metrics"]["packets"],
            42
        );
        assert!(!inspected.bypass_redaction);

        let invalid_bounds = [
            json!({ "action": "capture", "camera_id": "192.0.2.10", "duration_seconds": 0 }),
            json!({ "action": "capture", "camera_id": "192.0.2.10", "duration_seconds": 61 }),
            json!({ "action": "capture", "camera_id": "192.0.2.10", "max_bytes": 1024 }),
            json!({ "action": "capture", "camera_id": "192.0.2.10", "max_bytes": 4_194_304 }),
            json!({ "action": "export_repro_fixture", "capture_id": "../escape" }),
            json!({ "action": "delete_capture", "capture_id": "cap-1", "delete_all": true }),
            json!({ "action": "delete_capture" }),
        ];
        for bad in invalid_bounds {
            let err = execute(
                &state,
                &router_tx,
                &admin(),
                ClientClassificationReason::DirectLocal,
                false,
                deadline,
                bad,
            )
            .expect_err("invalid bounded argument must fail");
            assert_eq!(err._http_status, 400);
        }
    }

    #[test]
    fn redaction_bypass_matches_explicit_raw_intent() {
        let mock = Arc::new(MockCameraDebugService::new(None));
        let (state, router_tx) = test_state(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                ..McpConfig::default()
            },
            mock,
        );
        let deadline = Instant::now() + Duration::from_secs(5);

        let cap_default = execute(
            &state,
            &router_tx,
            &admin(),
            ClientClassificationReason::DirectLocal,
            false,
            deadline,
            json!({ "action": "capture", "camera_id": "192.0.2.10" }),
        )
        .unwrap();
        assert!(cap_default.bypass_redaction);

        let cap_redacted = execute(
            &state,
            &router_tx,
            &admin(),
            ClientClassificationReason::DirectLocal,
            false,
            deadline,
            json!({ "action": "capture", "camera_id": "192.0.2.10", "include_raw_auth": false }),
        )
        .unwrap();
        assert!(!cap_redacted.bypass_redaction);

        let export_default = execute(
            &state,
            &router_tx,
            &admin(),
            ClientClassificationReason::DirectLocal,
            false,
            deadline,
            json!({ "action": "export_repro_fixture", "capture_id": "cap-001" }),
        )
        .unwrap();
        assert!(!export_default.bypass_redaction);

        let export_raw = execute(
            &state,
            &router_tx,
            &admin(),
            ClientClassificationReason::DirectLocal,
            false,
            deadline,
            json!({ "action": "export_repro_fixture", "capture_id": "cap-001", "strip_credentials": false }),
        )
        .unwrap();
        assert!(export_raw.bypass_redaction);
    }

    #[test]
    fn delete_capture_fails_closed_without_metadata_and_blocks_active() {
        let dir = std::env::temp_dir().join(format!("keeppeek-mcp-del-{}", uuid::Uuid::new_v4()));
        let store_root = dir.join("camera-debug");
        std::fs::create_dir_all(store_root.join("orphan-cap")).unwrap();
        std::fs::write(store_root.join("orphan-cap/capture.pcapng"), b"pcap").unwrap();

        let service = EngineCameraDebugService::default();
        let scoped_err = service
            .delete_capture(DeleteCaptureRequest {
                capture_id: Some("orphan-cap".to_owned()),
                camera_id: None,
                delete_all: false,
                allowed_camera_ids: Some(vec!["192.0.2.10".to_owned()]),
                config_dir: Some(dir.clone()),
            })
            .unwrap_err();
        assert_eq!(scoped_err._http_status, 403);

        let store = CaptureStore::new(&store_root, StoreLimits::default()).unwrap();
        let (cap, _) = store.begin_capture("192.0.2.10", "active-cap").unwrap();
        let meta = CaptureMetadata {
            capture_id: "active-cap".to_owned(),
            camera_id: "192.0.2.10".to_owned(),
            protocol: "rtsp".to_owned(),
            created_at_unix_secs: 1,
            duration_ms: 10,
            total_bytes: 0,
            packet_count: 0,
            include_raw_auth: false,
            trigger_probe: false,
            tls_decrypted: false,
            transport: "tcp".to_owned(),
            files: Vec::new(),
            anomalies: Default::default(),
            warnings: Vec::new(),
        };
        store.save_metadata("active-cap", &meta).unwrap();
        let active_req = DeleteCaptureRequest {
            capture_id: Some("active-cap".to_owned()),
            camera_id: None,
            delete_all: false,
            allowed_camera_ids: None,
            config_dir: Some(dir.clone()),
        };
        assert_eq!(
            service.delete_capture(active_req).unwrap_err()._http_status,
            409
        );
        drop(cap);
        assert!(
            service
                .delete_capture(DeleteCaptureRequest {
                    capture_id: Some("active-cap".to_owned()),
                    camera_id: None,
                    delete_all: false,
                    allowed_camera_ids: None,
                    config_dir: Some(dir.clone()),
                })
                .is_ok()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_oversized_camera_id_and_dot_in_capture_id() {
        let (state, router_tx) = test_state(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                ..McpConfig::default()
            },
            Arc::new(MockCameraDebugService::new(None)),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        for bad in [
            json!({ "action": "inspect_pipeline", "camera_id": "x".repeat(129) }),
            json!({ "action": "delete_capture", "capture_id": "cap.with.dot" }),
            json!({ "action": "capture", "camera_id": "192.0.2.10", "trace_filter": "keeppeek=verbose" }),
            json!({ "action": "capture", "camera_id": "192.0.2.10", "trace_filter": "a".repeat(513) }),
            json!({ "action": "read_artifact", "capture_id": "cap-1", "artifact": "../secrets.toml" }),
            json!({ "action": "read_artifact", "capture_id": "cap-1", "artifact": "sub/file.bin" }),
            json!({ "action": "read_artifact", "capture_id": "cap-1", "artifact": "a\\b" }),
            json!({ "action": "read_artifact", "capture_id": "cap-1", "artifact": "foo..bar" }),
            json!({ "action": "read_artifact", "capture_id": "cap-1", "artifact": "capture.pcapng", "length": 0 }),
            json!({ "action": "read_artifact", "capture_id": "cap-1", "artifact": "capture.pcapng", "length": 131_073 }),
        ] {
            let err = execute(
                &state,
                &router_tx,
                &admin(),
                ClientClassificationReason::DirectLocal,
                false,
                deadline,
                bad,
            )
            .unwrap_err();
            assert_eq!(err._http_status, 400);
        }
    }

    #[test]
    fn read_artifact_streams_chunks_verifies_sha256_and_enforces_allowlist() {
        let dir =
            std::env::temp_dir().join(format!("keeppeek-mcp-read-art-{}", uuid::Uuid::new_v4()));
        let store_root = dir.join("camera-debug");
        let store = CaptureStore::new(&store_root, StoreLimits::default()).unwrap();
        let cap_dir = store.capture_dir("cap-read-1").unwrap();
        std::fs::create_dir_all(&cap_dir).unwrap();
        let text_payload = "line1\nline2\nline3\n";
        let bin_payload: Vec<u8> = (0..=255).collect();
        std::fs::write(cap_dir.join("traces.jsonl"), text_payload.as_bytes()).unwrap();
        std::fs::write(cap_dir.join("capture.pcapng"), &bin_payload).unwrap();
        let meta = CaptureMetadata {
            capture_id: "cap-read-1".to_owned(),
            camera_id: "192.0.2.10".to_owned(),
            protocol: "rtsp".to_owned(),
            created_at_unix_secs: 1,
            duration_ms: 100,
            total_bytes: 0,
            packet_count: 1,
            include_raw_auth: true,
            trigger_probe: false,
            tls_decrypted: false,
            transport: "tcp".to_owned(),
            files: vec!["capture.pcapng".to_owned(), "traces.jsonl".to_owned()],
            anomalies: Default::default(),
            warnings: Vec::new(),
        };
        store.save_metadata("cap-read-1", &meta).unwrap();

        let service = EngineCameraDebugService::default();
        let denied = service
            .read_artifact(ReadArtifactRequest {
                capture_id: "cap-read-1".to_owned(),
                artifact: "traces.jsonl".to_owned(),
                offset: 0,
                length: 64,
                encoding: ArtifactEncoding::Auto,
                allowed_camera_ids: Some(vec!["192.0.2.99".to_owned()]),
                config_dir: Some(dir.clone()),
            })
            .unwrap_err();
        assert_eq!(denied._http_status, 403);

        let chunk1 = service
            .read_artifact(ReadArtifactRequest {
                capture_id: "cap-read-1".to_owned(),
                artifact: "traces.jsonl".to_owned(),
                offset: 0,
                length: 6,
                encoding: ArtifactEncoding::Auto,
                allowed_camera_ids: Some(vec!["192.0.2.10".to_owned()]),
                config_dir: Some(dir.clone()),
            })
            .unwrap();
        assert_eq!(chunk1["encoding"], "utf8");
        assert_eq!(chunk1["data"], "line1\n");
        assert_eq!(chunk1["eof"], false);
        assert_eq!(chunk1["next_offset"], 6);

        let chunk2 = service
            .read_artifact(ReadArtifactRequest {
                capture_id: "cap-read-1".to_owned(),
                artifact: "traces.jsonl".to_owned(),
                offset: 6,
                length: 100,
                encoding: ArtifactEncoding::Auto,
                allowed_camera_ids: None,
                config_dir: Some(dir.clone()),
            })
            .unwrap();
        assert_eq!(chunk2["data"], "line2\nline3\n");
        assert_eq!(chunk2["eof"], true);
        assert_eq!(chunk1["sha256"], chunk2["sha256"]);

        let bin_chunk = service
            .read_artifact(ReadArtifactRequest {
                capture_id: "cap-read-1".to_owned(),
                artifact: "capture.pcapng".to_owned(),
                offset: 0,
                length: 128,
                encoding: ArtifactEncoding::Auto,
                allowed_camera_ids: None,
                config_dir: Some(dir.clone()),
            })
            .unwrap();
        assert_eq!(bin_chunk["encoding"], "base64");
        assert_eq!(bin_chunk["eof"], false);
        assert_eq!(
            BASE64.decode(bin_chunk["data"].as_str().unwrap()).unwrap(),
            bin_payload[..128]
        );

        let utf8_err = service
            .read_artifact(ReadArtifactRequest {
                capture_id: "cap-read-1".to_owned(),
                artifact: "capture.pcapng".to_owned(),
                offset: 128,
                length: 128,
                encoding: ArtifactEncoding::Utf8,
                allowed_camera_ids: None,
                config_dir: Some(dir.clone()),
            })
            .unwrap_err();
        assert_eq!(utf8_err._http_status, 400);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
