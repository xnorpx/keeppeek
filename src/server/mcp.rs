//! Embedded Streamable HTTP MCP route and core KeepPeek MCP tools.
//!
//! `POST /mcp` is disabled by default and returns `404` unless `[mcp].enabled`
//! is `true`. Every request passes through Host/Origin checks, KeepPeek access
//! control, bounded request/response budgets, RAII concurrency limits, and
//! serialization-layer secret redaction.

use super::{
    AuthenticatedApiRequest, CameraSettingsUpdate, ControlCommandError,
    MAX_NOTIFICATION_RULE_JSON_BYTES, ServerState, api_status, authenticated_api_request,
    camera_access, camera_configuration_revision, configuration, current_config,
    delete_camera_settings, get_logging_settings, logging, mcp_diagnostics, mcp_schemas,
    mcp_snapshot, notification_command_error, proto_notification_rule_record, record_access_audit,
    restore_notification_destinations, runtime_configuration, save_camera_settings, server_health,
    service_error, state_store, unix_time_ms, validate_client_id,
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
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use url::{Host, Url};

const DEFAULT_LOG_SEARCH_LIMIT: usize = 50;
const MAX_LOG_SEARCH_LIMIT: usize = 200;
const PROFILE_ACTIVE_SCHEMA: &str = "keeppeek.runtime-profiles.active.v1";
const PROFILE_SELECT_SCHEMA: &str = "keeppeek.runtime-profiles.select.v2";

/// Server-owned MCP runtime state, concurrency tracker, and redaction snapshot.
#[derive(Clone)]
pub(super) struct Runtime {
    config: McpConfig,
    active_requests: Arc<AtomicUsize>,
    secrets: Arc<RwLock<Secrets>>,
    snapshot_limiter: Arc<mcp_snapshot::SnapshotRateLimiter>,
}

impl Runtime {
    pub(super) fn new(config: McpConfig, secrets: Secrets) -> Self {
        Self {
            config,
            active_requests: Arc::new(AtomicUsize::new(0)),
            secrets: Arc::new(RwLock::new(secrets)),
            snapshot_limiter: Arc::new(mcp_snapshot::SnapshotRateLimiter::new()),
        }
    }

    pub(super) const fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub(super) const fn config(&self) -> &McpConfig {
        &self.config
    }

    pub(super) fn snapshot_limiter(&self) -> &mcp_snapshot::SnapshotRateLimiter {
        &self.snapshot_limiter
    }

    fn secrets_snapshot(&self) -> Secrets {
        self.secrets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn refresh_secrets(&self, secrets: Secrets) {
        *self
            .secrets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = secrets;
    }

    pub(super) fn try_acquire(&self) -> Option<RequestGuard> {
        let limit = self.config.max_concurrent_requests;
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
    pub(super) deadline: Instant,
}

impl McpContext<'_> {
    pub(super) const fn is_admin(&self) -> bool {
        matches!(self.identity.principal.role, AccessRole::Administrator)
    }

    pub(super) const fn writes_allowed(&self) -> bool {
        self.is_admin() && self.state.mcp.config().allow_writes && !self.read_only_header
    }

    pub(super) const fn snapshots_allowed(&self) -> bool {
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

fn handle_authenticated_post(
    request: &Request,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
) -> Response {
    if identity.principal.is_local() {
        if let Err(response) = validate_local_host_header(request) {
            return response;
        }
    } else if identity.principal.credential_binding().is_none() {
        return api_status(401, "MCP requires Bearer authentication")
            .with_additional_header("Cache-Control", "no-store");
    }
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
        return json_rpc_http_response(state, 429, &busy, state.mcp.config().max_response_bytes)
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
            );
        }
    };
    let response_limit = response_byte_limit(state.mcp.config(), &parsed);
    let ctx = McpContext {
        state,
        router_tx,
        identity,
        read_only_header: read_only_header_enabled(request),
        deadline: Instant::now() + Duration::from_secs(state.mcp.config().request_timeout_seconds),
    };
    dispatch_json_rpc(&ctx, parsed).map_or_else(
        || {
            Response::empty_204()
                .with_status_code(202)
                .with_additional_header("Cache-Control", "no-store")
        },
        |rpc_response| json_rpc_http_response(state, 200, &rpc_response, response_limit),
    )
}

fn response_byte_limit(config: &McpConfig, request: &JsonRpcRequest) -> usize {
    let is_snapshot = request.method == "tools/call"
        && request
            .params
            .as_ref()
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            == Some("get_camera_snapshot");
    if is_snapshot {
        // Snapshots use max_snapshot_bytes plus base64 expansion and JSON metadata envelope.
        let base64_bytes = config.max_snapshot_bytes.div_ceil(3).saturating_mul(4);
        config
            .max_response_bytes
            .max(base64_bytes.saturating_add(4_096))
    } else {
        config.max_response_bytes
    }
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
    // Use the in-memory secrets snapshot so hot responses never touch disk or chmod secrets.toml.
    let secrets = state.mcp.secrets_snapshot();
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
) -> Response {
    let redactor = build_redactor(state);
    let mut value = serde_json::to_value(response).unwrap_or_else(|_| {
        json!({
            "jsonrpc": "2.0",
            "id": response.id,
            "error": { "code": ERROR_INTERNAL, "message": "failed to serialize MCP response" }
        })
    });
    redactor.redact_value(&mut value);
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

fn dispatch_json_rpc(ctx: &McpContext<'_>, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
    if request.is_notification() {
        return None;
    }
    let id = request.id.unwrap_or(Value::Null);
    if let Err(error) = ctx.check_deadline() {
        return Some(JsonRpcResponse::error(id, error.into_json_rpc()));
    }
    let outcome = match request.method.as_str() {
        "initialize" => Ok(serde_json::to_value(InitializeResult::keeppeek(
            ctx.writes_allowed(),
            ctx.snapshots_allowed(),
        ))
        .unwrap_or(Value::Null)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": mcp_schemas::tool_definitions(ctx.is_admin()) })),
        "tools/call" => handle_tools_call(ctx, request.params),
        method => Err(ToolError {
            code: ERROR_METHOD_NOT_FOUND,
            message: format!("unsupported MCP method: {method}"),
            data: None,
        }),
    };
    let final_outcome = match outcome {
        Ok(value) => ctx.check_deadline().map(|()| value),
        Err(error) => Err(error),
    };
    match final_outcome {
        Ok(result) => Some(JsonRpcResponse::success(id, result)),
        Err(error) => Some(JsonRpcResponse::error(id, error.into_json_rpc())),
    }
}

fn handle_tools_call(ctx: &McpContext<'_>, params: Option<Value>) -> Result<Value, ToolError> {
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
    let result = dispatch_tool(ctx, &call.name, arguments)?;
    serde_json::to_value(result)
        .map_err(|error| ToolError::internal(format!("failed to encode tool result: {error}")))
}

fn dispatch_tool(
    ctx: &McpContext<'_>,
    name: &str,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    match name {
        "get_server_health" => tool_get_server_health(ctx),
        "search_events" => dispatch_diagnostic(
            ctx,
            "mcp_search_events",
            mcp_diagnostics::search_events,
            &args,
        ),
        "get_recording_coverage" => dispatch_diagnostic(
            ctx,
            "mcp_get_recording_coverage",
            mcp_diagnostics::get_recording_coverage,
            &args,
        ),
        "get_camera_snapshot" => tool_get_camera_snapshot(ctx, args),
        "search_logs" => tool_search_logs(ctx, args),
        "get_logging_settings" => tool_get_logging_settings(ctx),
        "get_configuration" => tool_get_configuration(ctx, args),
        "list_secret_keys" => tool_list_secret_keys(ctx),
        "plan_configuration_change" => tool_plan_configuration_change(ctx, args),
        "get_storage_status" => dispatch_diagnostic(
            ctx,
            "mcp_get_storage_status",
            mcp_diagnostics::get_storage_status,
            &args,
        ),
        "get_notification_status" => dispatch_diagnostic(
            ctx,
            "mcp_get_notification_status",
            mcp_diagnostics::get_notification_status,
            &args,
        ),
        "discover_cameras" => dispatch_diagnostic(
            ctx,
            "mcp_discover_cameras",
            mcp_diagnostics::discover_cameras,
            &args,
        ),
        "get_runtime_configuration" => dispatch_diagnostic(
            ctx,
            "mcp_get_runtime_configuration",
            mcp_diagnostics::get_runtime_configuration,
            &args,
        ),
        "list_runtime_profiles" => dispatch_diagnostic(
            ctx,
            "mcp_list_runtime_profiles",
            mcp_diagnostics::list_runtime_profiles,
            &args,
        ),
        "apply_configuration_plan" => tool_apply_configuration_plan(ctx, args),
        "update_runtime_configuration" => tool_update_runtime_configuration(ctx, args),
        "manage_configuration_template" => tool_manage_configuration_template(ctx, args),
        "manage_camera" => tool_manage_camera(ctx, args),
        "manage_notification_rule" => tool_manage_notification_rule(ctx, args),
        "set_runtime_profile" => tool_set_runtime_profile(ctx, args),
        "set_logging_filter" => tool_set_logging_filter(ctx, args),
        other => Err(ToolError {
            code: ERROR_METHOD_NOT_FOUND,
            message: format!("unknown MCP tool: {other}"),
            data: None,
        }),
    }
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
    if policy.all_cameras {
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
    let min_rank = args.level.map(log_level_rank);
    let target_needle = args.target.as_deref().map(str::to_ascii_lowercase);
    let camera_needle = args.camera.as_deref().map(str::to_ascii_lowercase);
    let query_needle = args.query.as_deref().map(str::to_ascii_lowercase);
    let mut matched = snapshot
        .entries
        .into_iter()
        .filter(|entry| {
            min_rank.is_none_or(|rank| log_level_rank(entry.level) >= rank)
                && args
                    .since_timestamp_ms
                    .is_none_or(|since| entry.timestamp_ms >= since)
                && target_needle
                    .as_deref()
                    .is_none_or(|needle| entry.target.to_ascii_lowercase().contains(needle))
                && camera_needle
                    .as_deref()
                    .is_none_or(|needle| log_entry_matches(entry, needle))
                && query_needle
                    .as_deref()
                    .is_none_or(|needle| log_entry_matches(entry, needle))
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

const fn log_level_rank(level: LogLevel) -> u8 {
    match level {
        LogLevel::Trace => 0,
        LogLevel::Debug => 1,
        LogLevel::Info => 2,
        LogLevel::Warn => 3,
        LogLevel::Error => 4,
    }
}

fn log_entry_matches(entry: &crate::logging::LogEntry, needle: &str) -> bool {
    if entry.target.to_ascii_lowercase().contains(needle)
        || entry.message.to_ascii_lowercase().contains(needle)
    {
        return true;
    }
    entry.fields.iter().any(|(key, value)| {
        key.to_ascii_lowercase().contains(needle)
            || value.to_string().to_ascii_lowercase().contains(needle)
    })
}

fn tool_get_logging_settings(ctx: &McpContext<'_>) -> Result<ToolCallResult, ToolError> {
    ctx.require_admin()?;
    let settings = get_logging_settings(ctx.state).map_err(ToolError::from_control)?;
    ctx.audit("mcp_get_logging_settings", None, "success");
    let value = serde_json::to_value(&settings).map_err(|error| {
        ToolError::internal(format!("failed to encode logging settings: {error}"))
    })?;
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
        ctx.state.mcp.refresh_secrets(secrets);
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
                ctx.state.mcp.refresh_secrets(secrets);
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

fn validate_rtsp_url_secret_policy(url: &str, field: &str) -> Result<(), ToolError> {
    let Some(scheme_pos) = url.find("://") else {
        return Ok(());
    };
    let authority_start = scheme_pos + 3;
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |offset| authority_start + offset);
    let authority = &url[authority_start..authority_end];
    let Some(at_pos) = authority.rfind('@') else {
        return Ok(());
    };
    let userinfo = &authority[..at_pos];
    let uses_ref = config::is_secret_reference(userinfo)
        || userinfo
            .split_once(':')
            .is_some_and(|(user, pass)| !user.is_empty() && config::is_secret_reference(pass));
    if uses_ref {
        Ok(())
    } else {
        Err(ToolError::invalid_params(format!(
            "{field} must not contain plaintext credentials; use {{secret:KEY|url}}"
        )))
    }
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
            state.mcp.refresh_secrets(secrets);
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
        main_rtsp_url: input.main_rtsp_url.map(to_string_update).transpose()?,
        sub_rtsp_url: input.sub_rtsp_url.map(to_string_update).transpose()?,
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
        let allowlisted_origin = mcp_post_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            vec![("Origin".into(), "http://127.0.0.1:8081".into())],
            "127.0.0.1:50000",
        );
        assert_eq!(
            super::super::handle_request(&allowlisted_origin, &router_tx, &allowlisted_state)
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
        assert_eq!(tools.len(), 21);
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
            "apply_configuration_plan",
            "update_runtime_configuration",
            "manage_configuration_template",
            "manage_camera",
            "manage_notification_rule",
            "set_runtime_profile",
            "set_logging_filter",
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
            MediaFrame, RecordingFrame, StorageConfig, StorageEngine, VideoCodec, VideoFrame,
            nal::annexb_to_avcc,
        };
        use std::sync::atomic::AtomicBool;
        use test_frame::{EncodedFrame, TestFrameConfig, TestFrameSource};

        const WARMUP_TRIALS: usize = 3;
        const MEASURED_TRIALS: usize = 30;
        const FRAMES_PER_TRIAL: usize = 30;

        let (tx, rx) = std::sync::mpsc::sync_channel::<EncodedFrame>(FRAMES_PER_TRIAL + 8);
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
        let mut frames = Vec::with_capacity(FRAMES_PER_TRIAL);
        while frames.len() < FRAMES_PER_TRIAL {
            frames.push(
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

        let run_phase = |name: &str, with_mcp_load: bool| -> (Vec<f64>, u64, Vec<u64>, usize) {
            let phase_dir = dir.join(name);
            std::fs::create_dir_all(&phase_dir).unwrap();
            let engine = StorageEngine::start(StorageConfig {
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
            });
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
                        while !worker_stop.load(Ordering::Acquire) || iter < 12 {
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
                            if worker_stop.load(Ordering::Acquire) && iter >= 12 {
                                break;
                            }
                        }
                        (latencies, max_bytes)
                    }));
                }
            }

            let mut trial_us = Vec::with_capacity(MEASURED_TRIALS);
            let mut gaps = 0u64;
            for trial in 0..(WARMUP_TRIALS + MEASURED_TRIALS) {
                let started = Instant::now();
                let mut previous = started;
                for frame in &frames {
                    let now = Instant::now();
                    if now.duration_since(previous) > Duration::from_millis(250) {
                        gaps += 1;
                    }
                    previous = now;
                    engine.ingest(
                        "192.0.2.10",
                        RecordingFrame {
                            source_rtp: None,
                            received_at: now,
                            timestamp: None,
                            frame: MediaFrame::Video(VideoFrame {
                                codec: VideoCodec::H264,
                                is_keyframe: frame.is_keyframe,
                                width: frame.width,
                                height: frame.height,
                                data: annexb_to_avcc(&frame.data).into(),
                            }),
                        },
                    );
                }
                let us_per_frame =
                    started.elapsed().as_secs_f64() * 1_000_000.0 / frames.len() as f64;
                if trial >= WARMUP_TRIALS {
                    trial_us.push(us_per_frame);
                }
            }

            stop.store(true, Ordering::Release);
            let mut latencies = Vec::new();
            let mut max_bytes = 0usize;
            for worker in mcp_workers {
                let (worker_latencies, worker_max) = worker.join().unwrap();
                latencies.extend(worker_latencies);
                max_bytes = max_bytes.max(worker_max);
            }
            engine.shutdown();
            (trial_us, gaps, latencies, max_bytes)
        };

        let (mut base_us, base_gaps, _, _) = run_phase("baseline", false);
        let (mut active_us, active_gaps, mut mcp_latencies_us, max_response_bytes) =
            run_phase("active", true);
        base_us.sort_by(f64::total_cmp);
        active_us.sort_by(f64::total_cmp);
        mcp_latencies_us.sort_unstable();

        let base_p50 = base_us[base_us.len() / 2];
        let base_p95 = base_us[(base_us.len() * 95) / 100];
        let active_p50 = active_us[active_us.len() / 2];
        let active_p95 = active_us[(active_us.len() * 95) / 100];
        let mcp_count = mcp_latencies_us.len();
        let mcp_p50_ms = mcp_latencies_us[mcp_count / 2] as f64 / 1000.0;
        let mcp_p95_ms = mcp_latencies_us[(mcp_count * 95) / 100] as f64 / 1000.0;
        let mcp_max_ms = *mcp_latencies_us.last().unwrap() as f64 / 1000.0;
        eprintln!(
            "AC-8 STORAGE ENGINE INGEST METRICS: trials={MEASURED_TRIALS} frames_per_trial={FRAMES_PER_TRIAL} base_p50_us={base_p50:.2} active_p50_us={active_p50:.2} base_p95_us={base_p95:.2} active_p95_us={active_p95:.2} base_gaps={base_gaps} active_gaps={active_gaps} mcp_calls={mcp_count} mcp_p50_ms={mcp_p50_ms:.3} mcp_p95_ms={mcp_p95_ms:.3} mcp_max_ms={mcp_max_ms:.3} max_response_bytes={max_response_bytes}"
        );
        assert_eq!(
            active_gaps, 0,
            "MCP load must not induce recording ingest gaps"
        );
        assert!(mcp_count >= 36);
        assert!(max_response_bytes <= state.mcp.config().max_response_bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
