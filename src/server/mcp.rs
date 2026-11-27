//! Embedded Streamable HTTP MCP route and core KeepPeek MCP tools.
//!
//! `POST /mcp` is disabled by default and returns `404` unless `[mcp].enabled`
//! is `true`. Every request passes through Host/Origin checks, KeepPeek access
//! control, bounded request/response budgets, RAII concurrency limits, and
//! serialization-layer secret redaction.

use super::{
    AuthenticatedApiRequest, ControlCommandError, ServerState, api_status,
    authenticated_api_request, camera_access, configuration, current_config, get_logging_settings,
    logging, record_access_audit, request_origin, runtime_configuration, server_health,
    service_error, state_store, unix_time_ms,
};
use crate::{
    access::AccessRole,
    api::proto,
    config::{self, mcp::McpConfig},
    health::{CameraHealthState, HealthTotals, ServerHealthResponse},
    logging::LogLevel,
    mcp::{
        protocol::{
            ERROR_BUSY, ERROR_FORBIDDEN, ERROR_INTERNAL, ERROR_INVALID_PARAMS,
            ERROR_METHOD_NOT_FOUND, ERROR_RESPONSE_TOO_LARGE, ERROR_TIMEOUT, InitializeResult,
            JsonRpcError, JsonRpcRequest, JsonRpcResponse, MAX_REQUEST_BODY_BYTES,
            READ_ONLY_HEADER, ToolAnnotations, ToolCallParams, ToolCallResult, ToolDefinition,
            parse_bounded_request,
        },
        redaction::SecretRedactor,
    },
    runtime::{FacadeSender, RouterMessage},
    webrtc::SessionId,
};
use prost_types::{Struct, Value as ProstValue, value::Kind};
use rouille::{Request, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;

const DEFAULT_LOG_SEARCH_LIMIT: usize = 50;
const MAX_LOG_SEARCH_LIMIT: usize = 200;
const PROFILE_ACTIVE_SCHEMA: &str = "keeppeek.runtime-profiles.active.v1";
const PROFILE_SELECT_SCHEMA: &str = "keeppeek.runtime-profiles.select.v2";

/// Server-owned MCP runtime state and concurrency tracker.
#[derive(Clone)]
pub(super) struct Runtime {
    config: McpConfig,
    active_requests: Arc<AtomicUsize>,
}

impl Runtime {
    pub(super) fn new(config: McpConfig) -> Self {
        Self {
            config,
            active_requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(super) const fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub(super) const fn config(&self) -> &McpConfig {
        &self.config
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
    let same_origin = match validate_host_and_origin(request) {
        Ok(origin) => origin,
        Err(response) => return response,
    };
    let response = authenticated_api_request(request, state, false, AccessRole::User, |identity| {
        handle_authenticated_post(request, router_tx, state, &identity)
    });
    with_same_origin_header(response, same_origin)
}

/// Handles `OPTIONS /mcp` preflight checks when MCP is enabled.
pub(super) fn handle_options(request: &Request, state: &ServerState) -> Response {
    if !state.mcp_enabled() {
        return service_error(404, "not found");
    }
    let same_origin = match validate_host_and_origin(request) {
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
    with_same_origin_header(response, same_origin)
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
    if !identity.principal.is_local() && identity.principal.credential_binding().is_none() {
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
        return json_rpc_http_response(state, 429, &busy)
            .with_additional_header("Retry-After", "1");
    };
    let parsed = match parse_bounded_request(&body, MAX_REQUEST_BODY_BYTES) {
        Ok(parsed) => parsed,
        Err(error_response) => return json_rpc_http_response(state, 200, error_response.as_ref()),
    };
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
        |rpc_response| json_rpc_http_response(state, 200, &rpc_response),
    )
}

fn validate_host_and_origin(request: &Request) -> Result<Option<String>, Response> {
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
    let Some(origin) = request.header("Origin") else {
        return Ok(None);
    };
    if request_origin(request).as_deref() == Some(origin) {
        Ok(Some(origin.to_owned()))
    } else {
        Err(api_status(
            403,
            "cross-origin browser requests are not allowed on /mcp",
        ))
    }
}

fn with_same_origin_header(response: Response, origin: Option<String>) -> Response {
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
    let secrets = state
        .camera_config_path
        .as_deref()
        .and_then(|path| config::load_secrets(path).ok())
        .unwrap_or_default();
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
) -> Response {
    let redactor = build_redactor(state);
    let max_bytes = state.mcp.config().max_response_bytes;
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
        "tools/list" => Ok(json!({ "tools": tool_definitions(ctx) })),
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

fn tool_definitions(ctx: &McpContext<'_>) -> Vec<ToolDefinition> {
    let mut tools = vec![tool_def(
        "get_server_health",
        "Read canonical KeepPeek server and camera health (filtered to permitted cameras for User role).",
        json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ToolAnnotations::read_only(),
    )];
    if !ctx.is_admin() {
        return tools;
    }
    tools.extend(admin_read_tool_definitions());
    tools.extend(admin_write_tool_definitions());
    tools
}

fn admin_read_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_def(
            "search_logs",
            "Search the bounded redacted server log buffer by level, target, camera, query, sequence, or timestamp.",
            json!({
                "type": "object",
                "properties": {
                    "level": { "type": "string", "enum": ["trace", "debug", "info", "warn", "error"] },
                    "target": { "type": "string" },
                    "camera": { "type": "string" },
                    "query": { "type": "string" },
                    "since_sequence": { "type": "integer", "minimum": 0 },
                    "since_timestamp_ms": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "get_logging_settings",
            "Read the active log filter directive, default filter, version, and log buffer statistics.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "get_configuration",
            "Read a paginated KeepPeek configuration snapshot with effective values, templates, and secret references preserved.",
            json!({
                "type": "object",
                "properties": {
                    "page_size": { "type": "integer", "minimum": 1, "maximum": 64 },
                    "page_token": { "type": "string" }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "list_secret_keys",
            "List sorted secret key names stored in secrets.toml without exposing any secret values.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "plan_configuration_change",
            "Dry-run a camera, default, or template configuration change without writing files; works even in read-only mode.",
            json!({
                "type": "object",
                "required": ["expected_configuration_revision", "change"],
                "properties": {
                    "expected_configuration_revision": { "type": "string" },
                    "targets": { "type": "object" },
                    "change": { "type": "object" }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::read_only(),
        ),
    ]
}

fn admin_write_tool_definitions() -> Vec<ToolDefinition> {
    let mut tools = admin_config_write_tool_definitions();
    tools.extend(admin_runtime_write_tool_definitions());
    tools
}

fn admin_config_write_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_def(
            "apply_configuration_plan",
            "Apply a previously validated configuration plan by plan_id and expected_configuration_revision.",
            json!({
                "type": "object",
                "required": ["plan_id", "expected_configuration_revision"],
                "properties": {
                    "plan_id": { "type": "string" },
                    "expected_configuration_revision": { "type": "string" }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::write(true, false),
        ),
        tool_def(
            "update_runtime_configuration",
            "Update runtime host, port, or storage settings; storage updates require expected_configuration_revision.",
            json!({
                "type": "object",
                "properties": {
                    "expected_configuration_revision": { "type": "string" },
                    "host": { "type": "string" },
                    "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
                    "move_existing_recordings": { "type": "boolean" },
                    "storage": { "type": "object" }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::write(true, false),
        ),
        tool_def(
            "manage_configuration_template",
            "Save, duplicate, or delete a camera configuration template.",
            json!({
                "type": "object",
                "required": ["action", "expected_configuration_revision"],
                "properties": {
                    "action": { "type": "string", "enum": ["save", "duplicate", "delete"] },
                    "expected_configuration_revision": { "type": "string" },
                    "template_id": { "type": "string" },
                    "name": { "type": "string" },
                    "expected_template_version": { "type": "integer", "minimum": 0 },
                    "template": { "type": "object" }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::write(true, false),
        ),
    ]
}

fn admin_runtime_write_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_def(
            "set_runtime_profile",
            "Switch the active runtime camera profile or revert to the base profile using expected_revision.",
            json!({
                "type": "object",
                "required": ["expected_revision"],
                "properties": {
                    "expected_revision": { "type": "integer", "minimum": 0 },
                    "profile_id": { "type": ["string", "null"] },
                    "definition_revision": { "type": "integer", "minimum": 0 },
                    "definition_digest": { "type": "string" }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::write(false, true),
        ),
        tool_def(
            "set_logging_filter",
            "Update and persist the live tracing log filter directive.",
            json!({
                "type": "object",
                "required": ["filter"],
                "properties": {
                    "filter": { "type": "string", "minLength": 1 }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::write(false, true),
        ),
    ]
}

fn tool_def(
    name: &str,
    description: &str,
    input_schema: Value,
    annotations: ToolAnnotations,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        annotations,
    }
}

fn dispatch_tool(
    ctx: &McpContext<'_>,
    name: &str,
    args: Value,
) -> Result<ToolCallResult, ToolError> {
    match name {
        "get_server_health" => tool_get_server_health(ctx),
        "search_logs" => tool_search_logs(ctx, args),
        "get_logging_settings" => tool_get_logging_settings(ctx),
        "get_configuration" => tool_get_configuration(ctx, args),
        "list_secret_keys" => tool_list_secret_keys(ctx),
        "plan_configuration_change" => tool_plan_configuration_change(ctx, args),
        "apply_configuration_plan" => tool_apply_configuration_plan(ctx, args),
        "update_runtime_configuration" => tool_update_runtime_configuration(ctx, args),
        "manage_configuration_template" => tool_manage_configuration_template(ctx, args),
        "set_runtime_profile" => tool_set_runtime_profile(ctx, args),
        "set_logging_filter" => tool_set_logging_filter(ctx, args),
        other => Err(ToolError {
            code: ERROR_METHOD_NOT_FOUND,
            message: format!("unknown MCP tool: {other}"),
            data: None,
        }),
    }
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
    health.issues.retain(|issue| {
        issue.scope == "camera"
            && health
                .cameras
                .iter()
                .any(|camera| issue.message.contains(&camera.id))
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
        config::load_secrets(path)
            .map_err(|error| ToolError::internal(format!("unable to read secrets keys: {error}")))?
            .keys()
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
    if args.storage.is_some() && args.expected_configuration_revision.trim().is_empty() {
        return Err(ToolError::invalid_params(
            "storage updates require expected_configuration_revision",
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
    let value = match input {
        OptionalUpdateInput::Direct(set) => proto::optional_string_update::Value::Set(set),
        OptionalUpdateInput::Structured {
            set: Some(set),
            clear: false,
        } => proto::optional_string_update::Value::Set(set),
        OptionalUpdateInput::Structured {
            set: None,
            clear: true,
        } => proto::optional_string_update::Value::Clear(true),
        _ => {
            return Err(ToolError::invalid_params(
                "optional update must provide either set or clear=true",
            ));
        }
    };
    Ok(proto::OptionalStringUpdate { value: Some(value) })
}

fn to_u32_update(
    input: OptionalUpdateInput<u32>,
) -> Result<proto::OptionalUint32Update, ToolError> {
    let value = match input {
        OptionalUpdateInput::Direct(set) => proto::optional_uint32_update::Value::Set(set),
        OptionalUpdateInput::Structured {
            set: Some(set),
            clear: false,
        } => proto::optional_uint32_update::Value::Set(set),
        OptionalUpdateInput::Structured {
            set: None,
            clear: true,
        } => proto::optional_uint32_update::Value::Clear(true),
        _ => {
            return Err(ToolError::invalid_params(
                "optional integer update must provide either set or clear=true",
            ));
        }
    };
    Ok(proto::OptionalUint32Update { value: Some(value) })
}

fn to_bool_update(
    input: OptionalUpdateInput<bool>,
) -> Result<proto::OptionalBoolUpdate, ToolError> {
    let value = match input {
        OptionalUpdateInput::Direct(set) => proto::optional_bool_update::Value::Set(set),
        OptionalUpdateInput::Structured {
            set: Some(set),
            clear: false,
        } => proto::optional_bool_update::Value::Set(set),
        OptionalUpdateInput::Structured {
            set: None,
            clear: true,
        } => proto::optional_bool_update::Value::Clear(true),
        _ => {
            return Err(ToolError::invalid_params(
                "optional boolean update must provide either set or clear=true",
            ));
        }
    };
    Ok(proto::OptionalBoolUpdate { value: Some(value) })
}

fn to_backend_update(
    input: OptionalUpdateInput<String>,
) -> Result<proto::OptionalCameraBackendUpdate, ToolError> {
    let value = match input {
        OptionalUpdateInput::Direct(set) => {
            proto::optional_camera_backend_update::Value::Set(parse_backend(&set)?)
        }
        OptionalUpdateInput::Structured {
            set: Some(set),
            clear: false,
        } => proto::optional_camera_backend_update::Value::Set(parse_backend(&set)?),
        OptionalUpdateInput::Structured {
            set: None,
            clear: true,
        } => proto::optional_camera_backend_update::Value::Clear(true),
        _ => {
            return Err(ToolError::invalid_params(
                "backend update must provide either set or clear=true",
            ));
        }
    };
    Ok(proto::OptionalCameraBackendUpdate { value: Some(value) })
}

fn to_transport_update(
    input: OptionalUpdateInput<String>,
) -> Result<proto::OptionalCameraTransportUpdate, ToolError> {
    let value = match input {
        OptionalUpdateInput::Direct(set) => {
            proto::optional_camera_transport_update::Value::Set(parse_transport(&set)?)
        }
        OptionalUpdateInput::Structured {
            set: Some(set),
            clear: false,
        } => proto::optional_camera_transport_update::Value::Set(parse_transport(&set)?),
        OptionalUpdateInput::Structured {
            set: None,
            clear: true,
        } => proto::optional_camera_transport_update::Value::Clear(true),
        _ => {
            return Err(ToolError::invalid_params(
                "transport update must provide either set or clear=true",
            ));
        }
    };
    Ok(proto::OptionalCameraTransportUpdate { value: Some(value) })
}

fn to_recording_mode_update(
    input: OptionalUpdateInput<String>,
) -> Result<proto::OptionalCameraRecordingModeUpdate, ToolError> {
    let value = match input {
        OptionalUpdateInput::Direct(set) => {
            proto::optional_camera_recording_mode_update::Value::Set(parse_recording_mode(&set)?)
        }
        OptionalUpdateInput::Structured {
            set: Some(set),
            clear: false,
        } => proto::optional_camera_recording_mode_update::Value::Set(parse_recording_mode(&set)?),
        OptionalUpdateInput::Structured {
            set: None,
            clear: true,
        } => proto::optional_camera_recording_mode_update::Value::Clear(true),
        _ => {
            return Err(ToolError::invalid_params(
                "recording_mode update must provide either set or clear=true",
            ));
        }
    };
    Ok(proto::OptionalCameraRecordingModeUpdate { value: Some(value) })
}

fn to_event_stream_update(
    input: OptionalUpdateInput<String>,
) -> Result<proto::OptionalEventRecordingStreamUpdate, ToolError> {
    let value = match input {
        OptionalUpdateInput::Direct(set) => {
            proto::optional_event_recording_stream_update::Value::Set(parse_event_stream(&set)?)
        }
        OptionalUpdateInput::Structured {
            set: Some(set),
            clear: false,
        } => proto::optional_event_recording_stream_update::Value::Set(parse_event_stream(&set)?),
        OptionalUpdateInput::Structured {
            set: None,
            clear: true,
        } => proto::optional_event_recording_stream_update::Value::Clear(true),
        _ => {
            return Err(ToolError::invalid_params(
                "event_recording_stream update must provide either set or clear=true",
            ));
        }
    };
    Ok(proto::OptionalEventRecordingStreamUpdate { value: Some(value) })
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

        let same_origin = mcp_post_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            vec![("Origin".into(), "http://127.0.0.1:8081".into())],
            "127.0.0.1:50000",
        );
        assert_eq!(
            super::super::handle_request(&same_origin, &router_tx, &state).status_code,
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
        assert_eq!(names, vec!["get_server_health"]);

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

        let missing_rev_storage = super::super::handle_request(
            &mcp_post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 15,
                    "method": "tools/call",
                    "params": {
                        "name": "update_runtime_configuration",
                        "arguments": { "storage": { "short_term_secs": 30 } }
                    }
                }),
                Vec::new(),
                "127.0.0.1:50000",
            ),
            &router_tx,
            &state,
        );
        assert_eq!(
            read_response_json(missing_rev_storage)["error"]["code"],
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
}
