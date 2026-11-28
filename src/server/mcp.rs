//! Embedded Streamable HTTP MCP route and core KeepPeek MCP tools.
//!
//! `POST /mcp` is disabled by default and returns `404` unless `[mcp].enabled`
//! is `true`. Every request passes through Host/Origin checks, KeepPeek access
//! control, bounded request/response budgets, RAII concurrency limits, and
//! serialization-layer secret redaction.

use super::{
    AuthenticatedApiRequest, CameraSettingsUpdate, ControlCommandError,
    MAX_NOTIFICATION_RULE_JSON_BYTES, ServerState, api_status, authenticate_api_bearer,
    authenticated_api_request, camera_access, camera_configuration_revision, configuration,
    current_config, delete_camera_settings, get_logging_settings, logging, mcp_camera_debug,
    mcp_catalog_probe, mcp_device_settings, mcp_diagnostics, mcp_schemas, mcp_secret_handoff,
    mcp_snapshot, notification_command_error, proto_notification_rule_record, record_access_audit,
    restore_notification_destinations, runtime_configuration, save_camera_settings, server_health,
    server_health_status, service_error, state_store, unix_time_ms, validate_client_id,
};
use crate::{
    access::AccessRole,
    api::proto,
    cameras::{CameraBackend, CameraRecordingMode, CameraTransport, EventRecordingStream},
    config::{self, Secrets, mcp::McpConfig},
    health::{CameraHealthState, HealthTotals, ServerHealthResponse},
    logging::LogLevel,
    mcp::{
        protocol::{
            ERROR_BUSY, ERROR_FORBIDDEN, ERROR_INTERNAL, ERROR_INVALID_PARAMS,
            ERROR_METHOD_NOT_FOUND, ERROR_RESPONSE_TOO_LARGE, ERROR_TIMEOUT, InitializeResult,
            JsonRpcError, JsonRpcRequest, JsonRpcResponse, MAX_REQUEST_BODY_BYTES,
            READ_ONLY_HEADER, ToolCallParams, ToolCallResult, ToolContent, parse_bounded_request,
        },
        redaction::SecretRedactor,
    },
    notifications::{Handle as NotificationHandle, model::Rule as NotificationRule},
    runtime::{FacadeSender, RouterMessage},
    webrtc::SessionId,
};
use prost_types::{Struct, Value as ProstValue, value::Kind};
use rouille::{Request, Response};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    net::IpAddr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use url::{Host, Url};

const DEFAULT_LOG_SEARCH_LIMIT: usize = 50;
const MAX_LOG_SEARCH_LIMIT: usize = 200;
const READ_ARTIFACT_MAX_RESPONSE_BYTES: usize = 512 * 1024;
const PROFILE_ACTIVE_SCHEMA: &str = "keeppeek.runtime-profiles.active.v1";
const PROFILE_SELECT_SCHEMA: &str = "keeppeek.runtime-profiles.select.v2";

#[derive(Clone, Default)]
struct CachedSecrets {
    stamp: Option<(std::time::SystemTime, u64)>,
    secrets: Secrets,
}

/// Server-owned MCP runtime state, concurrency tracker, and redaction snapshot.
#[derive(Clone)]
pub(super) struct Runtime {
    enabled: Arc<AtomicBool>,
    config: Arc<RwLock<McpConfig>>,
    active_requests: Arc<AtomicUsize>,
    secrets: Arc<RwLock<CachedSecrets>>,
    snapshot_limiter: Arc<mcp_snapshot::SnapshotRateLimiter>,
    handoffs: Arc<mcp_secret_handoff::HandoffStore>,
    camera_debug: Arc<dyn mcp_camera_debug::CameraDebugService>,
}

impl Runtime {
    pub(super) fn new(config: McpConfig, secrets: Secrets) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(config.enabled)),
            config: Arc::new(RwLock::new(config)),
            active_requests: Arc::new(AtomicUsize::new(0)),
            secrets: Arc::new(RwLock::new(CachedSecrets {
                stamp: None,
                secrets,
            })),
            snapshot_limiter: Arc::new(mcp_snapshot::SnapshotRateLimiter::new()),
            handoffs: Arc::new(mcp_secret_handoff::HandoffStore::new()),
            camera_debug: Arc::new(mcp_camera_debug::EngineCameraDebugService::default()),
        }
    }

    pub(super) fn emit_startup_warning(&self) {
        if let Some(warning) = crate::health::mcp_camera_debugging_warning(&self.config()) {
            tracing::warn!(target: "keeppeek::mcp", scope = "mcp", "{warning}");
        }
    }

    pub(super) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub(super) fn enabled_flag(&self) -> &AtomicBool {
        &self.enabled
    }

    pub(super) fn config(&self) -> McpConfig {
        self.config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Updates live MCP settings and flips endpoint availability immediately without a restart.
    pub(super) fn apply_live_config(&self, config: McpConfig) {
        let enabled = config.enabled;
        {
            let mut guard = self
                .config
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = config;
        }
        self.enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            self.handoffs.clear_all();
        }
    }

    pub(super) fn snapshot_limiter(&self) -> &mcp_snapshot::SnapshotRateLimiter {
        &self.snapshot_limiter
    }

    pub(super) fn handoffs(&self) -> &mcp_secret_handoff::HandoffStore {
        &self.handoffs
    }

    pub(super) fn camera_debug(&self) -> &dyn mcp_camera_debug::CameraDebugService {
        self.camera_debug.as_ref()
    }

    #[cfg(test)]
    pub(super) fn with_camera_debug_service(
        mut self,
        service: Arc<dyn mcp_camera_debug::CameraDebugService>,
    ) -> Self {
        self.camera_debug = service;
        self
    }

    pub(super) fn secrets_snapshot(&self, config_path: Option<&std::path::Path>) -> Secrets {
        let Some(path) = config_path else {
            return self
                .secrets
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .secrets
                .clone();
        };
        let secrets_file = config::secrets_path(path);
        let stamp = std::fs::metadata(&secrets_file)
            .ok()
            .and_then(|meta| meta.modified().ok().map(|modified| (modified, meta.len())));
        {
            let guard = self
                .secrets
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if stamp.is_none() || guard.stamp == stamp {
                return guard.secrets.clone();
            }
        }
        // Reload only when secrets.toml metadata changes so UI or backup edits stay redacted.
        if let Ok(loaded) = config::load_secrets(path) {
            let mut guard = self
                .secrets
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.stamp = stamp;
            guard.secrets = loaded.clone();
            return loaded;
        }
        self.secrets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .secrets
            .clone()
    }

    pub(super) fn refresh_secrets(&self, config_path: &std::path::Path, secrets: Secrets) {
        let stamp = std::fs::metadata(config::secrets_path(config_path))
            .ok()
            .and_then(|meta| meta.modified().ok().map(|modified| (modified, meta.len())));
        let mut guard = self
            .secrets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.stamp = stamp;
        guard.secrets = secrets;
    }

    pub(super) fn try_acquire(&self) -> Option<RequestGuard> {
        let limit = self.config().max_concurrent_requests;
        let mut current = self.active_requests.load(Ordering::Acquire);
        loop {
            if current >= limit {
                return None;
            }
            match self.active_requests.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(RequestGuard {
                        active_requests: self.active_requests.clone(),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

/// RAII guard that releases one MCP concurrency slot on drop.
pub(super) struct RequestGuard {
    active_requests: Arc<AtomicUsize>,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.active_requests.fetch_sub(1, Ordering::Release);
    }
}

/// Execution context shared across MCP tool handlers.
pub(super) struct McpContext<'a> {
    pub(super) state: &'a ServerState,
    pub(super) router_tx: &'a FacadeSender<RouterMessage>,
    pub(super) identity: &'a AuthenticatedApiRequest,
    pub(super) read_only_header: bool,
    pub(super) host_port: Option<u16>,
    pub(super) deadline: Instant,
}

impl McpContext<'_> {
    pub(super) const fn is_admin(&self) -> bool {
        matches!(self.identity.principal.role, AccessRole::Administrator)
    }

    pub(super) fn writes_allowed(&self) -> bool {
        self.is_admin() && self.state.mcp.config().allow_writes && !self.read_only_header
    }

    pub(super) fn snapshots_allowed(&self) -> bool {
        self.state.mcp.config().allow_snapshots
    }

    pub(super) fn require_admin(&self) -> Result<(), ToolError> {
        if self.is_admin() {
            Ok(())
        } else {
            Err(ToolError::forbidden(
                "Administrator role is required for this MCP tool",
            ))
        }
    }

    pub(super) fn require_write(&self) -> Result<(), ToolError> {
        self.require_admin()?;
        if !self.state.mcp.config().allow_writes {
            return Err(ToolError::forbidden(
                "MCP write tools are disabled by server configuration ([mcp].allow_writes = false)",
            ));
        }
        if self.read_only_header {
            return Err(ToolError::forbidden(
                "MCP write tools are blocked because X-KeepPeek-MCP-Read-Only is active",
            ));
        }
        Ok(())
    }

    pub(super) fn check_deadline(&self) -> Result<(), ToolError> {
        if Instant::now() > self.deadline {
            return Err(ToolError::timeout(
                "MCP request exceeded its timeout budget",
            ));
        }
        Ok(())
    }

    pub(super) fn audit(&self, action: &str, target_id: Option<&str>, result: &str) {
        record_access_audit(
            self.state,
            i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
            Some(&self.identity.principal.id()),
            Some(self.identity.principal.role),
            action,
            target_id,
            result,
            self.identity.classification.reason,
        );
    }
}

pub(super) struct ToolError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl ToolError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: ERROR_INVALID_PARAMS,
            message: message.into(),
            data: None,
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            code: ERROR_FORBIDDEN,
            message: message.into(),
            data: None,
        }
    }

    fn timeout(message: impl Into<String>) -> Self {
        Self {
            code: ERROR_TIMEOUT,
            message: message.into(),
            data: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ERROR_INTERNAL,
            message: message.into(),
            data: None,
        }
    }

    fn from_control(error: ControlCommandError) -> Self {
        let code = match error._http_status {
            400 | 422 => ERROR_INVALID_PARAMS,
            401 | 403 => ERROR_FORBIDDEN,
            429 => ERROR_BUSY,
            504 => ERROR_TIMEOUT,
            _ => ERROR_INTERNAL,
        };
        Self {
            code,
            message: error.message,
            data: None,
        }
    }

    fn into_json_rpc(self) -> JsonRpcError {
        JsonRpcError {
            code: self.code,
            message: self.message,
            data: self.data,
        }
    }
}

/// Handles `POST /mcp` JSON-RPC 2.0 requests.
pub(super) fn handle_http(
    request: &Request,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> Response {
    if !state.mcp_enabled() {
        return service_error(404, "not found");
    }
    let allowed_origin = match validate_host_and_origin(request, state) {
        Ok(origin) => origin,
        Err(response) => return response,
    };
    let response = authenticated_api_request(request, state, false, AccessRole::User, |identity| {
        handle_authenticated_post(request, router_tx, state, &identity)
    });
    with_allowed_origin_header(response, allowed_origin)
}

/// Handles `OPTIONS /mcp` preflight checks when MCP is enabled.
pub(super) fn handle_options(request: &Request, state: &ServerState) -> Response {
    if !state.mcp_enabled() {
        return service_error(404, "not found");
    }
    let allowed_origin = match validate_host_and_origin(request, state) {
        Ok(origin) => origin,
        Err(response) => return response,
    };
    let response = Response::empty_204()
        .with_additional_header("Access-Control-Allow-Methods", "POST, OPTIONS")
        .with_additional_header(
            "Access-Control-Allow-Headers",
            "Authorization, Content-Type, Accept, X-KeepPeek-MCP-Read-Only",
        )
        .with_additional_header("Cache-Control", "no-store");
    with_allowed_origin_header(response, allowed_origin)
}

/// Returns `404` when MCP is disabled or `405` for unsupported HTTP methods on `/mcp`.
pub(super) fn handle_unsupported_method(state: &ServerState) -> Response {
    if !state.mcp_enabled() {
        return service_error(404, "not found");
    }
    api_status(405, "MCP endpoint supports POST and OPTIONS only")
        .with_additional_header("Allow", "POST, OPTIONS")
        .with_additional_header("Cache-Control", "no-store")
}

fn resolve_post_identity(
    request: &Request,
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
) -> Result<AuthenticatedApiRequest, Response> {
    // Require Bearer auth whenever a browser Origin header is present, even on local networks.
    let effective = if request.header("Origin").is_some()
        && identity.principal.credential_binding().is_none()
    {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        authenticate_api_bearer(request, state, identity.classification, now_ms)
            .map_err(|response| response.with_additional_header("Cache-Control", "no-store"))?
    } else {
        identity.clone()
    };
    if effective.principal.is_local() {
        validate_local_host_header(request)?;
    } else if effective.principal.credential_binding().is_none() {
        return Err(api_status(401, "MCP requires Bearer authentication")
            .with_additional_header("Cache-Control", "no-store"));
    }
    Ok(effective)
}

fn handle_authenticated_post(
    request: &Request,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
) -> Response {
    let identity = match resolve_post_identity(request, state, identity) {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    if !is_json_content_type(request) {
        return api_status(415, "MCP requests require Content-Type: application/json")
            .with_additional_header("Cache-Control", "no-store");
    }
    let body = match read_bounded_body(request, MAX_REQUEST_BODY_BYTES) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let Some(_guard) = state.mcp.try_acquire() else {
        let busy = JsonRpcResponse::error(
            Value::Null,
            JsonRpcError::new(ERROR_BUSY, "MCP concurrency limit reached"),
        );
        return json_rpc_http_response(
            state,
            429,
            &busy,
            state.mcp.config().max_response_bytes,
            false,
        )
        .with_additional_header("Retry-After", "1");
    };
    let parsed = match parse_bounded_request(&body, MAX_REQUEST_BODY_BYTES) {
        Ok(parsed) => parsed,
        Err(error_response) => {
            return json_rpc_http_response(
                state,
                200,
                error_response.as_ref(),
                state.mcp.config().max_response_bytes,
                false,
            );
        }
    };
    let response_limit = response_byte_limit(&state.mcp.config(), &parsed);
    let host_port = parse_host_header(request)
        .ok()
        .and_then(|url| url.port_or_known_default());
    let ctx = McpContext {
        state,
        router_tx,
        identity: &identity,
        read_only_header: read_only_header_enabled(request),
        host_port,
        deadline: Instant::now() + Duration::from_secs(state.mcp.config().request_timeout_seconds),
    };
    dispatch_json_rpc(&ctx, parsed).map_or_else(
        || {
            Response::empty_204()
                .with_status_code(202)
                .with_additional_header("Cache-Control", "no-store")
        },
        |(rpc_response, bypass_redaction)| {
            json_rpc_http_response(state, 200, &rpc_response, response_limit, bypass_redaction)
        },
    )
}

fn response_byte_limit(config: &McpConfig, request: &JsonRpcRequest) -> usize {
    let params = (request.method == "tools/call")
        .then_some(request.params.as_ref())
        .flatten();
    let tool_name = params
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str);
    if tool_name == Some("get_camera_snapshot") {
        // Snapshots use max_snapshot_bytes plus base64 expansion and JSON metadata envelope.
        let base64_bytes = config.max_snapshot_bytes.div_ceil(3).saturating_mul(4);
        return config
            .max_response_bytes
            .max(base64_bytes.saturating_add(4_096));
    }
    let is_read_artifact = config.allow_camera_debugging
        && tool_name == Some("camera_debugging")
        && params
            .and_then(|value| value.get("arguments"))
            .and_then(|args| args.get("action"))
            .and_then(Value::as_str)
            == Some("read_artifact");
    if is_read_artifact {
        return config
            .max_response_bytes
            .max(READ_ARTIFACT_MAX_RESPONSE_BYTES);
    }
    config.max_response_bytes
}

fn parse_host_header(request: &Request) -> Result<Url, Response> {
    let Some(host) = request.header("Host").map(str::trim) else {
        return Err(api_status(403, "Host header is required for MCP requests"));
    };
    if host.is_empty()
        || host.len() > 256
        || host
            .bytes()
            .any(|byte| byte <= 0x20 || byte == 0x7f || matches!(byte, b'/' | b'?' | b'#' | b'@'))
    {
        return Err(api_status(403, "Host header is invalid"));
    }
    let scheme = if request.is_secure() { "https" } else { "http" };
    let parsed = Url::parse(&format!("{scheme}://{host}"))
        .map_err(|_| api_status(403, "Host header is invalid"))?;
    if parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(api_status(403, "Host header is invalid"));
    }
    Ok(parsed)
}

fn validate_host_and_origin(
    request: &Request,
    state: &ServerState,
) -> Result<Option<String>, Response> {
    let _parsed = parse_host_header(request)?;
    let Some(origin) = request.header("Origin") else {
        return Ok(None);
    };
    // Require explicit allowlist membership so DNS-rebound browser origins cannot match Host.
    if state.allowed_origins.contains(origin) {
        Ok(Some(origin.to_owned()))
    } else {
        Err(api_status(
            403,
            "browser Origin is not allowed on /mcp; configure [direct_card].allowed_origins",
        ))
    }
}

fn validate_local_host_header(request: &Request) -> Result<(), Response> {
    let parsed = parse_host_header(request)?;
    let is_local_host = match parsed.host() {
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => true,
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if is_local_host {
        Ok(())
    } else {
        Err(api_status(
            403,
            "unauthenticated local MCP requests require localhost or an IP literal Host header",
        ))
    }
}

fn with_allowed_origin_header(response: Response, origin: Option<String>) -> Response {
    match origin {
        Some(origin) => response
            .with_additional_header("Access-Control-Allow-Origin", origin)
            .with_additional_header("Vary", "Origin"),
        None => response,
    }
}

fn is_json_content_type(request: &Request) -> bool {
    request
        .header("Content-Type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|media_type| media_type.eq_ignore_ascii_case("application/json"))
}

fn read_only_header_enabled(request: &Request) -> bool {
    request
        .header(READ_ONLY_HEADER)
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1")
}

fn read_bounded_body(request: &Request, max_bytes: usize) -> Result<Vec<u8>, Response> {
    let Some(body) = request.data() else {
        return Err(api_status(400, "MCP JSON-RPC request body is required"));
    };
    let mut bytes = Vec::new();
    let cap = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if body.take(cap).read_to_end(&mut bytes).is_err() {
        return Err(api_status(400, "unable to read MCP request body"));
    }
    if bytes.len() > max_bytes {
        return Err(api_status(
            413,
            &format!("MCP request body exceeds {max_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

fn build_redactor(state: &ServerState) -> SecretRedactor {
    // Use the metadata-validated in-memory secrets snapshot without reading disk on unchanged requests.
    let secrets = state
        .mcp
        .secrets_snapshot(state.camera_config_path.as_deref());
    let mut extra = Vec::new();
    let access_key = *state
        .access_key
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !access_key.is_unset() {
        extra.push(access_key.canonical());
    }
    for camera in state.camera_entries() {
        if !camera.configuration.password.is_empty() {
            extra.push(camera.configuration.password.clone());
        }
        if let Some(url) = &camera.configuration.main_rtsp_url {
            extra.push(url.clone());
        }
        if let Some(url) = &camera.configuration.sub_rtsp_url {
            extra.push(url.clone());
        }
    }
    let hub = state
        .logging
        .as_ref()
        .map(crate::logging::LoggingService::dashboard_redactor);
    SecretRedactor::new(&secrets, extra, hub)
}

fn json_rpc_http_response(
    state: &ServerState,
    status: u16,
    response: &JsonRpcResponse,
    max_bytes: usize,
    bypass_redaction: bool,
) -> Response {
    let redactor = build_redactor(state);
    let mut value = serde_json::to_value(response).unwrap_or_else(|_| {
        json!({
            "jsonrpc": "2.0",
            "id": response.id,
            "error": { "code": ERROR_INTERNAL, "message": "failed to serialize MCP response" }
        })
    });
    let allow_raw = bypass_redaction
        && state.mcp.config().enabled
        && state.mcp.config().allow_camera_debugging
        && response.error.is_none();
    if !allow_raw {
        redactor.redact_value(&mut value);
    }
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    let final_bytes = if bytes.len() <= max_bytes {
        bytes
    } else {
        let overflow = JsonRpcResponse::error(
            response.id.clone(),
            JsonRpcError::new(
                ERROR_RESPONSE_TOO_LARGE,
                format!(
                    "MCP response exceeded max_response_bytes ({max_bytes} bytes); narrow filters or page_size"
                ),
            ),
        );
        let mut overflow_value = serde_json::to_value(&overflow).unwrap_or(Value::Null);
        redactor.redact_value(&mut overflow_value);
        serde_json::to_vec(&overflow_value).unwrap_or_default()
    };
    Response::from_data("application/json", final_bytes)
        .with_status_code(status)
        .with_additional_header("Cache-Control", "no-store")
}

fn dispatch_json_rpc(
    ctx: &McpContext<'_>,
    request: JsonRpcRequest,
) -> Option<(JsonRpcResponse, bool)> {
    if request.is_notification() {
        return None;
    }
    let id = request.id.unwrap_or(Value::Null);
    if let Err(error) = ctx.check_deadline() {
        return Some((JsonRpcResponse::error(id, error.into_json_rpc()), false));
    }
    let outcome: Result<(Value, bool), ToolError> = match request.method.as_str() {
        "initialize" => Ok((
            serde_json::to_value(InitializeResult::keeppeek(
                ctx.writes_allowed(),
                ctx.snapshots_allowed(),
            ))
            .unwrap_or(Value::Null),
            false,
        )),
        "ping" => Ok((json!({}), false)),
        "tools/list" => Ok((
            json!({ "tools": mcp_schemas::tool_definitions(ctx.is_admin()) }),
            false,
        )),
        "tools/call" => handle_tools_call(ctx, request.params),
        method => Err(ToolError {
            code: ERROR_METHOD_NOT_FOUND,
            message: format!("unsupported MCP method: {method}"),
            data: None,
        }),
    };
    let final_outcome = match outcome {
        Ok(pair) => ctx.check_deadline().map(|()| pair),
        Err(error) => Err(error),
    };
    match final_outcome {
        Ok((result, bypass_redaction)) => {
            let allow_raw =
                bypass_redaction && ctx.is_admin() && ctx.state.mcp.config().allow_camera_debugging;
            Some((JsonRpcResponse::success(id, result), allow_raw))
        }
        Err(error) => Some((JsonRpcResponse::error(id, error.into_json_rpc()), false)),
    }
}

fn handle_tools_call(
    ctx: &McpContext<'_>,
    params: Option<Value>,
) -> Result<(Value, bool), ToolError> {
    let params_value =
        params.ok_or_else(|| ToolError::invalid_params("tools/call requires params"))?;
    let call: ToolCallParams = serde_json::from_value(params_value).map_err(|error| {
        ToolError::invalid_params(format!("invalid tools/call params: {error}"))
    })?;
    let arguments = match call.arguments {
        Value::Null => Value::Object(Map::new()),
        Value::Object(map) => Value::Object(map),
        _ => {
            return Err(ToolError::invalid_params(
                "tools/call arguments must be a JSON object",
            ));
        }
    };
    let (result, bypass_redaction) = dispatch_tool(ctx, &call.name, arguments)?;
    let encoded = serde_json::to_value(result)
        .map_err(|error| ToolError::internal(format!("failed to encode tool result: {error}")))?;
    Ok((encoded, bypass_redaction))
}

fn dispatch_tool(
    ctx: &McpContext<'_>,
    name: &str,
    args: Value,
) -> Result<(ToolCallResult, bool), ToolError> {
    match name {
        "get_server_health" => tool_get_server_health(ctx).map(|r| (r, false)),
        "search_events" => dispatch_diagnostic(
            ctx,
            "mcp_search_events",
            mcp_diagnostics::search_events,
            &args,
        )
        .map(|r| (r, false)),
        "get_recording_coverage" => dispatch_diagnostic(
            ctx,
            "mcp_get_recording_coverage",
            mcp_diagnostics::get_recording_coverage,
            &args,
        )
        .map(|r| (r, false)),
        "get_camera_snapshot" => tool_get_camera_snapshot(ctx, args).map(|r| (r, false)),
        "search_logs" => tool_search_logs(ctx, args).map(|r| (r, false)),
        "get_logging_settings" => tool_get_logging_settings(ctx).map(|r| (r, false)),
        "get_configuration" => tool_get_configuration(ctx, args).map(|r| (r, false)),
        "list_secret_keys" => tool_list_secret_keys(ctx).map(|r| (r, false)),
        "plan_configuration_change" => {
            tool_plan_configuration_change(ctx, args).map(|r| (r, false))
        }
        "get_storage_status" => dispatch_diagnostic(
            ctx,
            "mcp_get_storage_status",
            mcp_diagnostics::get_storage_status,
            &args,
        )
        .map(|r| (r, false)),
        "get_notification_status" => dispatch_diagnostic(
            ctx,
            "mcp_get_notification_status",
            mcp_diagnostics::get_notification_status,
            &args,
        )
        .map(|r| (r, false)),
        "discover_cameras" => dispatch_diagnostic(
            ctx,
            "mcp_discover_cameras",
            mcp_diagnostics::discover_cameras,
            &args,
        )
        .map(|r| (r, false)),
        "get_runtime_configuration" => dispatch_diagnostic(
            ctx,
            "mcp_get_runtime_configuration",
            mcp_diagnostics::get_runtime_configuration,
            &args,
        )
        .map(|r| (r, false)),
        "list_runtime_profiles" => dispatch_diagnostic(
            ctx,
            "mcp_list_runtime_profiles",
            mcp_diagnostics::list_runtime_profiles,
            &args,
        )
        .map(|r| (r, false)),
        _ => dispatch_onboarding_or_write_tool(ctx, name, args),
    }
}

fn dispatch_onboarding_or_write_tool(
    ctx: &McpContext<'_>,
    name: &str,
    args: Value,
) -> Result<(ToolCallResult, bool), ToolError> {
    match name {
        "search_camera_catalog" => dispatch_diagnostic(
            ctx,
            "mcp_search_camera_catalog",
            mcp_catalog_probe::search_camera_catalog,
            &args,
        )
        .map(|r| (r, false)),
        "probe_camera_streams" => tool_probe_camera_streams(ctx, args).map(|r| (r, false)),
        "request_secret_entry" => tool_request_secret_entry(ctx, args).map(|r| (r, false)),
        "get_secret_entry_status" => tool_get_secret_entry_status(ctx, args).map(|r| (r, false)),
        "get_camera_device_settings" => {
            tool_get_camera_device_settings(ctx, args).map(|r| (r, false))
        }
        "update_camera_device_settings" => {
            tool_update_camera_device_settings(ctx, args).map(|r| (r, false))
        }
        "camera_debugging" => tool_camera_debugging(ctx, args),
        "apply_configuration_plan" => tool_apply_configuration_plan(ctx, args).map(|r| (r, false)),
        "update_runtime_configuration" => {
            tool_update_runtime_configuration(ctx, args).map(|r| (r, false))
        }
        "manage_configuration_template" => {
            tool_manage_configuration_template(ctx, args).map(|r| (r, false))
        }
        "manage_camera" => tool_manage_camera(ctx, args).map(|r| (r, false)),
        "manage_notification_rule" => tool_manage_notification_rule(ctx, args).map(|r| (r, false)),
        "set_runtime_profile" => tool_set_runtime_profile(ctx, args).map(|r| (r, false)),
        "set_logging_filter" => tool_set_logging_filter(ctx, args).map(|r| (r, false)),
        "disable_mcp_access" => tool_disable_mcp_access(ctx, args).map(|r| (r, false)),
        other => Err(ToolError {
            code: ERROR_METHOD_NOT_FOUND,
            message: format!("unknown MCP tool: {other}"),
            data: None,
        }),
    }
}

fn tool_camera_debugging(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<(ToolCallResult, bool), ToolError> {
    let outcome = mcp_camera_debug::execute(
        ctx.state,
        ctx.router_tx,
        &ctx.identity.principal,
        ctx.identity.classification.reason,
        ctx.read_only_header,
        ctx.deadline,
        args,
    )
    .map_err(ToolError::from_control)?;
    Ok((outcome.result, outcome.bypass_redaction))
}

fn dispatch_diagnostic(
    ctx: &McpContext<'_>,
    audit_action: &str,
    handler: fn(
        &ServerState,
        &FacadeSender<RouterMessage>,
        &super::ApiPrincipal,
        &Value,
    ) -> Result<Value, ControlCommandError>,
    args: &Value,
) -> Result<ToolCallResult, ToolError> {
    match handler(ctx.state, ctx.router_tx, &ctx.identity.principal, args) {
        Ok(value) => {
            if ctx.is_admin() {
                ctx.audit(audit_action, None, "success");
            }
            Ok(ToolCallResult::json(value))
        }
        Err(error) => {
            if ctx.is_admin() {
                ctx.audit(audit_action, None, "failed");
            }
            Err(ToolError::from_control(error))
        }
    }
}

fn tool_probe_camera_streams(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    match mcp_catalog_probe::probe_camera_streams(
        ctx.state,
        ctx.router_tx,
        &ctx.identity.principal,
        &args,
        ctx.read_only_header,
        Some(ctx.deadline),
    ) {
        Ok(value) => {
            ctx.audit("mcp_probe_camera_streams", None, "success");
            Ok(ToolCallResult::json(value))
        }
        Err(error) => {
            if ctx.is_admin() {
                ctx.audit("mcp_probe_camera_streams", None, "failed");
            }
            Err(ToolError::from_control(error))
        }
    }
}

fn tool_request_secret_entry(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    let payload = mcp_secret_handoff::request_secret_entry(
        ctx.state,
        &ctx.identity.principal,
        ctx.identity.classification.reason,
        ctx.host_port,
        &args,
    )
    .map_err(ToolError::from_control)?;
    Ok(ToolCallResult::json(payload))
}

fn tool_get_secret_entry_status(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    let payload =
        mcp_secret_handoff::get_secret_entry_status(ctx.state, &ctx.identity.principal, &args)
            .map_err(ToolError::from_control)?;
    Ok(ToolCallResult::json(payload))
}

fn tool_get_camera_device_settings(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    let camera_id = args
        .get("camera_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::invalid_params("get_camera_device_settings requires camera_id"))?
        .to_owned();
    match mcp_device_settings::get_camera_device_settings(
        ctx.state,
        ctx.router_tx,
        &ctx.identity.principal,
        &camera_id,
        &args,
        ctx.deadline,
    ) {
        Ok(payload) => {
            ctx.audit(
                "mcp_get_camera_device_settings",
                Some(&camera_id),
                "success",
            );
            Ok(ToolCallResult::json(payload))
        }
        Err(error) => {
            if ctx.is_admin() {
                ctx.audit("mcp_get_camera_device_settings", Some(&camera_id), "failed");
            }
            Err(ToolError::from_control(error))
        }
    }
}

fn tool_update_camera_device_settings(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    let camera_id = args
        .get("camera_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ToolError::invalid_params("update_camera_device_settings requires camera_id")
        })?
        .to_owned();
    let policy = mcp_device_settings::McpDeviceWritePolicy {
        allow_camera_device_writes: ctx.state.mcp.config().allow_camera_device_writes,
    };
    let payload = mcp_device_settings::update_camera_device_settings_classified(
        ctx.state,
        ctx.router_tx,
        &ctx.identity.principal,
        ctx.identity.classification.reason,
        &camera_id,
        &args,
        policy,
        ctx.read_only_header,
        ctx.deadline,
    )
    .map_err(ToolError::from_control)?;
    Ok(ToolCallResult::json(payload))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetCameraSnapshotArgs {
    camera_id: String,
    source_preference: Option<String>,
}

fn tool_get_camera_snapshot(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    let args: GetCameraSnapshotArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid get_camera_snapshot params: {error}"))
    })?;
    let cfg = ctx.state.mcp.config();
    let policy = mcp_snapshot::McpSnapshotPolicy {
        allow_snapshots: cfg.allow_snapshots,
        max_bytes: cfg.max_snapshot_bytes,
        max_dimension: cfg.max_snapshot_dimension,
        min_interval_secs: cfg.snapshot_min_interval_seconds,
    };
    let request = mcp_snapshot::SnapshotRequest {
        principal: &ctx.identity.principal,
        classification: ctx.identity.classification.reason,
        camera_id: &args.camera_id,
        source_preference: args.source_preference.as_deref(),
        policy: &policy,
        limiter: ctx.state.mcp.snapshot_limiter(),
        now: Instant::now(),
        deadline: ctx.deadline,
    };
    let mut payload = mcp_snapshot::capture_camera_snapshot(ctx.state, &request)
        .map_err(ToolError::from_control)?;
    let data = payload
        .as_object_mut()
        .and_then(|map| map.remove("data_base64"))
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default();
    let mime_type = payload
        .get("mime_type")
        .and_then(Value::as_str)
        .unwrap_or("image/jpeg")
        .to_owned();
    let summary_text = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_owned());
    Ok(ToolCallResult {
        content: vec![
            ToolContent::Image { data, mime_type },
            ToolContent::Text { text: summary_text },
        ],
        structured_content: Some(payload),
        is_error: false,
    })
}

fn tool_get_server_health(ctx: &McpContext<'_>) -> Result<ToolCallResult, ToolError> {
    let mut health = server_health(ctx.router_tx, ctx.state);
    if !ctx.is_admin() {
        let policy = camera_access::for_principal(ctx.state, &ctx.identity.principal)
            .map_err(ToolError::from_control)?;
        filter_health_for_user(&mut health, &policy);
    }
    let payload = serde_json::to_value(&health)
        .map_err(|error| ToolError::internal(format!("failed to encode server health: {error}")))?;
    Ok(ToolCallResult::json(payload))
}

fn filter_health_for_user(health: &mut ServerHealthResponse, policy: &crate::access::CameraAccess) {
    health.issues.retain(|issue| issue.scope != "mcp");
    if policy.all_cameras {
        health.status = server_health_status(&health.issues).to_owned();
        return;
    }
    health.cameras.retain(|camera| policy.allows(&camera.id));
    health
        .operational_events
        .retain(|event| policy.allows(&event.key.camera_id));
    let allowed_event_ids: HashSet<&str> = health
        .operational_events
        .iter()
        .map(|event| event.id.as_str())
        .collect();
    health.issues.retain(|issue| {
        issue
            .operational_event_id
            .as_deref()
            .is_some_and(|id| allowed_event_ids.contains(id))
    });
    let mut totals = HealthTotals {
        configured_cameras: health.cameras.len(),
        ..HealthTotals::default()
    };
    for camera in &health.cameras {
        if camera.dimensions.transport_connected == Some(true) {
            totals.connected_cameras += 1;
        }
        if camera.dimensions.frames_fresh == Some(true) {
            totals.fresh_cameras += 1;
        }
        if camera.dimensions.decodable == Some(true) {
            totals.decodable_cameras += 1;
        }
        if camera.dimensions.recording_requested {
            totals.recording_requested_cameras += 1;
        }
        if camera.dimensions.recording_progressing == Some(true) {
            totals.recording_cameras += 1;
        }
        if camera.state == CameraHealthState::Unknown {
            totals.unknown_cameras += 1;
        }
    }
    health.totals = totals;
    health.status = server_health_status(&health.issues).to_owned();
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchLogsArgs {
    level: Option<LogLevel>,
    target: Option<String>,
    camera: Option<String>,
    query: Option<String>,
    since_sequence: Option<u64>,
    since_timestamp_ms: Option<u64>,
    limit: Option<usize>,
}

fn tool_search_logs(ctx: &McpContext<'_>, args: Value) -> Result<ToolCallResult, ToolError> {
    ctx.require_admin()?;
    let args: SearchLogsArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid search_logs params: {error}"))
    })?;
    let Some(logging_service) = &ctx.state.logging else {
        ctx.audit("mcp_search_logs", None, "unavailable");
        return Err(ToolError::internal("logging service is unavailable"));
    };
    let max_entries = logging_service.settings().buffer.max_entries;
    let snapshot = logging_service.snapshot(args.since_sequence, max_entries);
    let min_rank = args.level.map(LogLevel::rank);
    let target_needle = args.target.as_deref().map(str::to_ascii_lowercase);
    let camera_needle = args.camera.as_deref().map(str::to_ascii_lowercase);
    let query_needle = args.query.as_deref().map(str::to_ascii_lowercase);
    let mut matched = snapshot
        .entries
        .into_iter()
        .filter(|entry| {
            min_rank.is_none_or(|rank| entry.level.rank() >= rank)
                && args
                    .since_timestamp_ms
                    .is_none_or(|since| entry.timestamp_ms >= since)
                && target_needle
                    .as_deref()
                    .is_none_or(|needle| entry.target.to_ascii_lowercase().contains(needle))
                && camera_needle
                    .as_deref()
                    .is_none_or(|needle| entry.matches_query(needle))
                && query_needle
                    .as_deref()
                    .is_none_or(|needle| entry.matches_query(needle))
        })
        .collect::<Vec<_>>();
    let matched_count = matched.len();
    let limit = args
        .limit
        .unwrap_or(DEFAULT_LOG_SEARCH_LIMIT)
        .clamp(1, MAX_LOG_SEARCH_LIMIT);
    let truncated = snapshot.truncated || matched_count > limit;
    if matched.len() > limit {
        matched.drain(..matched.len() - limit);
    }
    ctx.audit("mcp_search_logs", None, "success");
    Ok(ToolCallResult::json(json!({
        "entries": matched,
        "matched_count": matched_count,
        "returned_count": matched.len(),
        "oldest_sequence": snapshot.oldest_sequence,
        "newest_sequence": snapshot.newest_sequence,
        "truncated": truncated,
        "generation": snapshot.generation,
    })))
}

fn tool_get_logging_settings(ctx: &McpContext<'_>) -> Result<ToolCallResult, ToolError> {
    ctx.require_admin()?;
    let settings = get_logging_settings(ctx.state).map_err(ToolError::from_control)?;
    ctx.audit("mcp_get_logging_settings", None, "success");
    let mut value = serde_json::to_value(&settings).map_err(|error| {
        ToolError::internal(format!("failed to encode logging settings: {error}"))
    })?;
    if let Some(logging) = &ctx.state.logging
        && let Some(object) = value.as_object_mut()
    {
        object.insert(
            "base_filter".to_owned(),
            Value::String(logging.base_filter()),
        );
    }
    Ok(ToolCallResult::json(value))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetConfigurationArgs {
    page_size: Option<u32>,
    #[serde(default)]
    page_token: String,
}

fn tool_get_configuration(ctx: &McpContext<'_>, args: Value) -> Result<ToolCallResult, ToolError> {
    ctx.require_admin()?;
    let args: GetConfigurationArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid get_configuration params: {error}"))
    })?;
    let command = proto::ConfigurationCommand {
        action: Some(proto::configuration_command::Action::Get(
            proto::GetConfigurationSnapshot {
                page_size: args.page_size,
                page_token: args.page_token,
            },
        )),
    };
    let result = configuration::dispatch_as(
        ctx.state,
        SessionId::from_u64(0),
        &ctx.identity.principal,
        command,
    )
    .map_err(ToolError::from_control)?;
    ctx.audit("mcp_get_configuration", None, "success");
    let snapshot = extract_configuration_snapshot(result)?;
    Ok(ToolCallResult::json(snapshot_to_json(&snapshot)))
}

fn tool_list_secret_keys(ctx: &McpContext<'_>) -> Result<ToolCallResult, ToolError> {
    ctx.require_admin()?;
    let keys = if let Some(path) = ctx.state.camera_config_path.as_deref() {
        let secrets = config::load_secrets(path).map_err(|error| {
            ToolError::internal(format!("unable to read secrets keys: {error}"))
        })?;
        let keys = secrets.keys();
        ctx.state.mcp.refresh_secrets(path, secrets);
        keys
    } else {
        Vec::new()
    };
    ctx.audit("mcp_list_secret_keys", None, "success");
    Ok(ToolCallResult::json(json!({
        "keys": keys,
        "count": keys.len(),
        "available": ctx.state.camera_config_path.is_some(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanConfigurationChangeArgs {
    expected_configuration_revision: String,
    targets: Option<TargetSelectorInput>,
    change: ConfigurationChangeInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetSelectorInput {
    camera_ids: Option<Vec<String>>,
    group_id: Option<String>,
    all_cameras: Option<bool>,
    filtered_cameras: Option<CameraFilterInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CameraFilterInput {
    #[serde(default)]
    search: String,
    backend: Option<String>,
    recording_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationChangeInput {
    patch: Option<CameraPatchInput>,
    template_id: Option<String>,
    defaults: Option<CameraDefaultPatchInput>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OptionalUpdateInput<T> {
    Structured {
        set: Option<T>,
        #[serde(default)]
        clear: bool,
    },
    Direct(T),
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CameraPatchInput {
    display_name: Option<OptionalUpdateInput<String>>,
    manufacturer: Option<OptionalUpdateInput<String>>,
    username_secret_reference: Option<OptionalUpdateInput<String>>,
    password_secret_reference: Option<OptionalUpdateInput<String>>,
    onvif_port: Option<OptionalUpdateInput<u32>>,
    http_port: Option<OptionalUpdateInput<u32>>,
    main_rtsp_url: Option<OptionalUpdateInput<String>>,
    sub_rtsp_url: Option<OptionalUpdateInput<String>>,
    uid_secret_reference: Option<OptionalUpdateInput<String>>,
    backend: Option<OptionalUpdateInput<String>>,
    transport: Option<OptionalUpdateInput<String>>,
    record_generic_motion_events: Option<OptionalUpdateInput<bool>>,
    recording_mode: Option<OptionalUpdateInput<String>>,
    event_recording_duration_secs: Option<OptionalUpdateInput<u32>>,
    event_pre_recording_duration_secs: Option<OptionalUpdateInput<u32>>,
    event_recording_stream: Option<OptionalUpdateInput<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CameraDefaultPatchInput {
    username_secret_reference: Option<OptionalUpdateInput<String>>,
    password_secret_reference: Option<OptionalUpdateInput<String>>,
    backend: Option<OptionalUpdateInput<String>>,
    transport: Option<OptionalUpdateInput<String>>,
    record_generic_motion_events: Option<OptionalUpdateInput<bool>>,
    recording_mode: Option<OptionalUpdateInput<String>>,
    event_recording_duration_secs: Option<OptionalUpdateInput<u32>>,
    event_pre_recording_duration_secs: Option<OptionalUpdateInput<u32>>,
    event_recording_stream: Option<OptionalUpdateInput<String>>,
}

fn tool_plan_configuration_change(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    ctx.require_admin()?;
    let args: PlanConfigurationChangeArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid plan_configuration_change params: {error}"))
    })?;
    let targets = args.targets.map(build_target_selector).transpose()?;
    let change = build_configuration_change(args.change)?;
    let command = proto::ConfigurationCommand {
        action: Some(proto::configuration_command::Action::Plan(
            proto::PlanConfigurationChange {
                expected_configuration_revision: args.expected_configuration_revision,
                targets,
                change: Some(change),
            },
        )),
    };
    let result = configuration::dispatch_as(
        ctx.state,
        SessionId::from_u64(0),
        &ctx.identity.principal,
        command,
    );
    match result {
        Ok(ok) => {
            let plan = extract_configuration_plan(ok)?;
            ctx.audit(
                "mcp_plan_configuration_change",
                Some(&plan.plan_id),
                "success",
            );
            Ok(ToolCallResult::json(plan_to_json(&plan)))
        }
        Err(error) => {
            ctx.audit("mcp_plan_configuration_change", None, "failed");
            Err(ToolError::from_control(error))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyConfigurationPlanArgs {
    plan_id: String,
    expected_configuration_revision: String,
}

fn tool_apply_configuration_plan(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    ctx.require_write()?;
    let args: ApplyConfigurationPlanArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid apply_configuration_plan params: {error}"))
    })?;
    let plan_id = args.plan_id.clone();
    let command = proto::ConfigurationCommand {
        action: Some(proto::configuration_command::Action::Apply(
            proto::ApplyConfigurationPlan {
                plan_id: args.plan_id,
                expected_configuration_revision: args.expected_configuration_revision,
                administrator_confirmation: None,
            },
        )),
    };
    let result = configuration::dispatch_as(
        ctx.state,
        SessionId::from_u64(0),
        &ctx.identity.principal,
        command,
    );
    match result {
        Ok(ok) => {
            let applied = extract_configuration_applied(ok)?;
            let fallback_revision = applied
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.configuration_revision.clone())
                .or_else(|| current_configuration_revision(ctx))
                .unwrap_or_default();
            ctx.audit("mcp_apply_configuration_plan", Some(&plan_id), "success");
            Ok(ToolCallResult::json(applied_to_json(
                &applied,
                &fallback_revision,
            )))
        }
        Err(error) => {
            ctx.audit("mcp_apply_configuration_plan", Some(&plan_id), "failed");
            Err(ToolError::from_control(error))
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRuntimeConfigArgs {
    #[serde(default)]
    expected_configuration_revision: String,
    host: Option<String>,
    port: Option<u16>,
    #[serde(default)]
    move_existing_recordings: bool,
    storage: Option<RuntimeStoragePatchArgs>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeStoragePatchArgs {
    medium_term_path: Option<String>,
    long_term_path: Option<String>,
    recording_catalog_path: Option<String>,
    event_thumbnail_path: Option<String>,
    event_thumbnail_max_mb: Option<u64>,
    short_term_secs: Option<u64>,
    medium_term_secs: Option<u64>,
    flush_interval_secs: Option<u64>,
    write_buffer_bytes: Option<u64>,
    long_term_max_gb: Option<u64>,
    minimum_free_gb: Option<u64>,
    maximum_used_percent: Option<u32>,
    warning_free_gb: Option<u64>,
    critical_free_gb: Option<u64>,
    cleanup_hysteresis_gb: Option<u64>,
    pre_recording_stream_max_bytes: Option<u64>,
    pre_recording_global_max_bytes: Option<u64>,
}

fn tool_update_runtime_configuration(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    ctx.require_write()?;
    let args: UpdateRuntimeConfigArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!(
            "invalid update_runtime_configuration params: {error}"
        ))
    })?;
    if args.expected_configuration_revision.trim().is_empty() {
        return Err(ToolError::invalid_params(
            "update_runtime_configuration requires expected_configuration_revision",
        ));
    }
    let current = current_config(ctx.state);
    let storage = merge_runtime_storage(&current.storage, args.storage);
    let command = proto::RuntimeConfigurationCommand {
        action: Some(proto::runtime_configuration_command::Action::Update(
            proto::UpdateRuntimeConfiguration {
                host: args.host.unwrap_or(current.host),
                port: u32::from(args.port.unwrap_or(current.port)),
                storage: Some(storage),
                move_existing_recordings: args.move_existing_recordings,
                expected_configuration_revision: args.expected_configuration_revision,
            },
        )),
    };
    let result = runtime_configuration::dispatch(ctx.state, command);
    match result {
        Ok(proto::ok::Result::RuntimeConfigurationResult(updated)) => {
            ctx.audit("mcp_update_runtime_configuration", None, "success");
            Ok(ToolCallResult::json(runtime_config_result_to_json(
                &updated,
            )))
        }
        Ok(_) => Err(ToolError::internal(
            "unexpected runtime configuration response",
        )),
        Err(error) => {
            ctx.audit("mcp_update_runtime_configuration", None, "failed");
            Err(ToolError::from_control(error))
        }
    }
}

fn merge_runtime_storage(
    current: &crate::api::SanitizedStorage,
    patch: Option<RuntimeStoragePatchArgs>,
) -> proto::RuntimeStorageConfiguration {
    let patch = patch.unwrap_or_default();
    proto::RuntimeStorageConfiguration {
        medium_term_path: patch
            .medium_term_path
            .unwrap_or_else(|| current.medium_term_path.clone()),
        long_term_path: patch
            .long_term_path
            .unwrap_or_else(|| current.long_term_path.clone()),
        recording_catalog_path: patch
            .recording_catalog_path
            .unwrap_or_else(|| current.recording_catalog_path.clone()),
        event_thumbnail_path: patch
            .event_thumbnail_path
            .unwrap_or_else(|| current.event_thumbnail_path.clone()),
        event_thumbnail_max_mb: patch
            .event_thumbnail_max_mb
            .unwrap_or(current.event_thumbnail_max_mb),
        short_term_secs: patch.short_term_secs.unwrap_or(current.short_term_secs),
        medium_term_secs: patch.medium_term_secs.unwrap_or(current.medium_term_secs),
        flush_interval_secs: patch
            .flush_interval_secs
            .unwrap_or(current.flush_interval_secs),
        write_buffer_bytes: patch
            .write_buffer_bytes
            .unwrap_or_else(|| u64::try_from(current.write_buffer_bytes).unwrap_or(u64::MAX)),
        long_term_max_gb: patch.long_term_max_gb.unwrap_or(current.long_term_max_gb),
        minimum_free_gb: Some(patch.minimum_free_gb.unwrap_or(current.minimum_free_gb)),
        maximum_used_percent: patch
            .maximum_used_percent
            .or_else(|| current.maximum_used_percent.map(u32::from)),
        warning_free_gb: Some(patch.warning_free_gb.unwrap_or(current.warning_free_gb)),
        critical_free_gb: Some(patch.critical_free_gb.unwrap_or(current.critical_free_gb)),
        cleanup_hysteresis_gb: Some(
            patch
                .cleanup_hysteresis_gb
                .unwrap_or(current.cleanup_hysteresis_gb),
        ),
        pre_recording_stream_max_bytes: Some(
            patch
                .pre_recording_stream_max_bytes
                .unwrap_or(current.pre_recording_stream_max_bytes),
        ),
        pre_recording_global_max_bytes: Some(
            patch
                .pre_recording_global_max_bytes
                .unwrap_or(current.pre_recording_global_max_bytes),
        ),
        named_volumes: None,
        archive: None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManageTemplateArgs {
    action: String,
    expected_configuration_revision: String,
    template_id: Option<String>,
    name: Option<String>,
    expected_template_version: Option<u64>,
    template: Option<TemplateInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateInput {
    #[serde(default)]
    template_id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    values: TemplateValuesInput,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateValuesInput {
    username_secret_reference: Option<String>,
    password_secret_reference: Option<String>,
    onvif_port: Option<u32>,
    http_port: Option<u32>,
    backend: Option<String>,
    transport: Option<String>,
    record_generic_motion_events: Option<bool>,
    recording_mode: Option<String>,
    event_recording_duration_secs: Option<u32>,
    event_pre_recording_duration_secs: Option<u32>,
    event_recording_stream: Option<String>,
}

fn tool_manage_configuration_template(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    ctx.require_write()?;
    let args: ManageTemplateArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!(
            "invalid manage_configuration_template params: {error}"
        ))
    })?;
    let target = args.template_id.clone();
    let command = build_template_command(args)?;
    let result = configuration::dispatch_as(
        ctx.state,
        SessionId::from_u64(0),
        &ctx.identity.principal,
        command,
    );
    match result {
        Ok(proto::ok::Result::ConfigurationResult(proto::ConfigurationResult {
            result: Some(proto::configuration_result::Result::Template(template)),
        })) => {
            ctx.audit(
                "mcp_manage_configuration_template",
                Some(&template.template_id),
                "success",
            );
            Ok(ToolCallResult::json(template_to_json(&template)))
        }
        Ok(proto::ok::Result::ConfigurationResult(proto::ConfigurationResult {
            result: Some(proto::configuration_result::Result::Snapshot(snapshot)),
        })) => {
            ctx.audit(
                "mcp_manage_configuration_template",
                target.as_deref(),
                "success",
            );
            Ok(ToolCallResult::json(snapshot_to_json(&snapshot)))
        }
        Ok(_) => Err(ToolError::internal("unexpected template response")),
        Err(error) => {
            ctx.audit(
                "mcp_manage_configuration_template",
                target.as_deref(),
                "failed",
            );
            Err(ToolError::from_control(error))
        }
    }
}

fn build_template_command(
    args: ManageTemplateArgs,
) -> Result<proto::ConfigurationCommand, ToolError> {
    let action = match args.action.as_str() {
        "save" => {
            let input = args
                .template
                .ok_or_else(|| ToolError::invalid_params("save action requires template object"))?;
            let values = build_template_values(input.values)?;
            proto::configuration_command::Action::SaveTemplate(proto::SaveConfigurationTemplate {
                expected_configuration_revision: args.expected_configuration_revision,
                template: Some(proto::ConfigurationTemplate {
                    template_id: input.template_id,
                    version: 0,
                    name: input.name,
                    description: input.description,
                    values: Some(values),
                    created_at_ms: 0,
                    updated_at_ms: 0,
                }),
                expected_template_version: args.expected_template_version,
            })
        }
        "duplicate" => {
            let template_id = args.template_id.ok_or_else(|| {
                ToolError::invalid_params("duplicate action requires template_id")
            })?;
            let name = args
                .name
                .ok_or_else(|| ToolError::invalid_params("duplicate action requires name"))?;
            proto::configuration_command::Action::DuplicateTemplate(
                proto::DuplicateConfigurationTemplate {
                    expected_configuration_revision: args.expected_configuration_revision,
                    template_id,
                    name,
                },
            )
        }
        "delete" => {
            let template_id = args
                .template_id
                .ok_or_else(|| ToolError::invalid_params("delete action requires template_id"))?;
            proto::configuration_command::Action::DeleteTemplate(
                proto::DeleteConfigurationTemplate {
                    expected_configuration_revision: args.expected_configuration_revision,
                    template_id,
                },
            )
        }
        other => {
            return Err(ToolError::invalid_params(format!(
                "unsupported template action: {other}"
            )));
        }
    };
    Ok(proto::ConfigurationCommand {
        action: Some(action),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetRuntimeProfileArgs {
    expected_revision: u64,
    #[serde(default)]
    profile_id: Option<String>,
    definition_revision: Option<u64>,
    definition_digest: Option<String>,
}

fn tool_set_runtime_profile(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    ctx.require_write()?;
    let args: SetRuntimeProfileArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid set_runtime_profile params: {error}"))
    })?;
    let profile_id = args.profile_id.unwrap_or_default();
    let command = build_runtime_profile_put(
        &profile_id,
        args.expected_revision,
        args.definition_revision,
        args.definition_digest,
    )?;
    let target = if profile_id.is_empty() {
        "base"
    } else {
        profile_id.as_str()
    };
    let result = state_store::dispatch(
        ctx.state,
        SessionId::from_u64(0),
        &ctx.identity.principal,
        command,
    );
    match result {
        Ok(proto::ok::Result::StateStoreResult(proto::StateStoreResult {
            result: Some(proto::state_store_result::Result::Entry(entry)),
        })) => {
            ctx.audit("mcp_set_runtime_profile", Some(target), "success");
            Ok(ToolCallResult::json(json!({
                "namespace": entry.namespace,
                "key": entry.key,
                "revision": entry.revision,
                "active_profile_id": if profile_id.is_empty() { Value::Null } else { Value::String(profile_id) },
                "value": entry.value.as_ref().map(prost_struct_to_json),
            })))
        }
        Ok(_) => Err(ToolError::internal("unexpected runtime profile response")),
        Err(error) => {
            ctx.audit("mcp_set_runtime_profile", Some(target), "failed");
            Err(ToolError::from_control(error))
        }
    }
}

fn build_runtime_profile_put(
    profile_id: &str,
    expected_revision: u64,
    definition_revision: Option<u64>,
    definition_digest: Option<String>,
) -> Result<proto::StateStoreCommand, ToolError> {
    let mut fields = BTreeMap::new();
    fields.insert(
        "profile_id".to_owned(),
        ProstValue {
            kind: Some(Kind::StringValue(profile_id.to_owned())),
        },
    );
    let schema = match (definition_revision, definition_digest) {
        (Some(revision), Some(digest)) => {
            fields.insert(
                "definition_revision".to_owned(),
                ProstValue {
                    kind: Some(Kind::StringValue(revision.to_string())),
                },
            );
            fields.insert(
                "definition_digest".to_owned(),
                ProstValue {
                    kind: Some(Kind::StringValue(digest)),
                },
            );
            PROFILE_SELECT_SCHEMA
        }
        (None, None) => PROFILE_ACTIVE_SCHEMA,
        _ => {
            return Err(ToolError::invalid_params(
                "definition_revision and definition_digest must be provided together",
            ));
        }
    };
    Ok(proto::StateStoreCommand {
        action: Some(proto::state_store_command::Action::Put(proto::PutState {
            namespace: state_store::profiles::NAMESPACE.to_owned(),
            key: "active".to_owned(),
            schema: schema.to_owned(),
            value: Some(Struct { fields }),
            expected_revision: Some(expected_revision),
            ttl: None,
        })),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetLoggingFilterArgs {
    filter: String,
}

fn tool_set_logging_filter(ctx: &McpContext<'_>, args: Value) -> Result<ToolCallResult, ToolError> {
    ctx.require_write()?;
    let args: SetLoggingFilterArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid set_logging_filter params: {error}"))
    })?;
    let command = proto::LoggingCommand {
        action: Some(proto::logging_command::Action::SetFilter(
            proto::SetLoggingFilter {
                filter: args.filter,
            },
        )),
    };
    match logging::dispatch(ctx.state, command) {
        Ok(proto::ok::Result::LoggingSettingsResult(settings)) => {
            ctx.audit("mcp_set_logging_filter", None, "success");
            Ok(ToolCallResult::json(json!({
                "active_filter": settings.active_filter,
                "default_filter": settings.default_filter,
                "filter_error": settings.filter_error,
                "version": settings.version,
            })))
        }
        Ok(_) => Err(ToolError::internal("unexpected logging filter response")),
        Err(error) => {
            ctx.audit("mcp_set_logging_filter", None, "failed");
            Err(ToolError::from_control(error))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisableMcpAccessArgs {
    confirm: bool,
    #[serde(default)]
    reason: Option<String>,
}

fn tool_disable_mcp_access(ctx: &McpContext<'_>, args: Value) -> Result<ToolCallResult, ToolError> {
    ctx.require_admin()?;
    if ctx.read_only_header {
        return Err(ToolError::forbidden(
            "disable_mcp_access is blocked while X-KeepPeek-MCP-Read-Only: true is set",
        ));
    }
    let args: DisableMcpAccessArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid disable_mcp_access params: {error}"))
    })?;
    if !args.confirm {
        return Err(ToolError::invalid_params(
            "disable_mcp_access requires confirm = true",
        ));
    }
    if let Some(reason) = args.reason.as_deref() {
        let trimmed = reason.trim();
        if trimmed.is_empty()
            || reason.chars().count() > 256
            || reason.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
        {
            return Err(ToolError::invalid_params(
                "disable_mcp_access reason must be 1 to 256 printable characters",
            ));
        }
    }
    let now_secs = (unix_time_ms() / 1_000).max(1);
    let _lock = ctx
        .state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let next_mcp = if let Some(path) = ctx.state.camera_config_path.as_deref() {
        match config::mcp::update(path, None, |mcp| {
            mcp.apply_enabled_transition(false, now_secs);
            Ok(())
        }) {
            Ok((updated, _)) => updated.mcp,
            Err(error) => {
                // Fail safe: disable live MCP even when config.toml persistence fails.
                let mut live_disabled = ctx.state.mcp.config();
                live_disabled.apply_enabled_transition(false, now_secs);
                ctx.state.mcp.apply_live_config(live_disabled);
                ctx.audit("mcp_disable_access", None, "live_only_persist_failed");
                return Err(ToolError::internal(format!(
                    "MCP access was disabled live, but failed to persist disabled state to config.toml: {error}"
                )));
            }
        }
    } else {
        let mut updated = ctx.state.mcp.config();
        updated.apply_enabled_transition(false, now_secs);
        updated
    };
    ctx.state.mcp.apply_live_config(next_mcp);
    ctx.audit("mcp_disable_access", None, "success");
    let redacted_reason = args.reason.as_deref().map(|reason| {
        let secrets = ctx
            .state
            .mcp
            .secrets_snapshot(ctx.state.camera_config_path.as_deref());
        let redactor = SecretRedactor::new(&secrets, std::iter::empty(), None);
        redactor.redact_str(reason.trim())
    });
    tracing::info!(
        principal_id = %ctx.identity.principal.id(),
        reason = redacted_reason.as_deref().unwrap_or(""),
        "MCP access disabled by Administrator MCP tool call"
    );
    Ok(ToolCallResult::json(json!({
        "disabled": true,
        "effective_immediately": true,
        "re_enable_requires": "human_administrator_browser_or_config_edit",
    })))
}

fn deserialize_double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManageCameraArgs {
    action: String,
    ip: String,
    expected_configuration_revision: String,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    manufacturer: Option<Option<String>>,
    username: Option<String>,
    password: Option<String>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    uid: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    onvif_port: Option<Option<u16>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    http_port: Option<Option<u16>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    main_rtsp_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    sub_rtsp_url: Option<Option<String>>,
    backend: Option<String>,
    transport: Option<String>,
    record_generic_motion_events: Option<bool>,
    recording_mode: Option<String>,
    event_recording_duration_secs: Option<u64>,
    event_pre_recording_duration_secs: Option<u64>,
    event_recording_stream: Option<String>,
}

/// Translates a current `get_configuration` revision to the camera settings revision.
///
/// KeepPeek computes separate revision hashes for full configuration snapshots
/// (`get_configuration`) and camera/runtime settings (`get_runtime_configuration`).
/// Translating only an exact match of the current full revision lets callers pass
/// either revision while still rejecting stale revisions with compare-and-set.
fn resolve_camera_mutation_revision(
    ctx: &McpContext<'_>,
    expected: &str,
) -> Result<String, ToolError> {
    if current_configuration_revision(ctx).as_deref() == Some(expected) {
        return camera_configuration_revision(ctx.state).map_err(ToolError::from_control);
    }
    Ok(expected.to_owned())
}

fn tool_manage_camera(ctx: &McpContext<'_>, args: Value) -> Result<ToolCallResult, ToolError> {
    ctx.require_write()?;
    let mut args: ManageCameraArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid manage_camera params: {error}"))
    })?;
    if args.expected_configuration_revision.trim().is_empty() {
        return Err(ToolError::invalid_params(
            "manage_camera requires expected_configuration_revision",
        ));
    }
    args.expected_configuration_revision =
        resolve_camera_mutation_revision(ctx, args.expected_configuration_revision.trim())?;
    let camera_id = args
        .ip
        .trim()
        .parse::<IpAddr>()
        .map_err(|_| ToolError::invalid_params("manage_camera ip must be a valid IP address"))?
        .to_string();
    match args.action.as_str() {
        "remove" => match delete_camera_settings(
            ctx.state,
            &camera_id,
            &args.expected_configuration_revision,
        ) {
            Ok(revision) => {
                ctx.audit("mcp_manage_camera", Some(&camera_id), "success");
                Ok(ToolCallResult::json(json!({
                    "action": "remove",
                    "camera_id": camera_id,
                    "configuration_revision": revision,
                })))
            }
            Err(error) => {
                ctx.audit("mcp_manage_camera", Some(&camera_id), "failed");
                Err(ToolError::from_control(error))
            }
        },
        "upsert" => upsert_camera_via_mcp(ctx, &camera_id, args),
        other => Err(ToolError::invalid_params(format!(
            "unsupported manage_camera action: {other}"
        ))),
    }
}

fn upsert_camera_via_mcp(
    ctx: &McpContext<'_>,
    camera_id: &str,
    args: ManageCameraArgs,
) -> Result<ToolCallResult, ToolError> {
    validate_manage_camera_secrets(&args)?;
    let update = build_camera_settings_update(args)?;
    match save_camera_settings(update, ctx.router_tx, ctx.state, camera_id) {
        Ok(response) => {
            if let Some(path) = ctx.state.camera_config_path.as_deref()
                && let Ok(secrets) = config::load_secrets(path)
            {
                ctx.state.mcp.refresh_secrets(path, secrets);
            }
            ctx.audit("mcp_manage_camera", Some(camera_id), "success");
            let payload = serde_json::to_value(response).map_err(|error| {
                ToolError::internal(format!("failed to encode camera update response: {error}"))
            })?;
            Ok(ToolCallResult::json(payload))
        }
        Err(error) => {
            ctx.audit("mcp_manage_camera", Some(camera_id), "failed");
            Err(ToolError::from_control(error))
        }
    }
}

fn validate_manage_camera_secrets(args: &ManageCameraArgs) -> Result<(), ToolError> {
    validate_secret_ref_field(args.username.as_deref(), "username")?;
    validate_secret_ref_field(args.password.as_deref(), "password")?;
    if let Some(uid) = args.uid.as_ref().and_then(Option::as_deref) {
        validate_secret_ref_field(Some(uid), "uid")?;
    }
    if let Some(url) = args.main_rtsp_url.as_ref().and_then(Option::as_deref) {
        validate_rtsp_url_secret_policy(url, "main_rtsp_url")?;
    }
    if let Some(url) = args.sub_rtsp_url.as_ref().and_then(Option::as_deref) {
        validate_rtsp_url_secret_policy(url, "sub_rtsp_url")?;
    }
    Ok(())
}

fn validate_secret_ref_field(value: Option<&str>, field: &str) -> Result<(), ToolError> {
    let Some(raw) = value.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(());
    };
    if config::is_secret_reference(raw) {
        Ok(())
    } else {
        Err(ToolError::invalid_params(format!(
            "manage_camera {field} must be a {{secret:KEY}} or {{secret:KEY|url}} reference; raw credentials are rejected"
        )))
    }
}

const RTSP_SENSITIVE_QUERY_KEYS: &[&str] = &[
    "user",
    "username",
    "pass",
    "password",
    "passwd",
    "pwd",
    "token",
    "auth",
    "secret",
    "access_key",
    "apikey",
    "api_key",
];

fn validate_rtsp_url_secret_policy(url: &str, field: &str) -> Result<(), ToolError> {
    if let Some(scheme_pos) = url.find("://") {
        let authority_start = scheme_pos + 3;
        let authority_end = url[authority_start..]
            .find(['/', '?', '#'])
            .map_or(url.len(), |offset| authority_start + offset);
        let authority = &url[authority_start..authority_end];
        if let Some(at_pos) = authority.rfind('@') {
            let userinfo = &authority[..at_pos];
            let uses_ref = config::is_secret_reference(userinfo)
                || userinfo.split_once(':').is_some_and(|(user, pass)| {
                    !user.is_empty() && config::is_secret_reference(pass)
                });
            if !uses_ref {
                return Err(ToolError::invalid_params(format!(
                    "{field} must not contain plaintext credentials; use {{secret:KEY|url}}"
                )));
            }
        }
    }
    validate_rtsp_query_secret_policy(url, field)
}

fn validate_rtsp_query_secret_policy(url: &str, field: &str) -> Result<(), ToolError> {
    let Some((_, rest)) = url.split_once('?') else {
        return Ok(());
    };
    let query = rest.split('#').next().unwrap_or(rest);
    for pair in query.split('&') {
        let Some((raw_key, raw_value)) = pair.split_once('=') else {
            continue;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        let value = raw_value.trim();
        if !value.is_empty()
            && RTSP_SENSITIVE_QUERY_KEYS.contains(&key.as_str())
            && !config::is_secret_reference(value)
        {
            return Err(ToolError::invalid_params(format!(
                "{field} query parameter '{raw_key}' must not contain plaintext credentials; use {{secret:KEY|url}}"
            )));
        }
    }
    Ok(())
}

fn build_camera_settings_update(args: ManageCameraArgs) -> Result<CameraSettingsUpdate, ToolError> {
    Ok(CameraSettingsUpdate {
        expected_configuration_revision: args.expected_configuration_revision,
        display_name: args.display_name,
        manufacturer: args.manufacturer,
        username: args.username,
        password: args.password,
        onvif_port: args.onvif_port,
        http_port: args.http_port,
        main_rtsp_url: args.main_rtsp_url,
        sub_rtsp_url: args.sub_rtsp_url,
        uid: args.uid,
        backend: args
            .backend
            .as_deref()
            .map(parse_camera_backend)
            .transpose()?,
        transport: args
            .transport
            .as_deref()
            .map(parse_camera_transport)
            .transpose()?,
        record_generic_motion_events: args.record_generic_motion_events,
        recording_mode: args
            .recording_mode
            .as_deref()
            .map(parse_camera_recording_mode)
            .transpose()?,
        event_recording_duration_secs: args.event_recording_duration_secs,
        event_pre_recording_duration_secs: args.event_pre_recording_duration_secs,
        event_recording_stream: args
            .event_recording_stream
            .as_deref()
            .map(parse_camera_event_stream)
            .transpose()?,
        recorded_audio: None,
        live_audio: None,
    })
}

fn parse_camera_backend(raw: &str) -> Result<CameraBackend, ToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "auto" => Ok(CameraBackend::Auto),
        "retina" => Ok(CameraBackend::Retina),
        "reo_proto" | "reo-proto" => Ok(CameraBackend::ReoProto),
        _ => Err(ToolError::invalid_params(format!(
            "unsupported camera backend: {raw}"
        ))),
    }
}

fn parse_camera_transport(raw: &str) -> Result<CameraTransport, ToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "tcp" => Ok(CameraTransport::Tcp),
        "udp" => Ok(CameraTransport::Udp),
        _ => Err(ToolError::invalid_params(format!(
            "unsupported camera transport: {raw}"
        ))),
    }
}

fn parse_camera_recording_mode(raw: &str) -> Result<CameraRecordingMode, ToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "sub" => Ok(CameraRecordingMode::Sub),
        "main" => Ok(CameraRecordingMode::Main),
        "both" => Ok(CameraRecordingMode::Both),
        "event_boost" | "event-boost" => Ok(CameraRecordingMode::EventBoost),
        "off" => Ok(CameraRecordingMode::Off),
        "event_only" | "event-only" => Ok(CameraRecordingMode::EventOnly),
        _ => Err(ToolError::invalid_params(format!(
            "unsupported camera recording_mode: {raw}"
        ))),
    }
}

fn parse_camera_event_stream(raw: &str) -> Result<EventRecordingStream, ToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "sub" => Ok(EventRecordingStream::Sub),
        "main" => Ok(EventRecordingStream::Main),
        _ => Err(ToolError::invalid_params(format!(
            "unsupported event_recording_stream: {raw}"
        ))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManageNotificationRuleArgs {
    action: String,
    rule_id: Option<String>,
    expected_active_revision: Option<u64>,
    expected_draft_revision: Option<u64>,
    definition: Option<Value>,
}

fn tool_manage_notification_rule(
    ctx: &McpContext<'_>,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    let args: ManageNotificationRuleArgs = serde_json::from_value(args).map_err(|error| {
        ToolError::invalid_params(format!("invalid manage_notification_rule params: {error}"))
    })?;
    if args.action == "list" {
        ctx.require_admin()?;
    } else {
        ctx.require_write()?;
    }
    let notifications = ctx
        .state
        .notifications
        .as_ref()
        .ok_or_else(|| ToolError::internal("notification runtime is unavailable"))?;
    let principal_id = if ctx.identity.principal.is_local() {
        "local-administrator".to_owned()
    } else {
        ctx.identity.principal.id()
    };
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    match args.action.as_str() {
        "list" => list_notification_rules_via_mcp(ctx, notifications, &principal_id),
        "save_draft" => {
            save_notification_draft_via_mcp(ctx, notifications, &principal_id, args, now_ms)
        }
        "activate" => {
            activate_notification_rule_via_mcp(ctx, notifications, &principal_id, args, now_ms)
        }
        "delete" => {
            delete_notification_rule_via_mcp(ctx, notifications, &principal_id, args, now_ms)
        }
        other => Err(ToolError::invalid_params(format!(
            "unsupported manage_notification_rule action: {other}"
        ))),
    }
}

fn list_notification_rules_via_mcp(
    ctx: &McpContext<'_>,
    notifications: &NotificationHandle,
    principal_id: &str,
) -> Result<ToolCallResult, ToolError> {
    let records = notifications.rules(principal_id).map_err(|error| {
        ToolError::from_control(notification_command_error("list rules", error, false))
    })?;
    let rules = records
        .into_iter()
        .map(notification_rule_record_to_json)
        .collect::<Result<Vec<_>, _>>()?;
    ctx.audit("mcp_manage_notification_rule", None, "success");
    Ok(ToolCallResult::json(json!({
        "rules": rules,
        "count": rules.len(),
    })))
}

fn save_notification_draft_via_mcp(
    ctx: &McpContext<'_>,
    notifications: &NotificationHandle,
    principal_id: &str,
    args: ManageNotificationRuleArgs,
    now_ms: i64,
) -> Result<ToolCallResult, ToolError> {
    let mut definition = args
        .definition
        .ok_or_else(|| ToolError::invalid_params("save_draft action requires definition object"))?;
    let serialized_len = serde_json::to_vec(&definition)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if serialized_len > MAX_NOTIFICATION_RULE_JSON_BYTES {
        return Err(ToolError::invalid_params(
            "notification rule definition exceeds maximum size",
        ));
    }
    let rule_id = definition
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::invalid_params("notification rule definition has no rule ID"))?
        .to_owned();
    validate_and_resolve_notification_actions(ctx.state, &mut definition)?;
    let existing = notifications
        .rules(principal_id)
        .map_err(|error| {
            ToolError::from_control(notification_command_error("load rule draft", error, false))
        })?
        .into_iter()
        .find(|record| record.id == rule_id);
    restore_notification_destinations(&mut definition, existing.as_ref())
        .map_err(ToolError::from_control)?;
    if let Some(object) = definition.as_object_mut() {
        object.insert(
            "owner_id".to_owned(),
            Value::String(principal_id.to_owned()),
        );
    }
    let mut rule: NotificationRule = serde_json::from_value(definition).map_err(|error| {
        ToolError::invalid_params(format!("notification rule definition is invalid: {error}"))
    })?;
    rule.owner_id = principal_id.to_owned();
    let record = notifications
        .save_draft(rule, args.expected_draft_revision.unwrap_or(0), now_ms)
        .map_err(|error| {
            ctx.audit("mcp_manage_notification_rule", Some(&rule_id), "failed");
            ToolError::from_control(notification_command_error("save draft", error, true))
        })?;
    ctx.audit("mcp_manage_notification_rule", Some(&rule_id), "success");
    Ok(ToolCallResult::json(notification_rule_record_to_json(
        record,
    )?))
}

fn validate_and_resolve_notification_actions(
    state: &ServerState,
    definition: &mut Value,
) -> Result<(), ToolError> {
    let Some(actions) = definition.get_mut("actions").and_then(Value::as_array_mut) else {
        return Err(ToolError::invalid_params(
            "notification rule definition requires an actions array",
        ));
    };
    for action in actions.iter_mut() {
        let Some(object) = action.as_object_mut() else {
            return Err(ToolError::invalid_params(
                "notification rule action must be a JSON object",
            ));
        };
        let channel = object.get("channel").and_then(Value::as_str);
        if !matches!(channel, Some("push" | "webhook" | "forwarder")) {
            continue;
        }
        let preserve = object
            .get("destination_configured")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let destination = object
            .get("destination")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned();
        if preserve {
            if !destination.is_empty() && !config::is_secret_reference(&destination) {
                return Err(ToolError::invalid_params(
                    "notification destinations must use {secret:KEY} references or omit destination when destination_configured is true",
                ));
            }
            continue;
        }
        if !config::is_secret_reference(&destination) {
            return Err(ToolError::invalid_params(
                "notification destinations for push, webhook, and forwarder channels must use {secret:KEY} references or preserve destination_ref; raw credentials are rejected",
            ));
        }
        let Some(config_path) = state.camera_config_path.as_deref() else {
            return Err(ToolError::invalid_params(
                "secrets.toml is unavailable to resolve notification destination secret reference",
            ));
        };
        let resolved =
            config::resolve_secret_references(config_path, &destination).map_err(|error| {
                ToolError::invalid_params(format!(
                    "invalid notification destination secret reference: {error}"
                ))
            })?;
        if let Ok(secrets) = config::load_secrets(config_path) {
            state.mcp.refresh_secrets(config_path, secrets);
        }
        object.insert("destination".to_owned(), Value::String(resolved));
    }
    Ok(())
}

fn activate_notification_rule_via_mcp(
    ctx: &McpContext<'_>,
    notifications: &NotificationHandle,
    principal_id: &str,
    args: ManageNotificationRuleArgs,
    now_ms: i64,
) -> Result<ToolCallResult, ToolError> {
    let rule_id = args
        .rule_id
        .ok_or_else(|| ToolError::invalid_params("activate action requires rule_id"))?;
    validate_client_id(&rule_id, "notification rule ID").map_err(ToolError::from_control)?;
    let record = notifications
        .activate(
            rule_id.clone(),
            principal_id,
            args.expected_active_revision.unwrap_or(0),
            args.expected_draft_revision.unwrap_or(0),
            now_ms,
        )
        .map_err(|error| {
            ctx.audit("mcp_manage_notification_rule", Some(&rule_id), "failed");
            ToolError::from_control(notification_command_error("activate rule", error, true))
        })?;
    ctx.audit("mcp_manage_notification_rule", Some(&rule_id), "success");
    Ok(ToolCallResult::json(notification_rule_record_to_json(
        record,
    )?))
}

fn delete_notification_rule_via_mcp(
    ctx: &McpContext<'_>,
    notifications: &NotificationHandle,
    principal_id: &str,
    args: ManageNotificationRuleArgs,
    now_ms: i64,
) -> Result<ToolCallResult, ToolError> {
    let rule_id = args
        .rule_id
        .ok_or_else(|| ToolError::invalid_params("delete action requires rule_id"))?;
    validate_client_id(&rule_id, "notification rule ID").map_err(ToolError::from_control)?;
    notifications
        .delete(
            rule_id.clone(),
            principal_id,
            args.expected_active_revision.unwrap_or(0),
            args.expected_draft_revision.unwrap_or(0),
            now_ms,
        )
        .map_err(|error| {
            ctx.audit("mcp_manage_notification_rule", Some(&rule_id), "failed");
            ToolError::from_control(notification_command_error("delete rule", error, false))
        })?;
    ctx.audit("mcp_manage_notification_rule", Some(&rule_id), "success");
    Ok(ToolCallResult::json(json!({
        "action": "delete",
        "rule_id": rule_id,
    })))
}

fn notification_rule_record_to_json(
    record: crate::notifications::RuleRecord,
) -> Result<Value, ToolError> {
    let proto_record = proto_notification_rule_record(record).map_err(|error| {
        ToolError::internal(format!("failed to redact notification rule: {error}"))
    })?;
    let draft_definition: Value = serde_json::from_str(&proto_record.draft_definition_json)
        .map_err(|error| ToolError::internal(format!("invalid redacted draft JSON: {error}")))?;
    let active_definition = proto_record
        .active_definition_json
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|error| ToolError::internal(format!("invalid redacted active JSON: {error}")))?;
    Ok(json!({
        "rule_id": proto_record.rule_id,
        "owner_id": proto_record.owner_id,
        "active_revision": proto_record.active_revision,
        "draft_revision": proto_record.draft_revision,
        "active_definition": active_definition,
        "draft_definition": draft_definition,
        "created_at_ms": proto_record.created_at_ms,
        "updated_at_ms": proto_record.updated_at_ms,
        "last_match_at_ms": proto_record.last_match_at_ms,
        "last_delivery_at_ms": proto_record.last_delivery_at_ms,
    }))
}

fn build_target_selector(
    input: TargetSelectorInput,
) -> Result<proto::ConfigurationTargetSelector, ToolError> {
    let selection = match (
        input.camera_ids,
        input.group_id,
        input.all_cameras,
        input.filtered_cameras,
    ) {
        (Some(camera_ids), None, None, None) => {
            proto::configuration_target_selector::Selection::CameraIds(proto::CameraIdList {
                camera_ids,
            })
        }
        (None, Some(group_id), None, None) => {
            proto::configuration_target_selector::Selection::GroupId(group_id)
        }
        (None, None, Some(all), None) => {
            proto::configuration_target_selector::Selection::AllCameras(all)
        }
        (None, None, None, Some(filter)) => {
            proto::configuration_target_selector::Selection::FilteredCameras(
                proto::CameraConfigurationFilter {
                    search: filter.search,
                    backend: filter.backend.as_deref().map(parse_backend).transpose()?,
                    recording_mode: filter
                        .recording_mode
                        .as_deref()
                        .map(parse_recording_mode)
                        .transpose()?,
                },
            )
        }
        _ => {
            return Err(ToolError::invalid_params(
                "targets must specify exactly one of camera_ids, group_id, all_cameras, or filtered_cameras",
            ));
        }
    };
    Ok(proto::ConfigurationTargetSelector {
        selection: Some(selection),
    })
}

fn build_configuration_change(
    input: ConfigurationChangeInput,
) -> Result<proto::ConfigurationChange, ToolError> {
    let change = match (input.patch, input.template_id, input.defaults) {
        (Some(patch), None, None) => {
            proto::configuration_change::Change::Patch(build_camera_patch(patch)?)
        }
        (None, Some(template_id), None) => {
            proto::configuration_change::Change::TemplateId(template_id)
        }
        (None, None, Some(defaults)) => {
            proto::configuration_change::Change::Defaults(build_default_patch(defaults)?)
        }
        _ => {
            return Err(ToolError::invalid_params(
                "change must specify exactly one of patch, template_id, or defaults",
            ));
        }
    };
    Ok(proto::ConfigurationChange {
        change: Some(change),
    })
}

fn map_optional_update<T, P, V, Wrap>(
    input: OptionalUpdateInput<T>,
    label: &str,
    parse: impl FnOnce(T) -> Result<P, ToolError>,
    set: impl FnOnce(P) -> V,
    clear: impl FnOnce(bool) -> V,
    wrap: impl FnOnce(V) -> Wrap,
) -> Result<Wrap, ToolError> {
    let value = match input {
        OptionalUpdateInput::Direct(raw) => set(parse(raw)?),
        OptionalUpdateInput::Structured {
            set: Some(raw),
            clear: false,
        } => set(parse(raw)?),
        OptionalUpdateInput::Structured {
            set: None,
            clear: true,
        } => clear(true),
        _ => {
            return Err(ToolError::invalid_params(format!(
                "{label} must provide either set or clear=true"
            )));
        }
    };
    Ok(wrap(value))
}

fn build_camera_patch(
    input: CameraPatchInput,
) -> Result<proto::CameraConfigurationPatch, ToolError> {
    Ok(proto::CameraConfigurationPatch {
        display_name: input.display_name.map(to_string_update).transpose()?,
        manufacturer: input.manufacturer.map(to_string_update).transpose()?,
        username_secret_reference: input
            .username_secret_reference
            .map(to_string_update)
            .transpose()?,
        password_secret_reference: input
            .password_secret_reference
            .map(to_string_update)
            .transpose()?,
        onvif_port: input.onvif_port.map(to_u32_update).transpose()?,
        http_port: input.http_port.map(to_u32_update).transpose()?,
        main_rtsp_url: input
            .main_rtsp_url
            .map(|value| to_rtsp_url_update(value, "main_rtsp_url"))
            .transpose()?,
        sub_rtsp_url: input
            .sub_rtsp_url
            .map(|value| to_rtsp_url_update(value, "sub_rtsp_url"))
            .transpose()?,
        uid_secret_reference: input
            .uid_secret_reference
            .map(to_string_update)
            .transpose()?,
        backend: input.backend.map(to_backend_update).transpose()?,
        transport: input.transport.map(to_transport_update).transpose()?,
        record_generic_motion_events: input
            .record_generic_motion_events
            .map(to_bool_update)
            .transpose()?,
        recording_mode: input
            .recording_mode
            .map(to_recording_mode_update)
            .transpose()?,
        event_recording_duration_secs: input
            .event_recording_duration_secs
            .map(to_u32_update)
            .transpose()?,
        event_pre_recording_duration_secs: input
            .event_pre_recording_duration_secs
            .map(to_u32_update)
            .transpose()?,
        event_recording_stream: input
            .event_recording_stream
            .map(to_event_stream_update)
            .transpose()?,
    })
}

fn build_default_patch(
    input: CameraDefaultPatchInput,
) -> Result<proto::CameraDefaultPatch, ToolError> {
    Ok(proto::CameraDefaultPatch {
        username_secret_reference: input
            .username_secret_reference
            .map(to_string_update)
            .transpose()?,
        password_secret_reference: input
            .password_secret_reference
            .map(to_string_update)
            .transpose()?,
        backend: input.backend.map(to_backend_update).transpose()?,
        transport: input.transport.map(to_transport_update).transpose()?,
        record_generic_motion_events: input
            .record_generic_motion_events
            .map(to_bool_update)
            .transpose()?,
        recording_mode: input
            .recording_mode
            .map(to_recording_mode_update)
            .transpose()?,
        event_recording_duration_secs: input
            .event_recording_duration_secs
            .map(to_u32_update)
            .transpose()?,
        event_pre_recording_duration_secs: input
            .event_pre_recording_duration_secs
            .map(to_u32_update)
            .transpose()?,
        event_recording_stream: input
            .event_recording_stream
            .map(to_event_stream_update)
            .transpose()?,
    })
}

fn build_template_values(
    input: TemplateValuesInput,
) -> Result<proto::CameraTemplateValues, ToolError> {
    Ok(proto::CameraTemplateValues {
        username_secret_reference: input.username_secret_reference,
        password_secret_reference: input.password_secret_reference,
        onvif_port: input.onvif_port,
        http_port: input.http_port,
        backend: input.backend.as_deref().map(parse_backend).transpose()?,
        transport: input
            .transport
            .as_deref()
            .map(parse_transport)
            .transpose()?,
        record_generic_motion_events: input.record_generic_motion_events,
        recording_mode: input
            .recording_mode
            .as_deref()
            .map(parse_recording_mode)
            .transpose()?,
        event_recording_duration_secs: input.event_recording_duration_secs,
        event_pre_recording_duration_secs: input.event_pre_recording_duration_secs,
        event_recording_stream: input
            .event_recording_stream
            .as_deref()
            .map(parse_event_stream)
            .transpose()?,
    })
}

fn to_rtsp_url_update(
    input: OptionalUpdateInput<String>,
    field: &str,
) -> Result<proto::OptionalStringUpdate, ToolError> {
    map_optional_update(
        input,
        field,
        |url| {
            validate_rtsp_url_secret_policy(&url, field)?;
            Ok(url)
        },
        proto::optional_string_update::Value::Set,
        proto::optional_string_update::Value::Clear,
        |value| proto::OptionalStringUpdate { value: Some(value) },
    )
}

fn to_string_update(
    input: OptionalUpdateInput<String>,
) -> Result<proto::OptionalStringUpdate, ToolError> {
    map_optional_update(
        input,
        "optional string update",
        Ok,
        proto::optional_string_update::Value::Set,
        proto::optional_string_update::Value::Clear,
        |value| proto::OptionalStringUpdate { value: Some(value) },
    )
}

fn to_u32_update(
    input: OptionalUpdateInput<u32>,
) -> Result<proto::OptionalUint32Update, ToolError> {
    map_optional_update(
        input,
        "optional integer update",
        Ok,
        proto::optional_uint32_update::Value::Set,
        proto::optional_uint32_update::Value::Clear,
        |value| proto::OptionalUint32Update { value: Some(value) },
    )
}

fn to_bool_update(
    input: OptionalUpdateInput<bool>,
) -> Result<proto::OptionalBoolUpdate, ToolError> {
    map_optional_update(
        input,
        "optional boolean update",
        Ok,
        proto::optional_bool_update::Value::Set,
        proto::optional_bool_update::Value::Clear,
        |value| proto::OptionalBoolUpdate { value: Some(value) },
    )
}

fn to_backend_update(
    input: OptionalUpdateInput<String>,
) -> Result<proto::OptionalCameraBackendUpdate, ToolError> {
    map_optional_update(
        input,
        "backend update",
        |raw| parse_backend(&raw),
        proto::optional_camera_backend_update::Value::Set,
        proto::optional_camera_backend_update::Value::Clear,
        |value| proto::OptionalCameraBackendUpdate { value: Some(value) },
    )
}

fn to_transport_update(
    input: OptionalUpdateInput<String>,
) -> Result<proto::OptionalCameraTransportUpdate, ToolError> {
    map_optional_update(
        input,
        "transport update",
        |raw| parse_transport(&raw),
        proto::optional_camera_transport_update::Value::Set,
        proto::optional_camera_transport_update::Value::Clear,
        |value| proto::OptionalCameraTransportUpdate { value: Some(value) },
    )
}

fn to_recording_mode_update(
    input: OptionalUpdateInput<String>,
) -> Result<proto::OptionalCameraRecordingModeUpdate, ToolError> {
    map_optional_update(
        input,
        "recording_mode update",
        |raw| parse_recording_mode(&raw),
        proto::optional_camera_recording_mode_update::Value::Set,
        proto::optional_camera_recording_mode_update::Value::Clear,
        |value| proto::OptionalCameraRecordingModeUpdate { value: Some(value) },
    )
}

fn to_event_stream_update(
    input: OptionalUpdateInput<String>,
) -> Result<proto::OptionalEventRecordingStreamUpdate, ToolError> {
    map_optional_update(
        input,
        "event_recording_stream update",
        |raw| parse_event_stream(&raw),
        proto::optional_event_recording_stream_update::Value::Set,
        proto::optional_event_recording_stream_update::Value::Clear,
        |value| proto::OptionalEventRecordingStreamUpdate { value: Some(value) },
    )
}

fn parse_backend(raw: &str) -> Result<i32, ToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "auto" => Ok(proto::CameraBackend::Auto as i32),
        "retina" => Ok(proto::CameraBackend::Retina as i32),
        "reo_proto" | "reo-proto" => Ok(proto::CameraBackend::ReoProto as i32),
        _ => Err(ToolError::invalid_params(format!(
            "unsupported camera backend: {raw}"
        ))),
    }
}

fn parse_transport(raw: &str) -> Result<i32, ToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "tcp" => Ok(proto::CameraTransport::Tcp as i32),
        "udp" => Ok(proto::CameraTransport::Udp as i32),
        _ => Err(ToolError::invalid_params(format!(
            "unsupported camera transport: {raw}"
        ))),
    }
}

fn parse_recording_mode(raw: &str) -> Result<i32, ToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "sub" => Ok(proto::CameraRecordingMode::Sub as i32),
        "main" => Ok(proto::CameraRecordingMode::Main as i32),
        "both" => Ok(proto::CameraRecordingMode::Both as i32),
        "event_boost" | "event-boost" => Ok(proto::CameraRecordingMode::EventBoost as i32),
        "off" => Ok(proto::CameraRecordingMode::Off as i32),
        "event_only" | "event-only" => Ok(proto::CameraRecordingMode::EventOnly as i32),
        _ => Err(ToolError::invalid_params(format!(
            "unsupported camera recording_mode: {raw}"
        ))),
    }
}

fn parse_event_stream(raw: &str) -> Result<i32, ToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "sub" => Ok(proto::EventRecordingStream::Sub as i32),
        "main" => Ok(proto::EventRecordingStream::Main as i32),
        _ => Err(ToolError::invalid_params(format!(
            "unsupported event_recording_stream: {raw}"
        ))),
    }
}

fn extract_configuration_snapshot(
    result: proto::ok::Result,
) -> Result<proto::ConfigurationSnapshot, ToolError> {
    match result {
        proto::ok::Result::ConfigurationResult(proto::ConfigurationResult {
            result: Some(proto::configuration_result::Result::Snapshot(snapshot)),
        }) => Ok(snapshot),
        _ => Err(ToolError::internal(
            "unexpected configuration snapshot response",
        )),
    }
}

fn extract_configuration_plan(
    result: proto::ok::Result,
) -> Result<proto::ConfigurationPlan, ToolError> {
    match result {
        proto::ok::Result::ConfigurationResult(proto::ConfigurationResult {
            result: Some(proto::configuration_result::Result::Plan(plan)),
        }) => Ok(plan),
        _ => Err(ToolError::internal(
            "unexpected configuration plan response",
        )),
    }
}

fn extract_configuration_applied(
    result: proto::ok::Result,
) -> Result<proto::ConfigurationApplyResult, ToolError> {
    match result {
        proto::ok::Result::ConfigurationResult(proto::ConfigurationResult {
            result: Some(proto::configuration_result::Result::Applied(applied)),
        }) => Ok(applied),
        _ => Err(ToolError::internal(
            "unexpected configuration apply response",
        )),
    }
}

fn snapshot_to_json(snapshot: &proto::ConfigurationSnapshot) -> Value {
    json!({
        "contract_version": snapshot.contract_version,
        "configuration_revision": snapshot.configuration_revision,
        "total_camera_count": snapshot.total_camera_count,
        "next_page_token": snapshot.next_page_token,
        "defaults": snapshot.defaults.as_ref().map(|defaults| json!({
            "username_configured": defaults.username_configured,
            "password_configured": defaults.password_configured,
            "effective_backend": backend_label(defaults.effective_backend),
            "effective_transport": transport_label(defaults.effective_transport),
            "effective_record_generic_motion_events": defaults.effective_record_generic_motion_events,
            "effective_recording_mode": recording_mode_label(defaults.effective_recording_mode),
            "effective_event_recording_duration_secs": defaults.effective_event_recording_duration_secs,
            "effective_event_pre_recording_duration_secs": defaults.effective_event_pre_recording_duration_secs,
            "effective_event_recording_stream": event_stream_label(defaults.effective_event_recording_stream),
        })),
        "cameras": snapshot.cameras.iter().map(camera_effective_to_json).collect::<Vec<_>>(),
        "templates": snapshot.templates.iter().map(template_to_json).collect::<Vec<_>>(),
        "domains": snapshot.domains.iter().map(|domain| json!({
            "domain_id": domain.domain_id,
            "label": domain.label,
            "owner_path": domain.owner_path,
            "readable": domain.readable,
            "mutable": domain.mutable,
            "unavailable_reason": domain.unavailable_reason,
        })).collect::<Vec<_>>(),
    })
}

fn camera_effective_to_json(camera: &proto::CameraEffectiveConfiguration) -> Value {
    json!({
        "camera": camera.camera.as_ref().map(|settings| json!({
            "id": settings.id,
            "ip": settings.ip,
            "display_name": settings.display_name,
            "manufacturer_override": settings.manufacturer_override,
            "username_configured": settings.username_configured,
            "password_configured": settings.password_configured,
            "onvif_port": settings.onvif_port,
            "http_port": settings.http_port,
            "main_rtsp_url": settings.main_rtsp_url,
            "sub_rtsp_url": settings.sub_rtsp_url,
            "uid_configured": settings.uid_configured,
            "backend": settings.backend,
            "transport": settings.transport,
            "recording_mode": settings.recording_mode,
        })),
        "group_ids": camera.group_ids,
        "backend": camera.backend.as_ref().map(|v| backend_label(v.effective)),
        "transport": camera.transport.as_ref().map(|v| transport_label(v.effective)),
        "recording_mode": camera.recording_mode.as_ref().map(|v| recording_mode_label(v.effective)),
    })
}

fn template_to_json(template: &proto::ConfigurationTemplate) -> Value {
    json!({
        "template_id": template.template_id,
        "version": template.version,
        "name": template.name,
        "description": template.description,
        "created_at_ms": template.created_at_ms,
        "updated_at_ms": template.updated_at_ms,
        "values": template.values.as_ref().map(|values| json!({
            "username_secret_reference": values.username_secret_reference,
            "password_secret_reference": values.password_secret_reference,
            "onvif_port": values.onvif_port,
            "http_port": values.http_port,
            "backend": values.backend.map(backend_label),
            "transport": values.transport.map(transport_label),
            "record_generic_motion_events": values.record_generic_motion_events,
            "recording_mode": values.recording_mode.map(recording_mode_label),
            "event_recording_duration_secs": values.event_recording_duration_secs,
            "event_pre_recording_duration_secs": values.event_pre_recording_duration_secs,
            "event_recording_stream": values.event_recording_stream.map(event_stream_label),
        })),
    })
}

fn plan_to_json(plan: &proto::ConfigurationPlan) -> Value {
    json!({
        "plan_id": plan.plan_id,
        "configuration_revision": plan.configuration_revision,
        "expires_at_ms": plan.expires_at_ms,
        "authoritative_target_count": plan.authoritative_target_count,
        "valid": plan.valid,
        "impact": impact_label(plan.impact),
        "apply_semantics": plan.apply_semantics,
        "requires_administrator_confirmation": plan.requires_administrator_confirmation,
        "targets": plan.targets.iter().map(|target| json!({
            "camera_id": target.camera_id,
            "display_name": target.display_name,
            "group_ids": target.group_ids,
            "skipped": target.skipped,
            "skip_reason": target.skip_reason,
        })).collect::<Vec<_>>(),
        "changes": plan.changes.iter().map(|change| json!({
            "camera_id": change.camera_id,
            "field": change.field,
            "old_configured_value": change.old_configured_value,
            "old_effective_value": change.old_effective_value,
            "new_configured_value": change.new_configured_value,
            "new_effective_value": change.new_effective_value,
            "secret": change.secret,
        })).collect::<Vec<_>>(),
        "issues": plan.issues.iter().map(|issue| json!({
            "camera_id": issue.camera_id,
            "field": issue.field,
            "severity": issue_severity_label(issue.severity),
            "code": issue.code,
            "message": issue.message,
        })).collect::<Vec<_>>(),
    })
}

fn current_configuration_revision(ctx: &McpContext<'_>) -> Option<String> {
    let command = proto::ConfigurationCommand {
        action: Some(proto::configuration_command::Action::Get(
            proto::GetConfigurationSnapshot {
                page_size: Some(1),
                page_token: String::new(),
            },
        )),
    };
    let result = configuration::dispatch_as(
        ctx.state,
        SessionId::from_u64(0),
        &ctx.identity.principal,
        command,
    )
    .ok()?;
    extract_configuration_snapshot(result)
        .ok()
        .map(|snapshot| snapshot.configuration_revision)
}

fn applied_to_json(applied: &proto::ConfigurationApplyResult, fallback_revision: &str) -> Value {
    let revision = applied
        .snapshot
        .as_ref()
        .map_or(fallback_revision, |s| s.configuration_revision.as_str());
    json!({
        "plan_id": applied.plan_id,
        "configuration_committed": applied.configuration_committed,
        "impact": impact_label(applied.impact),
        "configuration_revision": revision,
        "activations": applied.activations.iter().map(|item| json!({
            "camera_id": item.camera_id,
            "status": activation_status_label(item.status),
            "detail": item.detail,
        })).collect::<Vec<_>>(),
    })
}

fn runtime_config_result_to_json(result: &proto::RuntimeConfigurationResult) -> Value {
    json!({
        "restart_required": result.restart_required,
        "config": result.config.as_ref().map(|cfg| json!({
            "host": cfg.host,
            "port": cfg.port,
            "configuration_revision": cfg.configuration_revision,
            "storage": cfg.storage.as_ref().map(|storage| json!({
                "medium_term_path": storage.medium_term_path,
                "long_term_path": storage.long_term_path,
                "recording_catalog_path": storage.recording_catalog_path,
                "event_thumbnail_path": storage.event_thumbnail_path,
                "event_thumbnail_max_mb": storage.event_thumbnail_max_mb,
                "short_term_secs": storage.short_term_secs,
                "medium_term_secs": storage.medium_term_secs,
                "flush_interval_secs": storage.flush_interval_secs,
                "write_buffer_bytes": storage.write_buffer_bytes,
                "long_term_max_gb": storage.long_term_max_gb,
                "minimum_free_gb": storage.minimum_free_gb,
                "maximum_used_percent": storage.maximum_used_percent,
                "warning_free_gb": storage.warning_free_gb,
                "critical_free_gb": storage.critical_free_gb,
                "cleanup_hysteresis_gb": storage.cleanup_hysteresis_gb,
            })),
        })),
    })
}

fn prost_struct_to_json(value: &Struct) -> Value {
    let mut map = Map::new();
    for (key, entry) in &value.fields {
        map.insert(key.clone(), prost_value_to_json(entry));
    }
    Value::Object(map)
}

fn prost_value_to_json(value: &ProstValue) -> Value {
    match value.kind.as_ref() {
        Some(Kind::NullValue(_)) | None => Value::Null,
        Some(Kind::NumberValue(number)) => {
            serde_json::Number::from_f64(*number).map_or(Value::Null, Value::Number)
        }
        Some(Kind::StringValue(text)) => Value::String(text.clone()),
        Some(Kind::BoolValue(flag)) => Value::Bool(*flag),
        Some(Kind::StructValue(object)) => prost_struct_to_json(object),
        Some(Kind::ListValue(list)) => {
            Value::Array(list.values.iter().map(prost_value_to_json).collect())
        }
    }
}

fn backend_label(value: i32) -> &'static str {
    match proto::CameraBackend::try_from(value) {
        Ok(proto::CameraBackend::Auto) => "auto",
        Ok(proto::CameraBackend::Retina) => "retina",
        Ok(proto::CameraBackend::ReoProto) => "reo_proto",
        _ => "unspecified",
    }
}

fn transport_label(value: i32) -> &'static str {
    match proto::CameraTransport::try_from(value) {
        Ok(proto::CameraTransport::Tcp) => "tcp",
        Ok(proto::CameraTransport::Udp) => "udp",
        _ => "unspecified",
    }
}

fn recording_mode_label(value: i32) -> &'static str {
    match proto::CameraRecordingMode::try_from(value) {
        Ok(proto::CameraRecordingMode::Sub) => "sub",
        Ok(proto::CameraRecordingMode::Main) => "main",
        Ok(proto::CameraRecordingMode::Both) => "both",
        Ok(proto::CameraRecordingMode::EventBoost) => "event_boost",
        Ok(proto::CameraRecordingMode::Off) => "off",
        Ok(proto::CameraRecordingMode::EventOnly) => "event_only",
        _ => "unspecified",
    }
}

fn event_stream_label(value: i32) -> &'static str {
    match proto::EventRecordingStream::try_from(value) {
        Ok(proto::EventRecordingStream::Sub) => "sub",
        Ok(proto::EventRecordingStream::Main) => "main",
        _ => "unspecified",
    }
}

fn impact_label(value: i32) -> &'static str {
    match proto::ConfigurationImpact::try_from(value) {
        Ok(proto::ConfigurationImpact::Immediate) => "immediate",
        Ok(proto::ConfigurationImpact::ReconnectCamera) => "reconnect_camera",
        Ok(proto::ConfigurationImpact::RestartComponent) => "restart_component",
        Ok(proto::ConfigurationImpact::RestartServer) => "restart_server",
        _ => "unspecified",
    }
}

fn issue_severity_label(value: i32) -> &'static str {
    match proto::ConfigurationIssueSeverity::try_from(value) {
        Ok(proto::ConfigurationIssueSeverity::Info) => "info",
        Ok(proto::ConfigurationIssueSeverity::Warning) => "warning",
        Ok(proto::ConfigurationIssueSeverity::Error) => "error",
        _ => "unspecified",
    }
}

fn activation_status_label(value: i32) -> &'static str {
    match proto::ConfigurationActivationStatus::try_from(value) {
        Ok(proto::ConfigurationActivationStatus::Applied) => "applied",
        Ok(proto::ConfigurationActivationStatus::ReconnectRequired) => "reconnect_required",
        Ok(proto::ConfigurationActivationStatus::RestartRequired) => "restart_required",
        Ok(proto::ConfigurationActivationStatus::Failed) => "failed",
        _ => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        access::{AccessManager, CameraAccess, IssuedCredential},
        cameras::CameraConfig,
        logging::{LogFilterFile, LoggingService},
        runtime::Router,
    };
    use std::{collections::HashMap, path::PathBuf};

    fn mcp_post_request(body: &Value, headers: Vec<(String, String)>, remote: &str) -> Request {
        let mut all_headers = vec![
            ("Host".to_owned(), "127.0.0.1:8081".to_owned()),
            ("Content-Type".to_owned(), "application/json".to_owned()),
        ];
        all_headers.extend(headers);
        let bytes = serde_json::to_vec(body).unwrap();
        Request::fake_http_from(remote.parse().unwrap(), "POST", "/mcp", all_headers, bytes)
    }

    fn read_response_json(response: Response) -> Value {
        let (mut reader, _) = response.data.into_reader_and_size();
        let mut body = String::new();
        reader.read_to_string(&mut body).unwrap();
        serde_json::from_str(&body).unwrap()
    }

    fn state_with_mcp(mcp_config: McpConfig) -> (ServerState, FacadeSender<RouterMessage>) {
        let (_router, router_tx) = Router::new().unwrap();
        let state = ServerState::empty().with_mcp_config(mcp_config);
        (state, router_tx)
    }

    fn temp_config_fixture(
        mcp_config: McpConfig,
        secret_sentinel: &str,
    ) -> (PathBuf, ServerState, FacadeSender<RouterMessage>) {
        let dir =
            std::env::temp_dir().join(format!("keeppeek-mcp-http-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        let secrets_path = dir.join("secrets.toml");
        std::fs::write(
            &secrets_path,
            format!("CAMERA_PASSWORD = \"{secret_sentinel}\"\nKEEPPEEK_ACCESS_KEY = \"550e8400-e29b-41d4-a716-446655440000\"\n"),
        )
        .unwrap();
        std::fs::write(
            &config_path,
            r#"
host = "127.0.0.1"
port = 8081
access_key = "{secret:KEEPPEEK_ACCESS_KEY}"

[mcp]
enabled = true

[camera_defaults]
username = "admin"
password = "{secret:CAMERA_PASSWORD}"

[front.door]
ip = "192.0.2.10"
display_name = "Front Door"
"#,
        )
        .unwrap();
        let cfg = config::load_config(&config_path).unwrap();
        let mut cfg = cfg;
        cfg.mcp = mcp_config;
        let cameras = cfg.camera_configurations().unwrap();
        let storage = crate::storage::StorageConfig {
            long_term_path: dir.join("recordings"),
            ..crate::storage::StorageConfig::default()
        };
        static TEST_CATALOG: std::sync::OnceLock<Arc<crate::camera_database::CameraDatabase>> =
            std::sync::OnceLock::new();
        let catalog = TEST_CATALOG
            .get_or_init(|| {
                Arc::new(
                    crate::camera_database::CameraDatabase::load_embedded()
                        .expect("load embedded camera catalog"),
                )
            })
            .clone();
        let state = ServerState::new(
            &cfg,
            &cameras,
            &HashMap::new(),
            &storage,
            crate::storage::RecordingDemand::new(Duration::from_secs(5)),
            crate::webrtc::WebRtc::new(),
        )
        .with_camera_config_path(config_path)
        .with_camera_database(catalog);
        let (_router, router_tx) = Router::new().unwrap();
        (dir, state, router_tx)
    }

    #[test]
    fn mcp_is_disabled_by_default_and_returns_404() {
        let (state, router_tx) = state_with_mcp(McpConfig::default());
        let post = mcp_post_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            Vec::new(),
            "127.0.0.1:50000",
        );
        assert_eq!(
            super::super::handle_request(&post, &router_tx, &state).status_code,
            404
        );

        let get = Request::fake_http(
            "GET",
            "/mcp",
            vec![("Host".into(), "127.0.0.1:8081".into())],
            Vec::new(),
        );
        assert_eq!(
            super::super::handle_request(&get, &router_tx, &state).status_code,
            404
        );
    }

    #[test]
    fn enforces_host_origin_method_and_query_credential_rules() {
        let (state, router_tx) = state_with_mcp(McpConfig {
            enabled: true,
            ..McpConfig::default()
        });
        let get = Request::fake_http(
            "GET",
            "/mcp",
            vec![("Host".into(), "127.0.0.1:8081".into())],
            Vec::new(),
        );
        assert_eq!(
            super::super::handle_request(&get, &router_tx, &state).status_code,
            405
        );

        let bad_origin = mcp_post_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            vec![("Origin".into(), "https://evil.example".into())],
            "127.0.0.1:50000",
        );
        assert_eq!(
            super::super::handle_request(&bad_origin, &router_tx, &state).status_code,
            403
        );

        // DNS-rebinding attack: attacker controls evil.example resolving to 127.0.0.1.
        let dns_rebind_with_origin = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "POST",
            "/mcp",
            vec![
                ("Host".into(), "evil.example:8081".into()),
                ("Origin".into(), "http://evil.example:8081".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec(),
        );
        assert_eq!(
            super::super::handle_request(&dns_rebind_with_origin, &router_tx, &state).status_code,
            403
        );

        let dns_rebind_host_only = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "POST",
            "/mcp",
            vec![
                ("Host".into(), "evil.example:8081".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec(),
        );
        assert_eq!(
            super::super::handle_request(&dns_rebind_host_only, &router_tx, &state).status_code,
            403
        );

        let unallowlisted_origin = mcp_post_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            vec![("Origin".into(), "http://127.0.0.1:8081".into())],
            "127.0.0.1:50000",
        );
        assert_eq!(
            super::super::handle_request(&unallowlisted_origin, &router_tx, &state).status_code,
            403
        );

        let mut allowlisted_state = state.clone();
        allowlisted_state.allowed_origins =
            Arc::new(HashSet::from(["http://127.0.0.1:8081".to_owned()]));
        let issued = allowlisted_state
            .access_manager
            .create_credential(
                "browser-admin",
                None,
                AccessRole::Administrator,
                None,
                1_000,
            )
            .unwrap();
        // Allowlisted Origin without Bearer token must be rejected even from loopback.
        let allowlisted_unauth = mcp_post_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            vec![("Origin".into(), "http://127.0.0.1:8081".into())],
            "127.0.0.1:50000",
        );
        assert_eq!(
            super::super::handle_request(&allowlisted_unauth, &router_tx, &allowlisted_state)
                .status_code,
            401
        );

        let allowlisted_with_bearer = mcp_post_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            vec![
                ("Origin".into(), "http://127.0.0.1:8081".into()),
                (
                    "Authorization".into(),
                    format!("Bearer {}", issued.access_key.canonical()),
                ),
            ],
            "127.0.0.1:50000",
        );
        assert_eq!(
            super::super::handle_request(&allowlisted_with_bearer, &router_tx, &allowlisted_state)
                .status_code,
            200
        );

        let localhost_no_origin = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "POST",
            "/mcp",
            vec![
                ("Host".into(), "localhost:8081".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec(),
        );
        assert_eq!(
            super::super::handle_request(&localhost_no_origin, &router_tx, &state).status_code,
            200
        );

        let query_token = Request::fake_http(
            "POST",
            "/mcp?token=550e8400-e29b-41d4-a716-446655440000",
            vec![
                ("Host".into(), "127.0.0.1:8081".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec(),
        );
        assert_eq!(
            super::super::handle_request(&query_token, &router_tx, &state).status_code,
            400
        );
    }

    #[test]
    fn enforces_write_gate_read_only_header_and_concurrency_cap() {
        let filter_dir =
            std::env::temp_dir().join(format!("keeppeek-mcp-log-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&filter_dir).unwrap();
        let (logging_service, _dispatch) =
            LoggingService::for_test(LogFilterFile::new(filter_dir.join("log-filter")), "info");

        let (state_ro, router_tx) = state_with_mcp(McpConfig {
            enabled: true,
            allow_writes: false,
            max_concurrent_requests: 1,
            ..McpConfig::default()
        });
        let state_ro = state_ro.with_logging(logging_service.clone());
        let write_call = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "set_logging_filter", "arguments": { "filter": "debug" } }
        });
        let resp = super::super::handle_request(
            &mcp_post_request(&write_call, Vec::new(), "127.0.0.1:50000"),
            &router_tx,
            &state_ro,
        );
        let body = read_response_json(resp);
        assert_eq!(body["error"]["code"], ERROR_FORBIDDEN);

        let _held_guard = state_ro.mcp.try_acquire().unwrap();
        let busy_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state_ro,
        );
        assert_eq!(busy_resp.status_code, 429);
        assert_eq!(read_response_json(busy_resp)["error"]["code"], ERROR_BUSY);
        drop(_held_guard);

        let (state_rw, router_tx) = state_with_mcp(McpConfig {
            enabled: true,
            allow_writes: true,
            ..McpConfig::default()
        });
        let state_rw = state_rw.with_logging(logging_service);
        let header_blocked = super::super::handle_request(
            &mcp_post_request(
                &write_call,
                vec![(READ_ONLY_HEADER.to_owned(), "true".to_owned())],
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state_rw,
        );
        assert_eq!(
            read_response_json(header_blocked)["error"]["code"],
            ERROR_FORBIDDEN
        );

        let allowed = super::super::handle_request(
            &mcp_post_request(&write_call, Vec::new(), "127.0.0.1:50000"),
            &router_tx,
            &state_rw,
        );
        assert_eq!(allowed.status_code, 200);
        assert_eq!(
            read_response_json(allowed)["result"]["structuredContent"]["active_filter"],
            "debug"
        );
        let _ = std::fs::remove_dir_all(&filter_dir);
    }

    #[test]
    fn enforces_user_rbac_and_filters_health_cameras() {
        let mut cfg = config::Config::default();
        cfg.access.require_secure_remote = false;
        cfg.mcp = McpConfig {
            enabled: true,
            allow_writes: true,
            ..McpConfig::default()
        };
        let mut cameras = HashMap::new();
        cameras.insert(
            "site".to_owned(),
            vec![
                toml::from_str::<CameraConfig>("ip = \"192.0.2.10\"\nname = \"Allowed\"\n")
                    .unwrap(),
                toml::from_str::<CameraConfig>("ip = \"192.0.2.11\"\nname = \"Restricted\"\n")
                    .unwrap(),
            ],
        );
        let access_manager = AccessManager::ephemeral(cfg.access_key);
        let IssuedCredential {
            metadata,
            access_key,
        } = access_manager
            .create_credential("viewer", None, AccessRole::User, None, 1_000)
            .unwrap();
        access_manager
            .set_camera_access(
                metadata.id,
                metadata.revision,
                CameraAccess {
                    all_cameras: false,
                    group_ids: Vec::new(),
                    camera_ids: vec!["192.0.2.10".to_owned()],
                },
            )
            .unwrap();
        let state = ServerState::new(
            &cfg,
            &cameras,
            &HashMap::new(),
            &crate::storage::StorageConfig::default(),
            crate::storage::RecordingDemand::new(Duration::from_secs(5)),
            crate::webrtc::WebRtc::new(),
        )
        .with_access_manager(access_manager);
        let (_router, router_tx) = Router::new().unwrap();
        let auth = vec![(
            "Authorization".to_owned(),
            format!("Bearer {}", access_key.canonical()),
        )];

        let list_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
                auth.clone(),
                "198.51.100.42:50000",
            ),
            &router_tx,
            &state,
        );
        let list_json = read_response_json(list_resp);
        let names = list_json["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "get_server_health",
                "search_events",
                "get_recording_coverage",
                "get_camera_snapshot",
            ]
        );

        let health_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_server_health","arguments":{}}}),
                auth.clone(),
                "198.51.100.42:50000",
            ),
            &router_tx,
            &state,
        );
        let health_json = read_response_json(health_resp);
        let cameras = health_json["result"]["structuredContent"]["cameras"]
            .as_array()
            .unwrap();
        assert_eq!(cameras.len(), 1);
        assert_eq!(cameras[0]["id"], "192.0.2.10");

        let denied = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_secret_keys","arguments":{}}}),
                auth,
                "198.51.100.42:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(read_response_json(denied)["error"]["code"], ERROR_FORBIDDEN);
    }

    #[test]
    fn config_plan_apply_and_secret_redaction_work_end_to_end() {
        let sentinel = "ULTRA-SECRET-SENTINEL-987654";
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: false,
                ..McpConfig::default()
            },
            sentinel,
        );

        let keys_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_secret_keys","arguments":{}}}),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let keys_json = read_response_json(keys_resp);
        let serialized_keys = serde_json::to_string(&keys_json).unwrap();
        assert!(!serialized_keys.contains(sentinel));
        assert_eq!(
            keys_json["result"]["structuredContent"]["keys"],
            json!(["CAMERA_PASSWORD", "KEEPPEEK_ACCESS_KEY"])
        );

        let cfg_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_configuration","arguments":{}}}),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let cfg_json = read_response_json(cfg_resp);
        assert!(!serde_json::to_string(&cfg_json).unwrap().contains(sentinel));
        let revision = cfg_json["result"]["structuredContent"]["configuration_revision"]
            .as_str()
            .unwrap()
            .to_owned();

        let plan_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "plan_configuration_change",
                        "arguments": {
                            "expected_configuration_revision": revision,
                            "targets": { "camera_ids": ["192.0.2.10"] },
                            "change": {
                                "patch": {
                                    "display_name": "Front Porch"
                                }
                            }
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let plan_json = read_response_json(plan_resp);
        assert!(
            !serde_json::to_string(&plan_json)
                .unwrap()
                .contains(sentinel)
        );
        assert_eq!(plan_json["result"]["structuredContent"]["valid"], true);
        let plan_id = plan_json["result"]["structuredContent"]["plan_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let apply_denied = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "tools/call",
                    "params": {
                        "name": "apply_configuration_plan",
                        "arguments": {
                            "plan_id": plan_id,
                            "expected_configuration_revision": revision
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(apply_denied)["error"]["code"],
            ERROR_FORBIDDEN
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_tools_apply_plan_templates_logs_runtime_and_record_audit() {
        let sentinel = "SECOND-SENTINEL-VALUE-12345";
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: true,
                ..McpConfig::default()
            },
            sentinel,
        );
        let (logging_service, dispatch) =
            LoggingService::for_test(LogFilterFile::new(dir.join("log-filter")), "trace");
        logging_service.redact_secret(sentinel);
        let state = state.with_logging(logging_service);

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::warn!(target: "keeppeek::camera", camera = "192.0.2.10", "camera auth {sentinel}");
            tracing::info!(target: "keeppeek::other", "unrelated info");
        });

        let log_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/call",
                    "params": {
                        "name": "search_logs",
                        "arguments": { "level": "warn", "camera": "192.0.2.10", "limit": 10 }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let log_json = read_response_json(log_resp);
        assert!(!serde_json::to_string(&log_json).unwrap().contains(sentinel));
        assert_eq!(log_json["result"]["structuredContent"]["returned_count"], 1);

        let cfg_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"get_configuration","arguments":{}}}),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let revision =
            read_response_json(cfg_resp)["result"]["structuredContent"]["configuration_revision"]
                .as_str()
                .unwrap()
                .to_owned();

        let plan_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 12,
                    "method": "tools/call",
                    "params": {
                        "name": "plan_configuration_change",
                        "arguments": {
                            "expected_configuration_revision": revision,
                            "targets": { "camera_ids": ["192.0.2.10"] },
                            "change": { "patch": { "display_name": "Updated Front" } }
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let plan_id = read_response_json(plan_resp)["result"]["structuredContent"]["plan_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let apply_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 13,
                    "method": "tools/call",
                    "params": {
                        "name": "apply_configuration_plan",
                        "arguments": {
                            "plan_id": plan_id,
                            "expected_configuration_revision": revision
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let apply_json = read_response_json(apply_resp);
        assert_eq!(
            apply_json["result"]["structuredContent"]["configuration_committed"],
            true
        );
        let next_revision = apply_json["result"]["structuredContent"]["configuration_revision"]
            .as_str()
            .unwrap()
            .to_owned();

        let save_tpl = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 14,
                    "method": "tools/call",
                    "params": {
                        "name": "manage_configuration_template",
                        "arguments": {
                            "action": "save",
                            "expected_configuration_revision": next_revision,
                            "template": {
                                "name": "Outdoor Default",
                                "description": "TCP retina preset",
                                "values": { "transport": "tcp", "recording_mode": "sub" }
                            }
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let save_json = read_response_json(save_tpl);
        assert!(save_json["result"]["structuredContent"]["template_id"].is_string());

        let missing_rev_host = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 15,
                    "method": "tools/call",
                    "params": {
                        "name": "update_runtime_configuration",
                        "arguments": { "host": "127.0.0.1" }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(missing_rev_host)["error"]["code"],
            ERROR_INVALID_PARAMS
        );

        let audits = state.access_manager.list_audit(64);
        assert!(audits.iter().any(
            |event| event.action == "mcp_apply_configuration_plan" && event.result == "success"
        ));
        assert!(
            audits
                .iter()
                .any(|event| event.action == "mcp_manage_configuration_template"
                    && event.result == "success")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn response_byte_budget_rejects_oversized_responses() {
        let (state, router_tx) = state_with_mcp(McpConfig {
            enabled: true,
            max_response_bytes: crate::config::mcp::MIN_MAX_RESPONSE_BYTES,
            ..McpConfig::default()
        });
        let filter_dir =
            std::env::temp_dir().join(format!("keeppeek-mcp-budget-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&filter_dir).unwrap();
        let (logging_service, dispatch) =
            LoggingService::for_test(LogFilterFile::new(filter_dir.join("log-filter")), "trace");
        let state = state.with_logging(logging_service);
        let payload = "x".repeat(200);
        tracing::dispatcher::with_default(&dispatch, || {
            for index in 0..40 {
                tracing::info!(target: "keeppeek::budget", "entry {index} {payload}");
            }
        });
        let resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": "tools/call",
                    "params": { "name": "search_logs", "arguments": { "limit": 40 } }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(resp.status_code, 200);
        let body = read_response_json(resp);
        assert_eq!(body["error"]["code"], ERROR_RESPONSE_TOO_LARGE);
        let _ = std::fs::remove_dir_all(&filter_dir);
    }

    #[test]
    fn stdio_bridge_e2e_lists_21_tools_calls_diagnostics_and_handles_errors() {
        let sentinel = "E2E-STDIO-SENTINEL-778899";
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: false,
                allow_snapshots: false,
                ..McpConfig::default()
            },
            sentinel,
        );
        let server = rouille::Server::new("127.0.0.1:0", move |request| {
            super::super::handle_request(request, &router_tx, &state)
        })
        .unwrap();
        let addr = server.server_addr();
        let (handle, shutdown) = server.stoppable();

        let client = crate::mcp::stdio::McpHttpClient::new(
            &format!("http://{addr}"),
            Some("550e8400-e29b-41d4-a716-446655440000".to_owned()),
            false,
        )
        .unwrap();
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"get_runtime_configuration\",\"arguments\":{}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"list_runtime_profiles\",\"arguments\":{}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"get_storage_status\",\"arguments\":{}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"tools/call\",\"params\":{\"name\":\"get_notification_status\",\"arguments\":{}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"get_camera_snapshot\",\"arguments\":{\"camera_id\":\"192.0.2.10\"}}}\n"
        );
        let mut stdout = Vec::new();
        crate::mcp::stdio::serve_stdio_loop(
            std::io::Cursor::new(input.as_bytes()),
            &mut stdout,
            |line| client.forward(line),
        )
        .unwrap();
        shutdown.send(()).ok();
        handle.join().ok();

        let output_text = String::from_utf8(stdout).unwrap();
        assert!(!output_text.contains(sentinel));
        let lines: Vec<Value> = output_text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 7);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[0]["result"]["protocolVersion"], "2025-03-26");

        let tools = lines[1]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 29);
        let tool_names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        for expected in [
            "get_server_health",
            "search_events",
            "get_recording_coverage",
            "get_camera_snapshot",
            "search_logs",
            "get_logging_settings",
            "get_configuration",
            "list_secret_keys",
            "plan_configuration_change",
            "get_storage_status",
            "get_notification_status",
            "discover_cameras",
            "get_runtime_configuration",
            "list_runtime_profiles",
            "search_camera_catalog",
            "probe_camera_streams",
            "request_secret_entry",
            "get_secret_entry_status",
            "get_camera_device_settings",
            "update_camera_device_settings",
            "camera_debugging",
            "apply_configuration_plan",
            "update_runtime_configuration",
            "manage_configuration_template",
            "manage_camera",
            "manage_notification_rule",
            "set_runtime_profile",
            "set_logging_filter",
            "disable_mcp_access",
        ] {
            assert!(
                tool_names.contains(&expected),
                "missing MCP tool in tools/list: {expected}"
            );
        }

        assert!(lines[2]["result"]["structuredContent"]["host"].is_string());
        assert!(lines[3]["result"]["structuredContent"].is_object());
        assert!(lines[4]["result"]["structuredContent"]["configuration"].is_object());
        assert!(lines[5]["result"]["structuredContent"]["rules"].is_object());
        assert_eq!(lines[6]["error"]["code"], ERROR_FORBIDDEN);

        // Verify unreachable server yields a clean JSON-RPC error frame without panicking.
        let closed_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let unused_addr = closed_listener.local_addr().unwrap();
        drop(closed_listener);
        let dead_client =
            crate::mcp::stdio::McpHttpClient::new(&format!("http://{unused_addr}"), None, true)
                .unwrap();
        let mut err_out = Vec::new();
        crate::mcp::stdio::serve_stdio_loop(
            std::io::Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"ping\"}\n"),
            &mut err_out,
            |line| dead_client.forward(line),
        )
        .unwrap();
        let err_json: Value =
            serde_json::from_str(std::str::from_utf8(&err_out).unwrap().trim()).unwrap();
        assert_eq!(err_json["id"], 99);
        assert_eq!(err_json["error"]["code"], ERROR_INTERNAL);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manage_camera_enforces_secret_references_and_upserts_and_removes() {
        let sentinel = "MANAGE-CAM-SENTINEL-884422";
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: true,
                ..McpConfig::default()
            },
            sentinel,
        );
        let secrets_path = dir.join("secrets.toml");
        std::fs::write(
            &secrets_path,
            format!(
                "CAMERA_USER = \"camadmin\"\nCAMERA_PASSWORD = \"{sentinel}\"\nKEEPPEEK_ACCESS_KEY = \"550e8400-e29b-41d4-a716-446655440000\"\n"
            ),
        )
        .unwrap();
        let cfg_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_configuration","arguments":{}}}),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let revision =
            read_response_json(cfg_resp)["result"]["structuredContent"]["configuration_revision"]
                .as_str()
                .unwrap()
                .to_owned();

        // Plaintext credentials and raw RTSP passwords must be rejected.
        for bad_args in [
            json!({
                "action": "upsert",
                "ip": "192.0.2.50",
                "expected_configuration_revision": revision,
                "password": "plaintext-password"
            }),
            json!({
                "action": "upsert",
                "ip": "192.0.2.50",
                "expected_configuration_revision": revision,
                "username": "plaintext-user"
            }),
            json!({
                "action": "upsert",
                "ip": "192.0.2.50",
                "expected_configuration_revision": revision,
                "uid": "raw-camera-uid"
            }),
            json!({
                "action": "upsert",
                "ip": "192.0.2.50",
                "expected_configuration_revision": revision,
                "main_rtsp_url": "rtsp://admin:hunter2@192.0.2.50/main"
            }),
            json!({
                "action": "upsert",
                "ip": "192.0.2.50",
                "expected_configuration_revision": revision,
                "sub_rtsp_url": "rtsp://192.0.2.50:554/sub?user=admin&password=hunter2"
            }),
        ] {
            let rejected = super::super::handle_request(
                &mcp_post_request(
                    &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"manage_camera","arguments": bad_args}}),
                    Vec::new(),
                    "127.0.0.1:50000",
                ),
                &router_tx,
                &state,
            );
            assert_eq!(
                read_response_json(rejected)["error"]["code"],
                ERROR_INVALID_PARAMS
            );
        }

        // plan_configuration_change must also reject plaintext RTSP userinfo and query secrets.
        for bad_url in [
            "rtsp://admin:hunter2@192.0.2.10/main",
            "rtsp://192.0.2.10/main?user=admin&password=hunter2",
        ] {
            let rejected_plan = super::super::handle_request(
                &mcp_post_request(
                    &json!({
                        "jsonrpc": "2.0",
                        "id": 20,
                        "method": "tools/call",
                        "params": {
                            "name": "plan_configuration_change",
                            "arguments": {
                                "expected_configuration_revision": revision,
                                "targets": { "camera_ids": ["192.0.2.10"] },
                                "change": { "patch": { "main_rtsp_url": bad_url } }
                            }
                        }
                    }),
                    Vec::new(),
                    "127.0.0.1:50000",
                ),
                &router_tx,
                &state,
            );
            assert_eq!(
                read_response_json(rejected_plan)["error"]["code"],
                ERROR_INVALID_PARAMS
            );
        }

        // Valid {secret:KEY} upsert succeeds without exposing resolved secret values.
        let added = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "manage_camera",
                        "arguments": {
                            "action": "upsert",
                            "ip": "192.0.2.50",
                            "expected_configuration_revision": revision,
                            "display_name": "Side Gate",
                            "username": "{secret:CAMERA_USER}",
                            "password": "{secret:CAMERA_PASSWORD}",
                            "main_rtsp_url": "rtsp://192.0.2.50:554/main"
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let added_json = read_response_json(added);
        assert!(
            !serde_json::to_string(&added_json)
                .unwrap()
                .contains(sentinel)
        );
        assert_eq!(
            added_json["result"]["structuredContent"]["camera"]["ip"],
            "192.0.2.50"
        );
        let next_revision = added_json["result"]["structuredContent"]["configuration_revision"]
            .as_str()
            .unwrap()
            .to_owned();

        let removed = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "tools/call",
                    "params": {
                        "name": "manage_camera",
                        "arguments": {
                            "action": "remove",
                            "ip": "192.0.2.50",
                            "expected_configuration_revision": next_revision
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let removed_json = read_response_json(removed);
        assert_eq!(
            removed_json["result"]["structuredContent"]["camera_id"],
            "192.0.2.50"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manage_notification_rule_enforces_secret_policy_and_lifecycle() {
        let webhook_secret = "https://hooks.example.com/secret-token-776655";
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: true,
                ..McpConfig::default()
            },
            "NOTIF-CAM-SECRET-1122",
        );
        std::fs::write(
            dir.join("secrets.toml"),
            format!(
                "CAMERA_PASSWORD = \"NOTIF-CAM-SECRET-1122\"\nKEEPPEEK_ACCESS_KEY = \"550e8400-e29b-41d4-a716-446655440000\"\nWEBHOOK_URL = \"{webhook_secret}\"\n"
            ),
        )
        .unwrap();
        let notif_runtime = crate::notifications::Runtime::open(&dir.join("config.toml")).unwrap();
        let state = state.with_notifications(notif_runtime.handle());

        let base_rule = |id: &str, channel: &str, destination: &str| {
            json!({
                "id": id,
                "name": "Front Door Alert",
                "enabled": true,
                "revision": 0,
                "triggers": ["event_created"],
                "schedule": { "timezone": "UTC", "active_windows": [], "quiet_hours": null },
                "enrichment": {
                    "deadline_ms": 5000,
                    "maximum_revisions": 2,
                    "maximum_attempts": 1,
                    "maximum_attachment_bytes": 1048576,
                    "wake_after_deadline": false
                },
                "actions": [{
                    "enabled": true,
                    "channel": channel,
                    "destination": destination,
                    "template": { "title": "Alert", "body": "Event {{event.kind}}" },
                    "attachment": "never",
                    "allow_second_delivery": false
                }],
                "failure": {
                    "maximum_attempts": 3,
                    "maximum_retry_interval_ms": 5000,
                    "expiry_ms": 60000
                }
            })
        };

        // Raw webhook and push destinations must be rejected.
        for (channel, dest) in [
            ("webhook", "https://hooks.example.com/raw-secret"),
            ("push", "{\"app_token\":\"raw\",\"user_key\":\"raw\"}"),
        ] {
            let rejected = super::super::handle_request(
                &mcp_post_request(
                    &json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {
                            "name": "manage_notification_rule",
                            "arguments": {
                                "action": "save_draft",
                                "definition": base_rule("raw-rejected", channel, dest)
                            }
                        }
                    }),
                    Vec::new(),
                    "127.0.0.1:50000",
                ),
                &router_tx,
                &state,
            );
            assert_eq!(
                read_response_json(rejected)["error"]["code"],
                ERROR_INVALID_PARAMS
            );
        }

        // {secret:WEBHOOK_URL} resolves server-side and returns redacted destination_ref.
        let saved = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "manage_notification_rule",
                        "arguments": {
                            "action": "save_draft",
                            "definition": base_rule("front-webhook", "webhook", "{secret:WEBHOOK_URL}")
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let saved_json = read_response_json(saved);
        assert!(
            !serde_json::to_string(&saved_json)
                .unwrap()
                .contains(webhook_secret)
        );
        let draft_rev = saved_json["result"]["structuredContent"]["draft_revision"]
            .as_u64()
            .unwrap();
        assert_eq!(
            saved_json["result"]["structuredContent"]["draft_definition"]["actions"][0]["destination_configured"],
            true
        );

        // Omitting expected_draft_revision (defaults to 0) on an existing rule must conflict.
        let cas_conflict = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 21,
                    "method": "tools/call",
                    "params": {
                        "name": "manage_notification_rule",
                        "arguments": {
                            "action": "save_draft",
                            "definition": base_rule("front-webhook", "webhook", "{secret:WEBHOOK_URL}")
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert!(read_response_json(cas_conflict)["error"].is_object());

        let activated = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "manage_notification_rule",
                        "arguments": {
                            "action": "activate",
                            "rule_id": "front-webhook",
                            "expected_draft_revision": draft_rev
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let active_rev =
            read_response_json(activated)["result"]["structuredContent"]["active_revision"]
                .as_u64()
                .unwrap();

        let listed = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "tools/call",
                    "params": {
                        "name": "manage_notification_rule",
                        "arguments": { "action": "list" }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let listed_json = read_response_json(listed);
        assert_eq!(listed_json["result"]["structuredContent"]["count"], 1);
        assert!(
            !serde_json::to_string(&listed_json)
                .unwrap()
                .contains(webhook_secret)
        );

        let deleted = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 5,
                    "method": "tools/call",
                    "params": {
                        "name": "manage_notification_rule",
                        "arguments": {
                            "action": "delete",
                            "rule_id": "front-webhook",
                            "expected_active_revision": active_rev,
                            "expected_draft_revision": draft_rev
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(deleted)["result"]["structuredContent"]["action"],
            "delete"
        );

        drop(notif_runtime);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filter_health_for_user_prevents_prefix_issue_leaks() {
        use crate::operational_events::{
            OperationalEvent, OperationalEventKey, OperationalEventKind, OperationalEvidence,
            OperationalSeverity,
        };

        let mut cfg = config::Config::default();
        cfg.mcp.enabled = true;
        let mut cameras = HashMap::new();
        cameras.insert(
            "site".to_owned(),
            vec![
                toml::from_str::<CameraConfig>("ip = \"192.0.2.1\"\nname = \"Cam1\"\n").unwrap(),
                toml::from_str::<CameraConfig>("ip = \"192.0.2.10\"\nname = \"Cam10\"\n").unwrap(),
            ],
        );
        let state = ServerState::new(
            &cfg,
            &cameras,
            &HashMap::new(),
            &crate::storage::StorageConfig::default(),
            crate::storage::RecordingDemand::new(Duration::from_secs(5)),
            crate::webrtc::WebRtc::new(),
        );
        let (_router, router_tx) = Router::new().unwrap();
        let mut health = server_health(&router_tx, &state);
        health.operational_events = vec![
            OperationalEvent {
                id: "evt-cam-1".to_owned(),
                key: OperationalEventKey {
                    camera_id: "192.0.2.1".to_owned(),
                    stream_id: Some("main".to_owned()),
                    kind: OperationalEventKind::CameraOffline,
                },
                evidence: OperationalEvidence {
                    cause: "offline".to_owned(),
                    explanation: "cam 1 offline".to_owned(),
                    affected_streams: vec!["main".to_owned()],
                    recording_interrupted: true,
                    source: "test".to_owned(),
                },
                severity: OperationalSeverity::Warning,
                revision: 1,
                start_time_ms: 1_000,
                end_time_ms: None,
                duration_ms: None,
            },
            OperationalEvent {
                id: "evt-cam-10".to_owned(),
                key: OperationalEventKey {
                    camera_id: "192.0.2.10".to_owned(),
                    stream_id: Some("main".to_owned()),
                    kind: OperationalEventKind::CameraOffline,
                },
                evidence: OperationalEvidence {
                    cause: "offline".to_owned(),
                    explanation: "cam 10 offline".to_owned(),
                    affected_streams: vec!["main".to_owned()],
                    recording_interrupted: true,
                    source: "test".to_owned(),
                },
                severity: OperationalSeverity::Warning,
                revision: 1,
                start_time_ms: 1_000,
                end_time_ms: None,
                duration_ms: None,
            },
        ];
        health.issues = vec![
            crate::health::HealthIssue {
                severity: "warning".to_owned(),
                scope: "camera".to_owned(),
                message: "camera 192.0.2.1 stream offline".to_owned(),
                operational_event_id: Some("evt-cam-1".to_owned()),
                timeline_start_ms: Some(1_000),
                timeline_end_ms: None,
            },
            crate::health::HealthIssue {
                severity: "warning".to_owned(),
                scope: "camera".to_owned(),
                message: "camera 192.0.2.10 stream offline".to_owned(),
                operational_event_id: Some("evt-cam-10".to_owned()),
                timeline_start_ms: Some(1_000),
                timeline_end_ms: None,
            },
            crate::health::HealthIssue {
                severity: "warning".to_owned(),
                scope: "storage".to_owned(),
                message: "global disk pressure on host".to_owned(),
                operational_event_id: None,
                timeline_start_ms: None,
                timeline_end_ms: None,
            },
        ];

        let policy = CameraAccess {
            all_cameras: false,
            group_ids: Vec::new(),
            camera_ids: vec!["192.0.2.1".to_owned()],
        };
        filter_health_for_user(&mut health, &policy);
        assert_eq!(health.cameras.len(), 1);
        assert_eq!(health.cameras[0].id, "192.0.2.1");
        assert_eq!(health.issues.len(), 1);
        assert_eq!(
            health.issues[0].operational_event_id.as_deref(),
            Some("evt-cam-1")
        );
    }

    #[test]
    fn mcp_snapshot_fails_closed_when_privacy_mode_is_active_and_records_audit() {
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_snapshots: true,
                ..McpConfig::default()
            },
            "SNAPSHOT-PRIVACY-SECRET-111",
        );
        state
            .privacy
            .replace_profile_overrides(BTreeMap::from([("192.0.2.10".to_owned(), true)]))
            .unwrap();

        let resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 77,
                    "method": "tools/call",
                    "params": {
                        "name": "get_camera_snapshot",
                        "arguments": { "camera_id": "192.0.2.10" }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(resp.status_code, 200);
        let body = read_response_json(resp);
        assert_eq!(body["error"]["code"], ERROR_FORBIDDEN);
        let audits = state.access_manager.list_audit(32);
        assert!(
            audits
                .iter()
                .any(|event| event.action == "mcp_camera_snapshot"
                    && event.result == "denied_privacy")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_concurrent_latency_and_response_budget_benchmark() {
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: false,
                max_concurrent_requests: 4,
                max_response_bytes: 65_536,
                ..McpConfig::default()
            },
            "BENCH-SECRET-SENTINEL-424242",
        );
        let (logging_service, dispatch) =
            LoggingService::for_test(LogFilterFile::new(dir.join("log-filter")), "info");
        let state = state.with_logging(logging_service);
        tracing::dispatcher::with_default(&dispatch, || {
            for index in 0..25 {
                tracing::info!(target: "keeppeek::ingest", camera = "192.0.2.10", "frame ok {index}");
            }
        });

        let mut handles = Vec::new();
        for worker in 0..3_u64 {
            let worker_state = state.clone();
            let worker_router = router_tx.clone();
            handles.push(std::thread::spawn(move || {
                let mut latencies_us = Vec::new();
                let mut max_bytes = 0usize;
                let requests = [
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_server_health","arguments":{}}}),
                    json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_logs","arguments":{"limit":20}}}),
                    json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_configuration","arguments":{"page_size":10}}}),
                ];
                for iter in 0..12 {
                    let payload = &requests[(usize::try_from(worker).unwrap_or(0) + iter) % 3];
                    let req = mcp_post_request(payload, Vec::new(), "127.0.0.1:50000");
                    let started = Instant::now();
                    let resp = super::super::handle_request(&req, &worker_router, &worker_state);
                    let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                    assert_eq!(resp.status_code, 200);
                    let (mut reader, _) = resp.data.into_reader_and_size();
                    let mut raw = Vec::new();
                    reader.read_to_end(&mut raw).unwrap();
                    assert!(raw.len() <= worker_state.mcp.config().max_response_bytes);
                    max_bytes = max_bytes.max(raw.len());
                    latencies_us.push(elapsed_us);
                }
                (latencies_us, max_bytes)
            }));
        }

        let mut all_latencies_us = Vec::new();
        let mut max_response_bytes = 0usize;
        for handle in handles {
            let (latencies, max_bytes) = handle.join().unwrap();
            all_latencies_us.extend(latencies);
            max_response_bytes = max_response_bytes.max(max_bytes);
        }
        assert_eq!(all_latencies_us.len(), 36);
        assert!(max_response_bytes <= state.mcp.config().max_response_bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_ac8_test_camera_ingest_isolation_and_latency() {
        use crate::storage::{
            MediaFrame, RecordingCatalog, RecordingFrame, StorageConfig, StorageEngine, VideoCodec,
            VideoFrame, nal::annexb_to_avcc,
        };
        use std::sync::atomic::AtomicBool;
        use test_frame::{EncodedFrame, TestFrameConfig, TestFrameSource};

        const FPS: u64 = 30;
        const WINDOW_SECS: u64 = 5;
        const FRAMES_SENT: usize = (FPS * WINDOW_SECS) as usize;
        const FRAME_INTERVAL_NS: u64 = 1_000_000_000 / FPS;
        const MAX_GOP_GAP_MS: i64 = 600;

        let (tx, rx) = std::sync::mpsc::sync_channel::<EncodedFrame>(40);
        let source = TestFrameSource::start(
            TestFrameConfig {
                width: 320,
                height: 240,
                fps: FPS as u32,
                codec: test_frame::Codec::H264,
                keyframe_interval: 15,
                bitrate_bps: 500_000,
            },
            move |frame| {
                let _ = tx.send(frame);
            },
        )
        .expect("start deterministic H.264 test source");
        let mut template_frames = Vec::with_capacity(30);
        while template_frames.len() < 30 {
            template_frames.push(
                rx.recv_timeout(Duration::from_secs(5))
                    .expect("H.264 frame"),
            );
        }
        source.stop();

        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: false,
                max_concurrent_requests: 4,
                max_response_bytes: 65_536,
                ..McpConfig::default()
            },
            "AC8-BENCH-SECRET-424242",
        );
        let (logging_service, dispatch) =
            LoggingService::for_test(LogFilterFile::new(dir.join("log-filter")), "info");
        let state = state.with_logging(logging_service);
        tracing::dispatcher::with_default(&dispatch, || {
            for index in 0..100 {
                tracing::info!(target: "keeppeek::ingest", camera = "192.0.2.10", "frame ok {index}");
            }
        });

        struct PhaseOutcome {
            frames_recorded: usize,
            fragments: usize,
            covered_ms: i64,
            max_gap_ms: i64,
            mcp_latencies_us: Vec<u64>,
            max_response_bytes: usize,
        }

        let run_phase = |name: &str, with_mcp_load: bool| -> PhaseOutcome {
            let phase_dir = dir.join(name);
            std::fs::create_dir_all(&phase_dir).unwrap();
            let catalog = RecordingCatalog::open(&phase_dir.join("recordings.db")).unwrap();
            let catalog_handle = catalog.handle();
            let engine = StorageEngine::start_with_catalog(
                StorageConfig {
                    medium_term_path: phase_dir.clone(),
                    long_term_path: phase_dir.clone(),
                    recording_catalog_path: phase_dir.join("recordings.db"),
                    event_thumbnail_path: phase_dir.join(".event-thumbnails"),
                    short_term_duration: Duration::from_secs(2),
                    medium_term_duration: Duration::from_secs(5),
                    flush_interval: Duration::from_secs(1),
                    write_buffer_bytes: 8 * 1024,
                    long_term_max_bytes: 0,
                    minimum_free_bytes: 0,
                    ..StorageConfig::default()
                },
                catalog_handle.clone(),
            );
            let stop = Arc::new(AtomicBool::new(false));
            let mut mcp_workers = Vec::new();
            if with_mcp_load {
                for worker in 0..3_usize {
                    let worker_state = state.clone();
                    let worker_router = router_tx.clone();
                    let worker_stop = stop.clone();
                    mcp_workers.push(std::thread::spawn(move || {
                        let requests = [
                            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_server_health","arguments":{}}}),
                            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_logs","arguments":{"limit":20}}}),
                            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_configuration","arguments":{"page_size":10}}}),
                        ];
                        let mut latencies = Vec::new();
                        let mut max_bytes = 0usize;
                        let mut iter = 0usize;
                        while !worker_stop.load(Ordering::Acquire) {
                            let payload = &requests[(worker + iter) % 3];
                            let req = mcp_post_request(payload, Vec::new(), "127.0.0.1:50000");
                            let started = Instant::now();
                            let resp =
                                super::super::handle_request(&req, &worker_router, &worker_state);
                            let elapsed_us =
                                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                            assert_eq!(resp.status_code, 200);
                            let (mut reader, _) = resp.data.into_reader_and_size();
                            let mut raw = Vec::new();
                            reader.read_to_end(&mut raw).unwrap();
                            assert!(raw.len() <= worker_state.mcp.config().max_response_bytes);
                            max_bytes = max_bytes.max(raw.len());
                            latencies.push(elapsed_us);
                            iter += 1;
                        }
                        (latencies, max_bytes)
                    }));
                }
            }

            let phase_start = Instant::now();
            for frame_idx in 0..FRAMES_SENT {
                let target_offset = Duration::from_nanos(frame_idx as u64 * FRAME_INTERVAL_NS);
                let target_time = phase_start + target_offset;
                let now = Instant::now();
                if now < target_time {
                    std::thread::sleep(target_time - now);
                }
                let template = &template_frames[frame_idx % template_frames.len()];
                engine.ingest(
                    "192.0.2.10",
                    RecordingFrame {
                        source_rtp: None,
                        received_at: Instant::now(),
                        timestamp: Some(target_offset),
                        frame: MediaFrame::Video(VideoFrame {
                            codec: VideoCodec::H264,
                            is_keyframe: template.is_keyframe,
                            width: template.width,
                            height: template.height,
                            data: annexb_to_avcc(&template.data).into(),
                        }),
                    },
                );
            }

            stop.store(true, Ordering::Release);
            let mut mcp_latencies_us = Vec::new();
            let mut max_response_bytes = 0usize;
            for worker in mcp_workers {
                let (worker_latencies, worker_max) = worker.join().unwrap();
                mcp_latencies_us.extend(worker_latencies);
                max_response_bytes = max_response_bytes.max(worker_max);
            }

            engine.flush_all();
            engine.shutdown();

            let mut fragments = catalog_handle
                .media_fragments_in_range("192.0.2.10", i64::MIN + 1, i64::MAX)
                .unwrap();
            fragments.sort_by_key(|fragment| (fragment.start_ms, fragment.sequence));
            let unique_paths: std::collections::BTreeSet<String> = fragments
                .iter()
                .map(|fragment| fragment.path.clone())
                .collect();
            let mut frames_recorded = 0usize;
            for path in &unique_paths {
                let file = std::fs::File::open(path).unwrap();
                let reader = mp4::read_mp4(file).unwrap();
                let video_samples: usize = reader
                    .tracks()
                    .values()
                    .filter(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
                    .map(|track| track.sample_count() as usize)
                    .sum();
                frames_recorded += video_samples;
            }
            let covered_ms = match (fragments.first(), fragments.last()) {
                (Some(first), Some(last)) => {
                    last.start_ms + i64::try_from(last.duration_ms).unwrap_or(0) - first.start_ms
                }
                _ => 0,
            };
            let mut max_gap_ms = 0i64;
            for pair in fragments.windows(2) {
                let prev_end = pair[0].start_ms + i64::try_from(pair[0].duration_ms).unwrap_or(0);
                let gap = pair[1].start_ms - prev_end;
                if gap > max_gap_ms {
                    max_gap_ms = gap;
                }
            }
            drop(catalog_handle);
            catalog.shutdown();

            PhaseOutcome {
                frames_recorded,
                fragments: fragments.len(),
                covered_ms,
                max_gap_ms,
                mcp_latencies_us,
                max_response_bytes,
            }
        };

        let baseline = run_phase("baseline", false);
        let mut active = run_phase("active", true);
        active.mcp_latencies_us.sort_unstable();
        let mcp_count = active.mcp_latencies_us.len();
        let mcp_p50_ms = active.mcp_latencies_us[mcp_count / 2] as f64 / 1000.0;
        let mcp_p95_ms = active.mcp_latencies_us[(mcp_count * 95) / 100] as f64 / 1000.0;
        let mcp_max_ms = *active.mcp_latencies_us.last().unwrap() as f64 / 1000.0;

        eprintln!(
            "AC-8 REALTIME RECORDING CONTINUITY METRICS: window_s={WINDOW_SECS} fps={FPS} frames_sent={FRAMES_SENT} base_recorded={} base_fragments={} base_covered_ms={} base_max_gap_ms={} active_recorded={} active_fragments={} active_covered_ms={} active_max_gap_ms={} mcp_calls={mcp_count} mcp_p50_ms={mcp_p50_ms:.3} mcp_p95_ms={mcp_p95_ms:.3} mcp_max_ms={mcp_max_ms:.3} max_response_bytes={}",
            baseline.frames_recorded,
            baseline.fragments,
            baseline.covered_ms,
            baseline.max_gap_ms,
            active.frames_recorded,
            active.fragments,
            active.covered_ms,
            active.max_gap_ms,
            active.max_response_bytes,
        );

        assert_eq!(baseline.frames_recorded, FRAMES_SENT);
        assert_eq!(active.frames_recorded, FRAMES_SENT);
        assert!(baseline.covered_ms >= 4_500);
        assert!(active.covered_ms >= 4_500);
        assert!(baseline.max_gap_ms <= MAX_GOP_GAP_MS);
        assert!(active.max_gap_ms <= MAX_GOP_GAP_MS);
        assert!(mcp_count >= 50);
        assert!(active.max_response_bytes <= state.mcp.config().max_response_bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_secret_handoff_end_to_end_live_reload_and_security_gates() {
        let secret_sentinel = "HANDOFF-LIVE-SENTINEL-665544";
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: true,
                ..McpConfig::default()
            },
            "EXISTING-CAM-SENTINEL-111",
        );

        let req_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "request_secret_entry",
                        "arguments": {
                            "keys": [
                                { "key": "NEW_CAM_USER", "purpose": "Front camera username" },
                                { "key": "NEW_CAM_PASS", "purpose": "Front camera password" }
                            ],
                            "ttl_seconds": 120
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let req_json = read_response_json(req_resp);
        let structured = &req_json["result"]["structuredContent"];
        let handoff_id = structured["handoff_id"].as_str().unwrap().to_owned();
        let local_url = structured["local_ui_url"].as_str().unwrap().to_owned();
        assert!(local_url.starts_with("http://127.0.0.1:8081/mcp/secret-entry?token="));
        assert!(!local_url.contains("[REDACTED]"));
        let token = local_url.split("token=").nth(1).unwrap().to_owned();
        assert!(token.len() >= 40);

        // Status starts pending.
        let status_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "get_secret_entry_status",
                        "arguments": { "handoff_id": handoff_id }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(status_resp)["result"]["structuredContent"]["status"],
            "pending"
        );

        // Non-loopback, proxied, non-loopback Host, cross-origin POST, and unsupported method must fail.
        let non_loopback = Request::fake_http_from(
            "198.51.100.10:50000".parse().unwrap(),
            "GET",
            format!("/mcp/secret-entry?token={token}"),
            vec![("Host".into(), "127.0.0.1:8081".into())],
            Vec::new(),
        );
        assert_eq!(
            super::super::handle_request(&non_loopback, &router_tx, &state).status_code,
            403
        );
        let proxied = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "GET",
            format!("/mcp/secret-entry?token={token}"),
            vec![
                ("Host".into(), "127.0.0.1:8081".into()),
                ("X-Forwarded-For".into(), "198.51.100.10".into()),
            ],
            Vec::new(),
        );
        assert_eq!(
            super::super::handle_request(&proxied, &router_tx, &state).status_code,
            403
        );
        let bad_host = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "GET",
            format!("/mcp/secret-entry?token={token}"),
            vec![("Host".into(), "evil.example:8081".into())],
            Vec::new(),
        );
        assert_eq!(
            super::super::handle_request(&bad_host, &router_tx, &state).status_code,
            403
        );
        let bad_origin = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "POST",
            "/mcp/secret-entry",
            vec![
                ("Host".into(), "127.0.0.1:8081".into()),
                ("Origin".into(), "http://evil.example:8081".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            b"{}".to_vec(),
        );
        assert_eq!(
            super::super::handle_request(&bad_origin, &router_tx, &state).status_code,
            403
        );
        let unsupported = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "DELETE",
            "/mcp/secret-entry",
            vec![("Host".into(), "127.0.0.1:8081".into())],
            Vec::new(),
        );
        assert_eq!(
            super::super::handle_request(&unsupported, &router_tx, &state).status_code,
            405
        );

        // GET renders self-contained HTML form with strict security headers.
        let get_form = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "GET",
            format!("/mcp/secret-entry?token={token}"),
            vec![("Host".into(), "127.0.0.1:8081".into())],
            Vec::new(),
        );
        let form_resp = super::super::handle_request(&get_form, &router_tx, &state);
        assert_eq!(form_resp.status_code, 200);
        let has_header = |resp: &Response, key: &str, needle: &str| {
            resp.headers.iter().any(|(k, v)| {
                k.eq_ignore_ascii_case(key)
                    && v.to_ascii_lowercase()
                        .contains(&needle.to_ascii_lowercase())
            })
        };
        assert!(has_header(&form_resp, "Cache-Control", "no-store"));
        assert!(has_header(&form_resp, "Referrer-Policy", "same-origin"));
        assert!(has_header(&form_resp, "X-Frame-Options", "deny"));
        assert!(has_header(
            &form_resp,
            "Content-Security-Policy",
            "default-src 'none'"
        ));

        // Origin: null (e.g. from no-referrer / sandboxed iframe) must still be rejected.
        let null_origin = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "POST",
            "/mcp/secret-entry",
            vec![
                ("Host".into(), "127.0.0.1:8081".into()),
                ("Origin".into(), "null".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            serde_json::to_vec(&json!({ "token": token, "secrets": { "NEW_CAM_PASS": "x" } }))
                .unwrap(),
        );
        assert_eq!(
            super::super::handle_request(&null_origin, &router_tx, &state).status_code,
            403
        );

        // Concurrent double-submit over loopback POST with browser same-origin Origin:
        // exactly 1 succeeds (200) and the concurrent second fails (403/410), with 1 audit.
        let form_body = format!(
            "token={token}&secret_NEW_CAM_USER=onboard_admin&secret_NEW_CAM_PASS={secret_sentinel}"
        );
        let barrier = std::sync::Barrier::new(2);
        let post_results: Vec<(u16, String)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let body = form_body.clone();
                    let state_ref = &state;
                    let router_ref = &router_tx;
                    let barrier_ref = &barrier;
                    scope.spawn(move || {
                        let req = Request::fake_http_from(
                            "127.0.0.1:50000".parse().unwrap(),
                            "POST",
                            "/mcp/secret-entry",
                            vec![
                                ("Host".into(), "127.0.0.1:8081".into()),
                                ("Origin".into(), "http://127.0.0.1:8081".into()),
                                (
                                    "Content-Type".into(),
                                    "application/x-www-form-urlencoded".into(),
                                ),
                            ],
                            body.into_bytes(),
                        );
                        barrier_ref.wait();
                        let resp = super::super::handle_request(&req, router_ref, state_ref);
                        let status = resp.status_code;
                        let (mut reader, _) = resp.data.into_reader_and_size();
                        let mut text = String::new();
                        reader.read_to_string(&mut text).unwrap();
                        (status, text)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let successes: Vec<&String> = post_results
            .iter()
            .filter(|(status, _)| *status == 200)
            .map(|(_, body)| body)
            .collect();
        let failures: Vec<u16> = post_results
            .iter()
            .filter(|(status, _)| *status != 200)
            .map(|(status, _)| *status)
            .collect();
        assert_eq!(successes.len(), 1);
        assert_eq!(failures.len(), 1);
        assert!(matches!(failures[0], 403 | 410));
        assert!(successes[0].contains("NEW_CAM_PASS"));
        assert!(!successes[0].contains(secret_sentinel));

        // Status is completed with zero secret values.
        let done_status = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "get_secret_entry_status",
                        "arguments": { "handoff_id": handoff_id }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let done_json = read_response_json(done_status);
        assert!(
            !serde_json::to_string(&done_json)
                .unwrap()
                .contains(secret_sentinel)
        );
        assert_eq!(
            done_json["result"]["structuredContent"]["status"],
            "completed"
        );
        assert_eq!(
            done_json["result"]["structuredContent"]["configured_keys"],
            json!(["NEW_CAM_PASS", "NEW_CAM_USER"])
        );

        // Replay must be rejected immediately.
        let replay = Request::fake_http_from(
            "127.0.0.1:50000".parse().unwrap(),
            "POST",
            "/mcp/secret-entry",
            vec![
                ("Host".into(), "127.0.0.1:8081".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            serde_json::to_vec(&json!({ "token": token, "secrets": { "NEW_CAM_PASS": "x" } }))
                .unwrap(),
        );
        assert_eq!(
            super::super::handle_request(&replay, &router_tx, &state).status_code,
            403
        );

        // Newly stored {secret:NEW_CAM_PASS} works immediately in manage_camera without restart.
        let cfg_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_configuration","arguments":{}}}),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let revision =
            read_response_json(cfg_resp)["result"]["structuredContent"]["configuration_revision"]
                .as_str()
                .unwrap()
                .to_owned();
        let cam_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 5,
                    "method": "tools/call",
                    "params": {
                        "name": "manage_camera",
                        "arguments": {
                            "action": "upsert",
                            "ip": "192.0.2.77",
                            "expected_configuration_revision": revision,
                            "username": "{secret:NEW_CAM_USER}",
                            "password": "{secret:NEW_CAM_PASS}"
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let cam_json = read_response_json(cam_resp);
        assert!(
            !serde_json::to_string(&cam_json)
                .unwrap()
                .contains(secret_sentinel)
        );
        assert_eq!(
            cam_json["result"]["structuredContent"]["camera"]["ip"],
            "192.0.2.77"
        );

        // Audit records never contain the submitted secret value and count exactly 1 completion.
        let audits = state.access_manager.list_audit(64);
        assert_eq!(
            audits
                .iter()
                .filter(|event| event.action == "mcp_secret_entry_completed"
                    && event.result == "success")
                .count(),
            1
        );
        assert!(
            !serde_json::to_string(&audits)
                .unwrap()
                .contains(secret_sentinel)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_onboarding_wire_catalog_probe_and_device_settings_e2e() {
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: false,
                allow_camera_device_writes: false,
                ..McpConfig::default()
            },
            "ONBOARD-WIRE-SENTINEL-2233",
        );

        let catalog_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "search_camera_catalog",
                        "arguments": { "query": "reolink", "ip": "192.0.2.10", "limit": 5 }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let catalog_json = read_response_json(catalog_resp);
        assert!(
            catalog_json["result"]["structuredContent"]["count"]
                .as_u64()
                .unwrap_or(0)
                >= 1
        );

        // probe_camera_streams rejects raw plaintext passwords and >5 candidates.
        let raw_probe = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "probe_camera_streams",
                        "arguments": { "ip": "192.0.2.10", "password": "raw-password" }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(raw_probe)["error"]["code"],
            ERROR_INVALID_PARAMS
        );

        let too_many = (0..6)
            .map(|_| {
                json!({
                    "username_secret_reference": "{secret:CAMERA_PASSWORD}",
                    "password_secret_reference": "{secret:CAMERA_PASSWORD}"
                })
            })
            .collect::<Vec<_>>();
        let too_many_resp = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "probe_camera_streams",
                        "arguments": { "ip": "192.0.2.10", "credential_candidates": too_many }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(too_many_resp)["error"]["code"],
            ERROR_INVALID_PARAMS
        );

        // get_camera_device_settings reports rtsp_only honestly for an RTSP-only fixture camera.
        let dev_get = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "tools/call",
                    "params": {
                        "name": "get_camera_device_settings",
                        "arguments": { "camera_id": "192.0.2.10" }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let dev_json = read_response_json(dev_get);
        assert_eq!(
            dev_json["result"]["structuredContent"]["protocol"],
            "rtsp_only"
        );

        // update_camera_device_settings blocks dry_run=false when allow_camera_device_writes=false
        // and blocks prohibited security fields even on dry_run=true.
        let dev_write_blocked = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 5,
                    "method": "tools/call",
                    "params": {
                        "name": "update_camera_device_settings",
                        "arguments": {
                            "camera_id": "192.0.2.10",
                            "dry_run": false,
                            "settings": { "osd": { "text": "Front" } }
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(dev_write_blocked)["error"]["code"],
            ERROR_FORBIDDEN
        );

        let excluded_field = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 6,
                    "method": "tools/call",
                    "params": {
                        "name": "update_camera_device_settings",
                        "arguments": {
                            "camera_id": "192.0.2.10",
                            "dry_run": true,
                            "settings": { "factory_reset": true }
                        }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(excluded_field)["error"]["code"],
            ERROR_FORBIDDEN
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_432_onboarding_performance_and_ingest_isolation_benchmark() {
        use crate::storage::{
            MediaFrame, RecordingCatalog, RecordingFrame, StorageConfig, StorageEngine, VideoCodec,
            VideoFrame, nal::annexb_to_avcc,
        };
        use std::sync::atomic::AtomicBool;
        use test_frame::{EncodedFrame, TestFrameConfig, TestFrameSource};

        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_writes: false,
                max_concurrent_requests: 4,
                max_response_bytes: 65_536,
                ..McpConfig::default()
            },
            "BENCH-432-SENTINEL-887766",
        );

        // 1. Measure >= 30 search_camera_catalog calls.
        let mut catalog_us = Vec::with_capacity(40);
        for iter in 0..40 {
            let query = if iter % 2 == 0 {
                "reolink"
            } else {
                "hikvision"
            };
            let req = mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": iter + 1,
                    "method": "tools/call",
                    "params": {
                        "name": "search_camera_catalog",
                        "arguments": { "query": query, "ip": "192.0.2.10", "limit": 10 }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            );
            let started = Instant::now();
            let resp = super::super::handle_request(&req, &router_tx, &state);
            let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            assert_eq!(resp.status_code, 200);
            let body = read_response_json(resp);
            assert!(
                body["result"]["structuredContent"]["count"]
                    .as_u64()
                    .unwrap_or(0)
                    >= 1,
                "expected catalog match for query={query}: {body}"
            );
            catalog_us.push(elapsed_us);
        }
        catalog_us.sort_unstable();
        let catalog_p50_ms = catalog_us[catalog_us.len() / 2] as f64 / 1000.0;
        let catalog_p95_ms = catalog_us[(catalog_us.len() * 95) / 100] as f64 / 1000.0;

        // 2. Measure >= 30 live secrets.toml reload cycles.
        let secrets_path = config::secrets_path(state.camera_config_path.as_deref().unwrap());
        let mut reload_us = Vec::with_capacity(30);
        for _ in 0..30 {
            let started = Instant::now();
            let loaded = config::secrets_store::load_secrets_file_with_lock(
                &secrets_path,
                &state.config_update,
            )
            .unwrap();
            state
                .mcp
                .refresh_secrets(state.camera_config_path.as_deref().unwrap(), loaded);
            reload_us.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
        }
        reload_us.sort_unstable();
        let reload_p50_ms = reload_us[reload_us.len() / 2] as f64 / 1000.0;
        let reload_p95_ms = reload_us[(reload_us.len() * 95) / 100] as f64 / 1000.0;

        // 3. Real-time 5 s 30 fps H.264 ingest alongside 3 concurrent onboarding MCP workers.
        const FPS: u64 = 30;
        const FRAMES_SENT: usize = 150;
        const FRAME_INTERVAL_NS: u64 = 1_000_000_000 / FPS;
        let (tx, rx) = std::sync::mpsc::sync_channel::<EncodedFrame>(40);
        let source = TestFrameSource::start(
            TestFrameConfig {
                width: 320,
                height: 240,
                fps: 30,
                codec: test_frame::Codec::H264,
                keyframe_interval: 15,
                bitrate_bps: 500_000,
            },
            move |frame| {
                let _ = tx.send(frame);
            },
        )
        .expect("start deterministic H.264 test source");
        let mut template_frames = Vec::with_capacity(30);
        while template_frames.len() < 30 {
            template_frames.push(rx.recv_timeout(Duration::from_secs(5)).expect("frame"));
        }
        source.stop();

        let rec_dir = dir.join("rec");
        std::fs::create_dir_all(&rec_dir).unwrap();
        let catalog = RecordingCatalog::open(&rec_dir.join("recordings.db")).unwrap();
        let catalog_handle = catalog.handle();
        let engine = StorageEngine::start_with_catalog(
            StorageConfig {
                medium_term_path: rec_dir.clone(),
                long_term_path: rec_dir.clone(),
                recording_catalog_path: rec_dir.join("recordings.db"),
                event_thumbnail_path: rec_dir.join(".event-thumbnails"),
                short_term_duration: Duration::from_secs(2),
                medium_term_duration: Duration::from_secs(5),
                flush_interval: Duration::from_secs(1),
                write_buffer_bytes: 8 * 1024,
                long_term_max_bytes: 0,
                minimum_free_bytes: 0,
                ..StorageConfig::default()
            },
            catalog_handle.clone(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();
        for worker in 0..3_usize {
            let worker_state = state.clone();
            let worker_router = router_tx.clone();
            let worker_stop = stop.clone();
            workers.push(std::thread::spawn(move || {
                let requests = [
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_camera_catalog","arguments":{"query":"reolink","ip":"192.0.2.10","limit":10}}}),
                    json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_camera_device_settings","arguments":{"camera_id":"192.0.2.10"}}}),
                    json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_secret_keys","arguments":{}}}),
                ];
                let mut calls = 0usize;
                while !worker_stop.load(Ordering::Acquire) {
                    let payload = &requests[(worker + calls) % 3];
                    let req = mcp_post_request(payload, Vec::new(), "127.0.0.1:50000");
                    let resp = super::super::handle_request(&req, &worker_router, &worker_state);
                    assert_eq!(resp.status_code, 200);
                    calls += 1;
                }
                calls
            }));
        }

        let phase_start = Instant::now();
        for idx in 0..FRAMES_SENT {
            let offset = Duration::from_nanos(idx as u64 * FRAME_INTERVAL_NS);
            let target = phase_start + offset;
            let now = Instant::now();
            if now < target {
                std::thread::sleep(target - now);
            }
            let template = &template_frames[idx % template_frames.len()];
            engine.ingest(
                "192.0.2.10",
                RecordingFrame {
                    source_rtp: None,
                    received_at: Instant::now(),
                    timestamp: Some(offset),
                    frame: MediaFrame::Video(VideoFrame {
                        codec: VideoCodec::H264,
                        is_keyframe: template.is_keyframe,
                        width: template.width,
                        height: template.height,
                        data: annexb_to_avcc(&template.data).into(),
                    }),
                },
            );
        }
        stop.store(true, Ordering::Release);
        let total_mcp_calls: usize = workers.into_iter().map(|h| h.join().unwrap()).sum();
        engine.flush_all();
        engine.shutdown();

        let fragments = catalog_handle
            .media_fragments_in_range("192.0.2.10", i64::MIN + 1, i64::MAX)
            .unwrap();
        let unique_paths: std::collections::BTreeSet<String> = fragments
            .iter()
            .map(|fragment| fragment.path.clone())
            .collect();
        let mut frames_recorded = 0usize;
        for path in &unique_paths {
            let reader = mp4::read_mp4(std::fs::File::open(path).unwrap()).unwrap();
            frames_recorded += reader
                .tracks()
                .values()
                .filter(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
                .map(|track| track.sample_count() as usize)
                .sum::<usize>();
        }
        drop(catalog_handle);
        catalog.shutdown();

        eprintln!(
            "ISSUE #432 PERF METRICS: catalog_calls=40 catalog_p50_ms={catalog_p50_ms:.3} catalog_p95_ms={catalog_p95_ms:.3} reload_calls=30 reload_p50_ms={reload_p50_ms:.3} reload_p95_ms={reload_p95_ms:.3} ingest_frames_sent={FRAMES_SENT} ingest_frames_recorded={frames_recorded} concurrent_mcp_calls={total_mcp_calls}"
        );
        assert_eq!(frames_recorded, FRAMES_SENT);
        assert!(total_mcp_calls >= 50);
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct HttpDebugBoundaryMock {
        sentinel: String,
    }

    impl mcp_camera_debug::CameraDebugService for HttpDebugBoundaryMock {
        fn capture(
            &self,
            request: mcp_camera_debug::CaptureRequest,
        ) -> Result<Value, ControlCommandError> {
            let password = request.credentials.as_ref().map(|c| c.password.clone());
            Ok(json!({
                "action": "capture",
                "camera_id": request.target.camera_id,
                "capture_id": "cap-http-1",
                "password": password,
            }))
        }

        fn compare_transports(
            &self,
            request: mcp_camera_debug::CompareTransportsRequest,
        ) -> Result<Value, ControlCommandError> {
            Ok(json!({
                "action": "compare_transports",
                "camera_id": request.target.camera_id,
            }))
        }

        fn export_repro_fixture(
            &self,
            request: mcp_camera_debug::ExportReproFixtureRequest,
        ) -> Result<Value, ControlCommandError> {
            Ok(json!({
                "action": "export_repro_fixture",
                "capture_id": request.capture_id,
                "strip_credentials": request.strip_credentials,
                "leaked_if_unredacted": self.sentinel,
            }))
        }

        fn list_captures(
            &self,
            _request: mcp_camera_debug::ListCapturesRequest,
        ) -> Result<Value, ControlCommandError> {
            Ok(json!({ "action": "list_captures", "captures": [] }))
        }

        fn delete_capture(
            &self,
            _request: mcp_camera_debug::DeleteCaptureRequest,
        ) -> Result<Value, ControlCommandError> {
            Ok(json!({ "action": "delete_capture", "deleted": 1 }))
        }

        fn read_artifact(
            &self,
            request: mcp_camera_debug::ReadArtifactRequest,
        ) -> Result<Value, ControlCommandError> {
            Ok(json!({
                "action": "read_artifact",
                "capture_id": request.capture_id,
                "camera_id": "192.0.2.10",
                "artifact": request.artifact,
                "offset": request.offset,
                "length": self.sentinel.len(),
                "total_bytes": self.sentinel.len(),
                "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                "eof": true,
                "next_offset": request.offset + self.sentinel.len() as u64,
                "encoding": "utf8",
                "data": self.sentinel,
            }))
        }
    }

    #[test]
    fn camera_debugging_http_gate_read_only_and_redaction_boundary() {
        let sentinel = "RAW-DEBUG-SENTINEL-998877";
        let (dir, state, router_tx) = temp_config_fixture(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                ..McpConfig::default()
            },
            sentinel,
        );
        let state = state.with_camera_debug_service(Arc::new(HttpDebugBoundaryMock {
            sentinel: sentinel.to_owned(),
        }));

        // Normal MCP tools remain strictly redacted even when allow_camera_debugging is true.
        let cfg_resp = call_mcp_tool(&state, &router_tx, 1, "get_configuration", json!({}));
        assert!(!serde_json::to_string(&cfg_resp).unwrap().contains(sentinel));

        // Default capture (include_raw_auth: true) passes raw debug credentials when allowed.
        let raw_cap_json = call_mcp_tool(
            &state,
            &router_tx,
            2,
            "camera_debugging",
            json!({"action":"capture","camera_id":"192.0.2.10"}),
        );
        assert_eq!(
            raw_cap_json["result"]["structuredContent"]["password"],
            sentinel
        );

        // capture with include_raw_auth: false enforces SecretRedactor.
        let redacted_cap_json = call_mcp_tool(
            &state,
            &router_tx,
            3,
            "camera_debugging",
            json!({"action":"capture","camera_id":"192.0.2.10","include_raw_auth":false}),
        );
        assert!(
            !serde_json::to_string(&redacted_cap_json)
                .unwrap()
                .contains(sentinel)
        );

        assert_repro_export_redaction_boundary(&state, &router_tx, sentinel);
        assert_read_only_header_blocks_capture_only(&state, &router_tx);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn assert_repro_export_redaction_boundary(
        state: &ServerState,
        router_tx: &FacadeSender<RouterMessage>,
        sentinel: &str,
    ) {
        // export_repro_fixture strictly redacts when strip_credentials is true (default).
        let stripped_export_json = call_mcp_tool(
            state,
            router_tx,
            4,
            "camera_debugging",
            json!({"action":"export_repro_fixture","capture_id":"cap-http-1"}),
        );
        assert!(
            !serde_json::to_string(&stripped_export_json)
                .unwrap()
                .contains(sentinel)
        );

        // export_repro_fixture with strip_credentials: false allows unredacted fixture output.
        let raw_export_json = call_mcp_tool(
            state,
            router_tx,
            5,
            "camera_debugging",
            json!({"action":"export_repro_fixture","capture_id":"cap-http-1","strip_credentials":false}),
        );
        assert_eq!(
            raw_export_json["result"]["structuredContent"]["leaked_if_unredacted"],
            sentinel
        );
    }

    fn assert_read_only_header_blocks_capture_only(
        state: &ServerState,
        router_tx: &FacadeSender<RouterMessage>,
    ) {
        let ro_headers = vec![(READ_ONLY_HEADER.to_owned(), "true".to_owned())];
        let ro_cap = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"camera_debugging","arguments":{"action":"capture","camera_id":"192.0.2.10"}}}),
                ro_headers.clone(),
                "127.0.0.1:50000",
            ),
            router_tx,
            state,
        );
        assert_eq!(read_response_json(ro_cap)["error"]["code"], ERROR_FORBIDDEN);

        let ro_export = super::super::handle_request(
            &mcp_post_request(
                &json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"camera_debugging","arguments":{"action":"export_repro_fixture","capture_id":"cap-http-1"}}}),
                ro_headers,
                "127.0.0.1:50000",
            ),
            router_tx,
            state,
        );
        assert!(read_response_json(ro_export)["result"].is_object());
    }

    fn call_mcp_tool(
        state: &ServerState,
        router_tx: &FacadeSender<RouterMessage>,
        id: u64,
        name: &str,
        arguments: Value,
    ) -> Value {
        let req = mcp_post_request(
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }),
            Vec::new(),
            "127.0.0.1:50000",
        );
        let resp = super::super::handle_request(&req, router_tx, state);
        assert_eq!(resp.status_code, 200);
        read_response_json(resp)
    }

    fn assert_valid_pcapng_blocks(path: &std::path::Path) {
        let bytes = std::fs::read(path).expect("read capture.pcapng");
        assert!(bytes.len() >= 28, "pcapng file too short");
        let shb_magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(shb_magic, 0x0A0D_0D0A);
        let mut offset = 0usize;
        let mut saw_shb = false;
        let mut saw_idb = false;
        let mut saw_epb = false;
        while offset + 12 <= bytes.len() {
            let block_type = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            let block_len = usize::try_from(u32::from_le_bytes(
                bytes[offset + 4..offset + 8].try_into().unwrap(),
            ))
            .unwrap();
            assert!(block_len >= 12 && offset + block_len <= bytes.len());
            match block_type {
                0x0A0D_0D0A => saw_shb = true,
                0x0000_0001 => saw_idb = true,
                0x0000_0006 => saw_epb = true,
                _ => {}
            }
            offset += block_len;
        }
        assert!(saw_shb && saw_idb && saw_epb, "missing SHB/IDB/EPB blocks");
    }

    fn live_debug_fixture(
        mcp_config: McpConfig,
        sentinel: &str,
        camera_toml: &str,
    ) -> (PathBuf, ServerState, FacadeSender<RouterMessage>) {
        let dir = std::env::temp_dir().join(format!("keeppeek-mcp-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        std::fs::write(
            dir.join("secrets.toml"),
            format!("CAMERA_PASSWORD = \"{sentinel}\"\nKEEPPEEK_ACCESS_KEY = \"550e8400-e29b-41d4-a716-446655440000\"\n"),
        )
        .unwrap();
        std::fs::write(
            &config_path,
            format!(
                "host = \"127.0.0.1\"\nport = 8081\naccess_key = \"{{secret:KEEPPEEK_ACCESS_KEY}}\"\n\n[mcp]\nenabled = true\nallow_camera_debugging = true\n\n[camera_defaults]\nusername = \"admin\"\npassword = \"{{secret:CAMERA_PASSWORD}}\"\n\n{camera_toml}"
            ),
        )
        .unwrap();
        let mut cfg = config::load_config(&config_path).unwrap();
        cfg.mcp = mcp_config;
        let cameras = cfg.camera_configurations().unwrap();
        let storage = crate::storage::StorageConfig {
            long_term_path: dir.join("recordings"),
            ..crate::storage::StorageConfig::default()
        };
        let state = ServerState::new(
            &cfg,
            &cameras,
            &HashMap::new(),
            &storage,
            crate::storage::RecordingDemand::new(Duration::from_secs(5)),
            crate::webrtc::WebRtc::new(),
        )
        .with_camera_config_path(config_path);
        let (_router, router_tx) = Router::new().unwrap();
        (dir, state, router_tx)
    }

    fn assert_capture_traces_readable(
        state: &ServerState,
        router_tx: &FacadeSender<RouterMessage>,
        capture_id: &str,
        sentinel: &str,
    ) {
        let read_trace = call_mcp_tool(
            state,
            router_tx,
            11,
            "camera_debugging",
            json!({
                "action": "read_artifact",
                "capture_id": capture_id,
                "artifact": "traces.jsonl",
                "offset": 0,
                "length": 65_536
            }),
        );
        let trace_sc = &read_trace["result"]["structuredContent"];
        assert_eq!(trace_sc["action"], "read_artifact");
        assert_eq!(trace_sc["eof"], true);
        assert_eq!(trace_sc["encoding"], "utf8");
        assert!(!trace_sc["data"].as_str().unwrap().contains(sentinel));
    }

    fn assert_rtsp_capture_and_inspect(
        state: &ServerState,
        router_tx: &FacadeSender<RouterMessage>,
        dir: &std::path::Path,
        sentinel: &str,
    ) -> String {
        let cap_resp = call_mcp_tool(
            state,
            router_tx,
            1,
            "camera_debugging",
            json!({
                "action": "capture",
                "camera_id": "127.0.0.1",
                "protocols": ["rtsp"],
                "duration_seconds": 1
            }),
        );
        let cap = &cap_resp["result"]["structuredContent"];
        let capture_id = cap["capture_id"].as_str().expect("capture_id").to_owned();
        let capture_dir = dir.join("camera-debug").join(&capture_id);
        assert!(capture_dir.is_dir());
        assert_valid_pcapng_blocks(&capture_dir.join("capture.pcapng"));
        for file in [
            "video.h264",
            "muxed.mp4",
            "protocol_transcript.json",
            "traces.jsonl",
            "capture.json",
        ] {
            assert!(capture_dir.join(file).is_file(), "missing {file}");
        }
        assert!(
            cap["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f == "traces.jsonl")
        );
        if cap["bitstream"]["audio_path"].is_string() {
            assert!(capture_dir.join("audio.aac").is_file());
        }
        assert_capture_traces_readable(state, router_tx, &capture_id, sentinel);

        let inspect = call_mcp_tool(
            state,
            router_tx,
            2,
            "camera_debugging",
            json!({ "action": "inspect_pipeline", "camera_id": "127.0.0.1" }),
        );
        assert_eq!(
            inspect["result"]["structuredContent"]["cameras"][0]["debug_metrics"]["capture_id"],
            capture_id
        );

        let exported = call_mcp_tool(
            state,
            router_tx,
            4,
            "camera_debugging",
            json!({ "action": "export_repro_fixture", "capture_id": capture_id }),
        );
        let exp = &exported["result"]["structuredContent"];
        assert_eq!(exp["fixture"]["schema_version"], "keeppeek.camera-repro.v1");
        assert!(exp["fixture"]["rtsp"]["sessions"].is_array());
        assert_eq!(exp["sharing_safety_report"]["safe_to_share_publicly"], true);
        assert!(!serde_json::to_string(exp).unwrap().contains(sentinel));
        capture_id
    }

    fn assert_rtsp_compare_list_delete_and_redaction(
        state: &ServerState,
        router_tx: &FacadeSender<RouterMessage>,
        dir: &std::path::Path,
        capture_id: &str,
        sentinel: &str,
    ) {
        let cmp = call_mcp_tool(
            state,
            router_tx,
            3,
            "camera_debugging",
            json!({
                "action": "compare_transports",
                "camera_id": "127.0.0.1",
                "mode": "media_transports",
                "duration_seconds": 1
            }),
        );
        let cmp_sc = &cmp["result"]["structuredContent"];
        assert_eq!(cmp_sc["recommended_transport"], "tcp");
        assert_eq!(cmp_sc["entries"].as_array().unwrap().len(), 2);

        let listed = call_mcp_tool(
            state,
            router_tx,
            5,
            "camera_debugging",
            json!({ "action": "list_captures", "camera_id": "127.0.0.1" }),
        );
        assert!(
            listed["result"]["structuredContent"]["count"]
                .as_u64()
                .unwrap()
                >= 2
        );
        let del_one = call_mcp_tool(
            state,
            router_tx,
            6,
            "camera_debugging",
            json!({ "action": "delete_capture", "capture_id": capture_id }),
        );
        assert_eq!(del_one["result"]["structuredContent"]["deleted"], 1);
        assert!(!dir.join("camera-debug").join(capture_id).exists());
        let del_all = call_mcp_tool(
            state,
            router_tx,
            7,
            "camera_debugging",
            json!({ "action": "delete_capture", "delete_all": true }),
        );
        assert!(
            del_all["result"]["structuredContent"]["deleted"]
                .as_u64()
                .unwrap()
                >= 1
        );

        for (id, tool, args) in [
            (8, "search_logs", json!({})),
            (9, "get_server_health", json!({})),
            (10, "get_configuration", json!({})),
        ] {
            let text =
                serde_json::to_string(&call_mcp_tool(state, router_tx, id, tool, args)).unwrap();
            assert!(!text.contains(sentinel), "{tool} leaked secret");
        }
    }

    #[test]
    fn camera_debugging_e2e_rtsp_capture_compare_export_list_delete_and_redaction() {
        let sentinel = "E2E-RTSP-SECRET-442111";
        let mp4 = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata/flash-tone-lead-200ms.mp4");
        let rtsp = retina::server::RtspServer::from_mp4(&mp4).unwrap();
        let cam_toml = format!(
            "[front.door]\nip = \"127.0.0.1\"\ndisplay_name = \"Front Door\"\nmain_rtsp_url = \"{}\"\n",
            rtsp.url()
        );
        let (dir, state, router_tx) = live_debug_fixture(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                request_timeout_seconds: 30,
                ..McpConfig::default()
            },
            sentinel,
            &cam_toml,
        );
        let (logging_service, dispatch) =
            LoggingService::for_test(LogFilterFile::new(dir.join("log-filter")), "info");
        let logging_service_ref = logging_service.clone();
        let state = state.with_logging(logging_service);
        let capture_id = tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(target: "keeppeek::test", "camera auth {sentinel}");
            assert_rtsp_capture_and_inspect(&state, &router_tx, &dir, sentinel)
        });
        assert_eq!(logging_service_ref.active_filter(), "info");
        assert_rtsp_compare_list_delete_and_redaction(
            &state,
            &router_tx,
            &dir,
            &capture_id,
            sentinel,
        );
        drop(rtsp);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn assert_isapi_repro_redaction(
        state: &ServerState,
        router_tx: &FacadeSender<RouterMessage>,
        raw_id: &str,
        sentinel: &str,
    ) {
        let safe_exp = call_mcp_tool(
            state,
            router_tx,
            2,
            "camera_debugging",
            json!({ "action": "export_repro_fixture", "capture_id": raw_id }),
        );
        let safe_sc = &safe_exp["result"]["structuredContent"];
        assert_eq!(
            safe_sc["sharing_safety_report"]["safe_to_share_publicly"],
            true
        );
        assert_eq!(safe_sc["fixture"]["camera"]["credentials_stripped"], true);
        assert_eq!(safe_sc["fixture"]["camera"]["anonymized"], true);
        assert_eq!(
            safe_sc["fixture"]["camera"]["base_host"],
            "camera-1.example.invalid"
        );
        let safe_text = serde_json::to_string(safe_sc).unwrap();
        let fixture_text = serde_json::to_string(&safe_sc["fixture"]).unwrap();
        assert!(!safe_text.contains(sentinel));
        assert!(!fixture_text.contains("127.0.0.1"));

        let raw_exp = call_mcp_tool(
            state,
            router_tx,
            3,
            "camera_debugging",
            json!({
                "action": "export_repro_fixture",
                "capture_id": raw_id,
                "strip_credentials": false,
                "anonymize_network": false
            }),
        );
        let raw_sc = &raw_exp["result"]["structuredContent"];
        assert_eq!(
            raw_sc["sharing_safety_report"]["safe_to_share_publicly"],
            false
        );
        assert_eq!(raw_sc["fixture"]["camera"]["credentials_stripped"], false);
        assert_eq!(raw_sc["fixture"]["camera"]["anonymized"], false);
        assert_eq!(raw_sc["fixture"]["camera"]["base_host"], "127.0.0.1");
        assert!(
            !raw_sc["sharing_safety_report"]["warnings"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    fn run_isapi_capture(
        state: &ServerState,
        router_tx: &FacadeSender<RouterMessage>,
        dir: &std::path::Path,
        id: u64,
        include_raw_auth: bool,
    ) -> (Value, String, String) {
        let resp = call_mcp_tool(
            state,
            router_tx,
            id,
            "camera_debugging",
            json!({
                "action": "capture",
                "camera_id": "127.0.0.1",
                "protocols": ["isapi"],
                "include_raw_auth": include_raw_auth,
                "duration_seconds": 1
            }),
        );
        let capture_id = resp["result"]["structuredContent"]["capture_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let cap_dir = dir.join("camera-debug").join(&capture_id);
        assert_valid_pcapng_blocks(&cap_dir.join("capture.pcapng"));
        let transcript = std::fs::read_to_string(cap_dir.join("protocol_transcript.json")).unwrap();
        (resp, capture_id, transcript)
    }

    #[test]
    fn camera_debugging_e2e_isapi_raw_auth_vs_stripped_repro() {
        let sentinel = "E2E-ISAPI-SECRET-442222";
        let device = test_hikvision::FakeHikvision::builder()
            .credentials("admin", sentinel)
            .start()
            .unwrap();
        let port = url::Url::parse(&device.origin())
            .unwrap()
            .port()
            .expect("fake hikvision port");
        let cam_toml = format!(
            "[isapi.cam]\nip = \"127.0.0.1\"\ndisplay_name = \"ISAPI Cam\"\nhttp_port = {port}\n"
        );
        let (dir, state, router_tx) = live_debug_fixture(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                ..McpConfig::default()
            },
            sentinel,
            &cam_toml,
        );

        let (_raw_cap, raw_id, raw_transcript) =
            run_isapi_capture(&state, &router_tx, &dir, 1, true);
        assert!(raw_transcript.contains("Digest username="));
        assert_isapi_repro_redaction(&state, &router_tx, &raw_id, sentinel);

        let (redacted_cap, _red_id, red_transcript) =
            run_isapi_capture(&state, &router_tx, &dir, 4, false);
        assert!(!red_transcript.contains("Digest username="));
        assert!(
            !serde_json::to_string(&redacted_cap)
                .unwrap()
                .contains(sentinel)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disable_mcp_access_works_without_allow_writes_disables_live_and_blocks_user_and_readonly() {
        let dir = std::env::temp_dir().join(format!(
            "keeppeek-mcp-self-disable-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(not(windows))]
        let dir = dir.canonicalize().unwrap();
        let config_path = dir.join("config.toml");
        let now_secs = unix_time_ms() / 1_000;
        let eight_days_ago = now_secs.saturating_sub(8 * 86_400);
        std::fs::write(
            &config_path,
            format!(
                "host = \"127.0.0.1\"\nport = 8081\n\n[mcp]\nenabled = true\nenabled_at_unix_secs = {eight_days_ago}\nallow_writes = false\nmax_concurrent_requests = 6\n"
            ),
        )
        .unwrap();
        std::fs::write(config::secrets_path(&config_path), "").unwrap();

        let loaded = config::load_config(&config_path).unwrap();
        let access_manager = AccessManager::ephemeral(loaded.access_key);
        let IssuedCredential {
            access_key: user_key,
            ..
        } = access_manager
            .create_credential("viewer", None, AccessRole::User, None, 1_000)
            .unwrap();
        let (mut state, router_tx) = state_with_mcp(loaded.mcp);
        state.camera_config_path = Some(config_path.clone());
        state = state.with_access_manager(access_manager);

        // 1. 8-day enabled MCP emits a warning HealthIssue with scope "mcp".
        let health = server_health(&router_tx, &state);
        assert!(
            health
                .issues
                .iter()
                .any(|issue| issue.scope == "mcp" && issue.severity == "warning"),
            "expected mcp 7-day warning in health issues: {:?}",
            health.issues
        );

        // 1b. Non-admin User health omits scope "mcp" and does not report degraded status from it.
        let user_health_req = Request::fake_https_from(
            "198.51.100.10:50000".parse().unwrap(),
            "POST",
            "/mcp",
            vec![
                ("Host".to_owned(), "127.0.0.1:8081".to_owned()),
                ("Content-Type".to_owned(), "application/json".to_owned()),
                (
                    "Authorization".to_owned(),
                    format!("Bearer {}", user_key.canonical()),
                ),
            ],
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 99,
                "method": "tools/call",
                "params": { "name": "get_server_health", "arguments": {} }
            }))
            .unwrap(),
        );
        let user_health_body =
            read_response_json(handle_http(&user_health_req, &router_tx, &state));
        let user_health = &user_health_body["result"]["structuredContent"];
        assert!(
            !user_health["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| issue["scope"] == "mcp")
        );

        let mut mcp_only_health = health;
        mcp_only_health.issues.retain(|issue| issue.scope == "mcp");
        mcp_only_health.status = server_health_status(&mcp_only_health.issues).to_owned();
        assert_eq!(mcp_only_health.status, "degraded");
        filter_health_for_user(
            &mut mcp_only_health,
            &CameraAccess {
                all_cameras: true,
                ..CameraAccess::default()
            },
        );
        assert!(mcp_only_health.issues.is_empty());
        assert_eq!(mcp_only_health.status, "healthy");

        // 2. Non-admin User credential cannot call disable_mcp_access.
        let disable_call = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "disable_mcp_access",
                "arguments": { "confirm": true, "reason": "Finished maintenance" }
            }
        });
        let user_req = Request::fake_https_from(
            "198.51.100.10:50000".parse().unwrap(),
            "POST",
            "/mcp",
            vec![
                ("Host".to_owned(), "127.0.0.1:8081".to_owned()),
                ("Content-Type".to_owned(), "application/json".to_owned()),
                (
                    "Authorization".to_owned(),
                    format!("Bearer {}", user_key.canonical()),
                ),
            ],
            serde_json::to_vec(&disable_call).unwrap(),
        );
        let user_resp = handle_http(&user_req, &router_tx, &state);
        assert_eq!(user_resp.status_code, 200);
        let user_body = read_response_json(user_resp);
        assert_eq!(user_body["error"]["code"], ERROR_FORBIDDEN);
        assert!(state.mcp_enabled());

        // 3. Read-only header blocks disable_mcp_access.
        let ro_resp = handle_http(
            &mcp_post_request(
                &disable_call,
                vec![(READ_ONLY_HEADER.to_owned(), "true".to_owned())],
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        let ro_body = read_response_json(ro_resp);
        assert_eq!(ro_body["error"]["code"], ERROR_FORBIDDEN);
        assert!(state.mcp_enabled());

        // 4. confirm = false is rejected with ERROR_INVALID_PARAMS.
        let unconfirmed_call = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "disable_mcp_access",
                "arguments": { "confirm": false }
            }
        });
        let bad_resp = handle_http(
            &mcp_post_request(&unconfirmed_call, Vec::new(), "127.0.0.1:50000"),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(bad_resp)["error"]["code"],
            ERROR_INVALID_PARAMS
        );

        // 5. Create a pending secret-entry handoff, then disable MCP even with allow_writes = false.
        let handoff_call = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "request_secret_entry",
                "arguments": {
                    "keys": [{ "key": "CAM_PASS", "purpose": "Camera password" }]
                }
            }
        });
        let handoff_resp = read_response_json(handle_http(
            &mcp_post_request(&handoff_call, Vec::new(), "127.0.0.1:50000"),
            &router_tx,
            &state,
        ));
        let entry_url = handoff_resp["result"]["structuredContent"]["local_ui_url"]
            .as_str()
            .expect("local_ui_url must be returned")
            .to_owned();
        let token = entry_url
            .split("token=")
            .nth(1)
            .expect("token query param")
            .to_owned();

        let ok_resp = handle_http(
            &mcp_post_request(&disable_call, Vec::new(), "127.0.0.1:50000"),
            &router_tx,
            &state,
        );
        assert_eq!(ok_resp.status_code, 200);
        let ok_body = read_response_json(ok_resp);
        assert_eq!(ok_body["result"]["structuredContent"]["disabled"], true);
        assert_eq!(
            ok_body["result"]["structuredContent"]["effective_immediately"],
            true
        );

        // 6. Persisted config has enabled = false, cleared timestamp & silence, preserved max_concurrent_requests.
        let persisted = config::load_config(&config_path).unwrap();
        assert!(!persisted.mcp.enabled);
        assert_eq!(persisted.mcp.enabled_at_unix_secs, None);
        assert!(!persisted.mcp.long_enabled_warning_silenced);
        assert_eq!(persisted.mcp.max_concurrent_requests, 6);

        // 7. Very next POST /mcp and GET /mcp/secret-entry immediately return 404.
        let next_post = handle_http(
            &mcp_post_request(&disable_call, Vec::new(), "127.0.0.1:50000"),
            &router_tx,
            &state,
        );
        assert_eq!(next_post.status_code, 404);
        let next_handoff = mcp_secret_handoff::handle_get(
            &Request::fake_http(
                "GET",
                format!("/mcp/secret-entry?token={token}"),
                vec![("Host".to_owned(), "127.0.0.1:8081".to_owned())],
                Vec::new(),
            ),
            &state,
        );
        assert_eq!(next_handoff.status_code, 404);

        // 8. Health issues no longer include the mcp warning after disable.
        let health_after = server_health(&router_tx, &state);
        assert!(
            !health_after.issues.iter().any(|issue| issue.scope == "mcp"),
            "mcp warning must disappear once disabled"
        );

        // 9. No enable_mcp_access or silence tool exists in the catalog.
        let admin_tools = mcp_schemas::tool_definitions(true);
        assert!(
            !admin_tools
                .iter()
                .any(|t| t.name.contains("enable_mcp") || t.name.contains("silence"))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_camera_span_flows_to_search_logs_and_sse_route() {
        let dir =
            std::env::temp_dir().join(format!("keeppeek-mcp-span-logs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut state, router_tx) = state_with_mcp(McpConfig {
            enabled: true,
            ..McpConfig::default()
        });
        let (logging, dispatch) = LoggingService::for_test(
            LogFilterFile::new(dir.join("log-filter")),
            "info,keeppeek=debug",
        );
        let logging_ref = logging.clone();
        state = state.with_logging(logging);

        tracing::dispatcher::with_default(&dispatch, || {
            let span = tracing::info_span!("camera", camera_id = "cam-1", stream = "main");
            let _guard = span.enter();
            tracing::info!(target: "keeppeek::rtsp", "stream started");
        });

        let search_resp = call_mcp_tool(
            &state,
            &router_tx,
            1,
            "search_logs",
            json!({ "camera": "cam-1" }),
        );
        let entries = search_resp["result"]["structuredContent"]["entries"]
            .as_array()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["fields"]["camera_id"], "cam-1");
        assert_eq!(entries[0]["fields"]["stream"], "main");

        let sse_req = Request::fake_http("GET", "/logs?tail=10", Vec::new(), Vec::new());
        let sse_resp = super::super::handle_request(&sse_req, &router_tx, &state);
        assert_eq!(sse_resp.status_code, 200);
        logging_ref.close_streams();
        let (mut reader, _) = sse_resp.data.into_reader_and_size();
        let mut sse_body = String::new();
        reader.read_to_string(&mut sse_body).unwrap();
        assert!(sse_body.contains("\"camera_id\":\"cam-1\""));
        assert!(sse_body.contains("\"stream\":\"main\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn seed_test_capture_artifact(
        store: &crate::camera_debug::store::CaptureStore,
        cap_id: &str,
        cam_id: &str,
        artifact: &str,
        bytes: &[u8],
    ) {
        let cap_dir = store.capture_dir(cap_id).unwrap();
        std::fs::create_dir_all(&cap_dir).unwrap();
        std::fs::write(cap_dir.join(artifact), bytes).unwrap();
        store
            .save_metadata(
                cap_id,
                &crate::camera_debug::store::CaptureMetadata {
                    capture_id: cap_id.to_owned(),
                    camera_id: cam_id.to_owned(),
                    protocol: "rtsp".to_owned(),
                    created_at_unix_secs: 1,
                    duration_ms: 10,
                    total_bytes: bytes.len() as u64,
                    packet_count: 1,
                    include_raw_auth: true,
                    trigger_probe: false,
                    tls_decrypted: false,
                    transport: "tcp".to_owned(),
                    files: vec![artifact.to_owned()],
                    anomalies: Default::default(),
                    warnings: Vec::new(),
                },
            )
            .unwrap();
    }

    #[test]
    fn read_artifact_http_redaction_and_large_chunk_boundary() {
        let sentinel = "READ-ARTIFACT-SENTINEL-665544";
        let (dir, state, router_tx) = live_debug_fixture(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                debug_camera_ids: vec!["192.0.2.10".to_owned()],
                ..McpConfig::default()
            },
            sentinel,
            "[front.door]\nip = \"192.0.2.10\"\n\n[back.yard]\nip = \"192.0.2.11\"\n",
        );
        let store = crate::camera_debug::store::CaptureStore::new(
            dir.join("camera-debug"),
            crate::camera_debug::store::StoreLimits::default(),
        )
        .unwrap();
        let big = format!("{sentinel}\n{}", "x".repeat(65_536));
        seed_test_capture_artifact(
            &store,
            "cap-allowed",
            "192.0.2.10",
            "traces.jsonl",
            big.as_bytes(),
        );
        seed_test_capture_artifact(
            &store,
            "cap-blocked",
            "192.0.2.11",
            "traces.jsonl",
            big.as_bytes(),
        );

        let ok_chunk = call_mcp_tool(
            &state,
            &router_tx,
            1,
            "camera_debugging",
            json!({
                "action": "read_artifact",
                "capture_id": "cap-allowed",
                "artifact": "traces.jsonl",
                "offset": 0,
                "length": 65_536
            }),
        );
        assert!(ok_chunk["error"].is_null());
        assert_eq!(ok_chunk["result"]["structuredContent"]["length"], 65_536);
        assert!(
            ok_chunk["result"]["structuredContent"]["data"]
                .as_str()
                .unwrap()
                .contains(sentinel)
        );

        let blocked = call_mcp_tool(
            &state,
            &router_tx,
            2,
            "camera_debugging",
            json!({ "action": "read_artifact", "capture_id": "cap-blocked", "artifact": "traces.jsonl" }),
        );
        assert_eq!(blocked["error"]["code"], ERROR_FORBIDDEN);
        assert!(!serde_json::to_string(&blocked).unwrap().contains(sentinel));

        let cfg = call_mcp_tool(&state, &router_tx, 3, "get_configuration", json!({}));
        assert!(!serde_json::to_string(&cfg).unwrap().contains(sentinel));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_artifact_http_control_byte_chunk_fits_json_rpc_budget() {
        let (dir, state, router_tx) = live_debug_fixture(
            McpConfig {
                enabled: true,
                allow_camera_debugging: true,
                debug_camera_ids: vec!["192.0.2.10".to_owned()],
                ..McpConfig::default()
            },
            "READ-ARTIFACT-SENTINEL-665544",
            "[front.door]\nip = \"192.0.2.10\"\n",
        );
        let store = crate::camera_debug::store::CaptureStore::new(
            dir.join("camera-debug"),
            crate::camera_debug::store::StoreLimits::default(),
        )
        .unwrap();
        let ctrl_bytes = vec![0x01_u8; 131_072];
        seed_test_capture_artifact(&store, "cap-ctrl", "192.0.2.10", "ctrl.bin", &ctrl_bytes);

        let auto_resp = call_mcp_tool(
            &state,
            &router_tx,
            1,
            "camera_debugging",
            json!({
                "action": "read_artifact",
                "capture_id": "cap-ctrl",
                "artifact": "ctrl.bin",
                "offset": 0,
                "length": 131_072,
                "encoding": "auto"
            }),
        );
        assert!(auto_resp["error"].is_null(), "{auto_resp}");
        assert_eq!(
            auto_resp["result"]["structuredContent"]["encoding"],
            "base64"
        );
        assert_eq!(auto_resp["result"]["structuredContent"]["length"], 131_072);
        assert_eq!(auto_resp["result"]["structuredContent"]["eof"], true);

        let utf8_resp = call_mcp_tool(
            &state,
            &router_tx,
            2,
            "camera_debugging",
            json!({
                "action": "read_artifact",
                "capture_id": "cap-ctrl",
                "artifact": "ctrl.bin",
                "offset": 0,
                "length": 131_072,
                "encoding": "utf8"
            }),
        );
        assert!(utf8_resp["error"].is_null(), "{utf8_resp}");
        let sc = &utf8_resp["result"]["structuredContent"];
        let clamped_len = sc["length"].as_u64().unwrap();
        assert!(clamped_len > 0 && clamped_len < 131_072);
        assert_eq!(sc["encoding"], "utf8");
        assert_eq!(sc["eof"], false);
        assert_eq!(sc["next_offset"], clamped_len);
        assert!(serde_json::to_vec(&utf8_resp).unwrap().len() <= READ_ARTIFACT_MAX_RESPONSE_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
