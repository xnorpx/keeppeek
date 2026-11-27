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
    config::mcp::McpConfig,
    mcp::protocol::ToolCallResult,
    runtime::{FacadeSender, RouterMessage, RouterQuery, RouterResponse},
    stats::StreamHealthReport,
    storage::health::RecordingStreamHealthSnapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt,
    net::IpAddr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use zeroize::Zeroize;

const CAMERA_ID_MAX_BYTES: usize = 256;
const CAPTURE_ID_MAX_BYTES: usize = 128;
const DURATION_SECONDS_DEFAULT: u64 = 10;
const DURATION_SECONDS_MIN: u64 = 1;
const DURATION_SECONDS_MAX: u64 = 60;
const CAPTURE_BYTES_MIN: usize = 65_536;
const CAPTURE_BYTES_MAX: usize = 268_435_456;
const ARTIFACTS_MAX: usize = 5;
const PROTOCOLS_MAX: usize = 6;
const INSPECT_CAMERAS_MAX: usize = 256;

const _: () = {
    assert!(DURATION_SECONDS_MIN <= DURATION_SECONDS_DEFAULT);
    assert!(DURATION_SECONDS_DEFAULT <= DURATION_SECONDS_MAX);
    assert!(CAPTURE_BYTES_MIN <= CAPTURE_BYTES_MAX);
    assert!(ARTIFACTS_MAX == 5);
    assert!(PROTOCOLS_MAX == 6);
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

/// Bounded inputs for a `capture` request.
#[derive(Debug, Clone, Serialize)]
pub(super) struct CaptureRequest {
    pub(super) target: DebugCameraTarget,
    pub(super) stream: DebugStream,
    pub(super) artifacts: Vec<DebugArtifact>,
    pub(super) protocols: Vec<DebugProtocol>,
    pub(super) duration_seconds: u64,
    pub(super) max_bytes: usize,
    pub(super) max_storage_bytes: u64,
    pub(super) include_raw_auth: bool,
    pub(super) trigger_probe: bool,
    pub(super) config_dir: Option<PathBuf>,
    #[serde(skip_serializing)]
    pub(super) credentials: Option<CameraDebugCredentials>,
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
    pub(super) config_dir: Option<PathBuf>,
    #[serde(skip_serializing)]
    pub(super) credentials: Option<CameraDebugCredentials>,
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

    fn pipeline_debug_metrics(&self, _camera_id: &str) -> Option<Value> {
        None
    }
}

/// Default backend used before the capture engine is wired into the runtime.
#[derive(Debug, Default)]
pub(super) struct UnavailableCameraDebugService;

impl CameraDebugService for UnavailableCameraDebugService {
    fn capture(&self, request: CaptureRequest) -> Result<Value, ControlCommandError> {
        remaining_timeout(request.deadline)?;
        let _ = request.credentials.as_ref().map(|c| format!("{c:?}"));
        Err(capture_backend_unavailable("capture"))
    }

    fn compare_transports(
        &self,
        request: CompareTransportsRequest,
    ) -> Result<Value, ControlCommandError> {
        remaining_timeout(request.deadline)?;
        let _ = request.credentials.as_ref().map(|c| format!("{c:?}"));
        Err(capture_backend_unavailable("compare_transports"))
    }

    fn export_repro_fixture(
        &self,
        request: ExportReproFixtureRequest,
    ) -> Result<Value, ControlCommandError> {
        remaining_timeout(request.deadline)?;
        Err(capture_backend_unavailable("export_repro_fixture"))
    }

    fn list_captures(&self, _request: ListCapturesRequest) -> Result<Value, ControlCommandError> {
        Err(capture_backend_unavailable("list_captures"))
    }

    fn delete_capture(&self, _request: DeleteCaptureRequest) -> Result<Value, ControlCommandError> {
        Err(capture_backend_unavailable("delete_capture"))
    }
}

fn capture_backend_unavailable(action: &str) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        format!("camera debugging capture backend is not attached for action '{action}'"),
    )
}

/// Output from executing `camera_debugging`, including its redaction boundary decision.
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
    stream: Option<DebugStream>,
    artifacts: Option<Vec<DebugArtifact>>,
    protocols: Option<Vec<DebugProtocol>>,
    duration_seconds: Option<u64>,
    max_bytes: Option<usize>,
    include_raw_auth: Option<bool>,
    trigger_probe: Option<bool>,
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
        state.mcp.config(),
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
                allowed_camera_ids: allowed_camera_scope(ctx.cfg),
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
    }
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
    let protocols = parse_protocols(args.protocols.as_deref())?;
    let duration_seconds = parse_duration_seconds(args.duration_seconds)?;
    let max_bytes = parse_max_bytes(args.max_bytes, ctx.cfg)?;
    let credentials = include_raw_auth.then(|| CameraDebugCredentials::from_camera(&camera));
    Ok(CaptureRequest {
        target: DebugCameraTarget::from_camera(&camera, include_raw_auth),
        stream,
        artifacts,
        protocols,
        duration_seconds,
        max_bytes,
        max_storage_bytes: ctx.cfg.max_debug_storage_bytes,
        include_raw_auth,
        trigger_probe: args.trigger_probe.unwrap_or(false),
        config_dir: ctx.config_dir.clone(),
        credentials,
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
    let credentials = include_raw_auth.then(|| CameraDebugCredentials::from_camera(&camera));
    Ok(CompareTransportsRequest {
        target: DebugCameraTarget::from_camera(&camera, include_raw_auth),
        stream,
        mode,
        duration_seconds,
        max_bytes,
        max_storage_bytes: ctx.cfg.max_debug_storage_bytes,
        include_raw_auth,
        config_dir: ctx.config_dir.clone(),
        credentials,
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
    Ok(ExportReproFixtureRequest {
        capture_id,
        camera_id,
        allowed_camera_ids: allowed_camera_scope(ctx.cfg),
        strip_credentials,
        anonymize_network: args.anonymize_network.unwrap_or(true),
        trim_media: args
            .trim_media
            .unwrap_or(TrimMediaMode::HeadersAndMinimalFailingSlice),
        max_bytes,
        config_dir: ctx.config_dir.clone(),
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
        allowed_camera_ids: allowed_camera_scope(ctx.cfg),
        config_dir: ctx.config_dir.clone(),
    })
}

fn inspect_pipeline(
    ctx: &ActionContext<'_>,
    router_tx: &FacadeSender<RouterMessage>,
    args: &CameraDebuggingArgs,
) -> Result<(Value, bool), ControlCommandError> {
    let include_raw_auth = args.include_raw_auth.unwrap_or(false);
    let cameras = match args.camera_id.as_deref() {
        Some(raw_id) => {
            let camera_id = validate_camera_id(raw_id)?;
            vec![resolve_allowed_camera(
                ctx.state,
                ctx.principal,
                ctx.classification,
                ctx.cfg,
                camera_id,
            )?]
        }
        None => ctx
            .state
            .camera_entries()
            .into_iter()
            .filter(|camera| camera_entry_allowed(ctx.cfg, camera))
            .take(INSPECT_CAMERAS_MAX)
            .collect(),
    };
    let (router_statuses, router_available) = read_router_statuses(router_tx);
    let mut ingress_by_ip = ctx
        .state
        .health
        .snapshot()
        .into_iter()
        .map(|report| (report.ip, report))
        .collect::<HashMap<_, _>>();
    let recording_by_stream = ctx
        .state
        .recording_health
        .snapshot()
        .streams
        .into_iter()
        .map(|stream| (stream.stream_id.clone(), stream))
        .collect::<HashMap<_, _>>();
    let mut pipelines = Vec::with_capacity(cameras.len());
    for camera in &cameras {
        let ingress = camera
            .info
            .ip
            .parse::<IpAddr>()
            .ok()
            .and_then(|ip| ingress_by_ip.remove(&ip))
            .map_or_else(Vec::new, |report| report.streams);
        pipelines.push(build_camera_pipeline_json(
            ctx.state,
            camera,
            router_statuses.get(&camera.recording_label),
            &ingress,
            &recording_by_stream,
            include_raw_auth,
        ));
    }
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

fn allowed_camera_scope(cfg: &McpConfig) -> Option<Vec<String>> {
    (!cfg.debug_camera_ids.is_empty()).then(|| cfg.debug_camera_ids.clone())
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
            "camera_id must contain 1 to 256 printable characters",
        ));
    }
    Ok(trimmed)
}

fn validate_capture_id(raw: Option<&str>) -> Result<&str, ControlCommandError> {
    let raw = raw.ok_or_else(|| invalid("capture_id is required for this action"))?;
    let trimmed = raw.trim();
    let valid_chars = trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if trimmed.is_empty()
        || trimmed.len() > CAPTURE_ID_MAX_BYTES
        || trimmed.contains("..")
        || !valid_chars
    {
        return Err(invalid(
            "capture_id must be 1 to 128 safe ASCII characters ([A-Za-z0-9._-])",
        ));
    }
    Ok(trimmed)
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
        ]);
    };
    if items.is_empty() || items.len() > ARTIFACTS_MAX {
        return Err(invalid("artifacts must contain 1 to 5 entries"));
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
        cameras::{CameraBackend, CameraConfig, CameraRecordingMode, CameraTransport},
        config::Config,
        runtime::Router,
        storage::{RecordingDemand, StorageConfig},
        webrtc::WebRtc,
    };
    use std::sync::{Arc, Mutex};

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
        .err()
        .expect("User role must be denied");
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
        .err()
        .expect("disabled allow_camera_debugging must fail closed");
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
            .err()
            .expect("read-only header must reject action");
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
        .err()
        .expect("unlisted camera must be denied");
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
            .err()
            .expect("invalid bounded argument must fail");
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
}
