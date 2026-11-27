//! JSON-RPC 2.0 and MCP `2025-03-26` protocol types and bounded request parsing.
//!
//! KeepPeek implements the MCP protocol directly with `serde` and `serde_json`
//! so every request and response respects explicit byte and structure bounds.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Supported MCP protocol version negotiated during `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-03-26";
/// JSON-RPC version required on every request and response.
pub const JSONRPC_VERSION: &str = "2.0";
/// Maximum accepted JSON-RPC request body size in bytes.
pub const MAX_REQUEST_BODY_BYTES: usize = 256 * 1_024;
/// Maximum accepted tool name length in bytes.
pub const MAX_TOOL_NAME_BYTES: usize = 128;
/// HTTP header sent by `keeppeek mcp --read-only` to disable write tools.
pub const READ_ONLY_HEADER: &str = "X-KeepPeek-MCP-Read-Only";

/// Standard JSON-RPC parse error code.
pub const ERROR_PARSE: i64 = -32700;
/// Standard JSON-RPC invalid request error code.
pub const ERROR_INVALID_REQUEST: i64 = -32600;
/// Standard JSON-RPC method not found error code.
pub const ERROR_METHOD_NOT_FOUND: i64 = -32601;
/// Standard JSON-RPC invalid parameters error code.
pub const ERROR_INVALID_PARAMS: i64 = -32602;
/// Standard JSON-RPC internal error code.
pub const ERROR_INTERNAL: i64 = -32603;
/// Server error code returned when authorization or write policy blocks a call.
pub const ERROR_FORBIDDEN: i64 = -32001;
/// Server error code returned when the concurrency limit is full.
pub const ERROR_BUSY: i64 = -32002;
/// Server error code returned when a request exceeds its timeout budget.
pub const ERROR_TIMEOUT: i64 = -32003;
/// Server error code returned when a serialized response exceeds `max_response_bytes`.
pub const ERROR_RESPONSE_TOO_LARGE: i64 = -32004;

/// Inbound JSON-RPC 2.0 request or notification envelope.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct JsonRpcRequest {
    /// Must equal `"2.0"`.
    pub jsonrpc: String,
    /// Optional request identifier; absent on notifications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Target JSON-RPC or MCP method name.
    pub method: String,
    /// Optional method parameter payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Returns `true` when the request carries no identifier and expects no reply.
    #[must_use]
    pub const fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Validates the JSON-RPC 2.0 envelope fields.
    ///
    /// # Errors
    ///
    /// Returns a JSON-RPC invalid-request error when the version, method, or
    /// identifier type is invalid.
    pub fn validate(&self) -> Result<(), JsonRpcError> {
        if self.jsonrpc != JSONRPC_VERSION {
            return Err(JsonRpcError::new(
                ERROR_INVALID_REQUEST,
                "jsonrpc must equal \"2.0\"",
            ));
        }
        if self.method.trim().is_empty() || self.method.len() > MAX_TOOL_NAME_BYTES {
            return Err(JsonRpcError::new(
                ERROR_INVALID_REQUEST,
                "method must be a non-empty string within 128 bytes",
            ));
        }
        if let Some(id) = &self.id
            && !matches!(id, Value::Null | Value::String(_) | Value::Number(_))
        {
            return Err(JsonRpcError::new(
                ERROR_INVALID_REQUEST,
                "request id must be a string, number, or null",
            ));
        }
        Ok(())
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct JsonRpcError {
    /// Numeric JSON-RPC or server error code.
    pub code: i64,
    /// Short human-readable error message.
    pub message: String,
    /// Optional structured error details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    /// Creates an error object without extra data.
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attaches structured detail data to this error.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// Outbound JSON-RPC 2.0 response envelope.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct JsonRpcResponse {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Identifier copied from the corresponding request, or `null`.
    pub id: Value,
    /// Successful result payload when `error` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error payload when `result` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Builds a successful JSON-RPC 2.0 response.
    #[must_use]
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Builds an error JSON-RPC 2.0 response.
    #[must_use]
    pub fn error(id: Value, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Parameters accepted by the MCP `initialize` method.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct InitializeParams {
    /// Protocol version requested by the client.
    #[serde(rename = "protocolVersion", default)]
    pub protocol_version: Option<String>,
    /// Client capability map.
    #[serde(default)]
    pub capabilities: Value,
    /// Client implementation name and version.
    #[serde(rename = "clientInfo", default)]
    pub client_info: Option<ImplementationInfo>,
}

/// Client or server implementation metadata.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ImplementationInfo {
    /// Implementation package name.
    pub name: String,
    /// Implementation semantic version.
    pub version: String,
}

/// Result returned by the MCP `initialize` method.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct InitializeResult {
    /// Negotiated MCP protocol version.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// Capabilities advertised by KeepPeek.
    pub capabilities: Value,
    /// KeepPeek server implementation metadata.
    #[serde(rename = "serverInfo")]
    pub server_info: ImplementationInfo,
    /// Short guidance for MCP clients.
    pub instructions: String,
}

impl InitializeResult {
    /// Creates the default KeepPeek MCP `initialize` response payload.
    #[must_use]
    pub fn keeppeek(writes_enabled: bool, snapshots_enabled: bool) -> Self {
        let mode = if writes_enabled {
            "read-write"
        } else {
            "read-only"
        };
        let snapshots = if snapshots_enabled {
            "enabled"
        } else {
            "disabled"
        };
        Self {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: json!({
                "tools": {
                    "listChanged": false
                }
            }),
            server_info: ImplementationInfo {
                name: "keeppeek".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            instructions: format!(
                "KeepPeek MCP server ({mode}, snapshots {snapshots}). Always plan configuration changes with plan_configuration_change before applying."
            ),
        }
    }
}

/// MCP tool behavior hints returned in `tools/list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolAnnotations {
    /// Indicates that the tool does not modify server state.
    #[serde(rename = "readOnlyHint")]
    pub read_only_hint: bool,
    /// Indicates that the tool may perform destructive state changes.
    #[serde(rename = "destructiveHint")]
    pub destructive_hint: bool,
    /// Indicates that repeated identical calls have no extra effect.
    #[serde(rename = "idempotentHint")]
    pub idempotent_hint: bool,
    /// Indicates that the tool interacts with external systems beyond KeepPeek.
    #[serde(rename = "openWorldHint")]
    pub open_world_hint: bool,
}

impl ToolAnnotations {
    /// Returns annotations for a read-only KeepPeek inspection tool.
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            read_only_hint: true,
            destructive_hint: false,
            idempotent_hint: true,
            open_world_hint: false,
        }
    }

    /// Returns annotations for a state-changing KeepPeek tool.
    #[must_use]
    pub const fn write(destructive: bool, idempotent: bool) -> Self {
        Self {
            read_only_hint: false,
            destructive_hint: destructive,
            idempotent_hint: idempotent,
            open_world_hint: false,
        }
    }
}

/// Metadata for one MCP tool returned by `tools/list`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolDefinition {
    /// Unique tool identifier.
    pub name: String,
    /// Concise summary of the tool behavior.
    pub description: String,
    /// JSON Schema describing accepted tool arguments.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    /// Behavioral hints for MCP clients.
    pub annotations: ToolAnnotations,
}

/// Parameters accepted by `tools/call`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolCallParams {
    /// Target tool name.
    pub name: String,
    /// JSON object of tool arguments.
    #[serde(default)]
    pub arguments: Value,
}

/// Content item returned inside an MCP `tools/call` result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    /// Plain or JSON-encoded text content.
    Text {
        /// UTF-8 text payload.
        text: String,
    },
    /// Base64-encoded image content.
    Image {
        /// Base64-encoded image bytes.
        data: String,
        /// MIME media type such as `image/jpeg`.
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

/// Result returned by `tools/call`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolCallResult {
    /// Human-readable or media content items.
    pub content: Vec<ToolContent>,
    /// Machine-readable structured JSON payload.
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// Marks a tool-level error that the model can inspect.
    #[serde(rename = "isError", default, skip_serializing_if = "is_false")]
    pub is_error: bool,
}

impl ToolCallResult {
    /// Creates a successful tool result carrying both compact JSON text and structured data.
    #[must_use]
    pub fn json(value: Value) -> Self {
        let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
        Self {
            content: vec![ToolContent::Text { text }],
            structured_content: Some(value),
            is_error: false,
        }
    }

    /// Creates a tool-level error result.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent::Text {
                text: message.into(),
            }],
            structured_content: None,
            is_error: true,
        }
    }
}

const fn is_false(value: &bool) -> bool {
    !*value
}

/// Parses and validates a JSON-RPC 2.0 request within a strict byte cap.
///
/// # Errors
///
/// Returns a ready-to-serialize JSON-RPC error response when the body exceeds
/// `max_bytes`, fails JSON parsing, or violates the JSON-RPC 2.0 envelope.
pub fn parse_bounded_request(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<JsonRpcRequest, Box<JsonRpcResponse>> {
    if bytes.is_empty() {
        return Err(Box::new(JsonRpcResponse::error(
            Value::Null,
            JsonRpcError::new(ERROR_INVALID_REQUEST, "request body must not be empty"),
        )));
    }
    if bytes.len() > max_bytes {
        return Err(Box::new(JsonRpcResponse::error(
            Value::Null,
            JsonRpcError::new(
                ERROR_INVALID_REQUEST,
                format!("request body exceeds {max_bytes} bytes"),
            ),
        )));
    }
    let raw: Value = serde_json::from_slice(bytes).map_err(|_| {
        Box::new(JsonRpcResponse::error(
            Value::Null,
            JsonRpcError::new(ERROR_PARSE, "invalid JSON-RPC request payload"),
        ))
    })?;
    let fallback_id = raw
        .as_object()
        .and_then(|map| map.get("id"))
        .filter(|id| matches!(id, Value::Null | Value::String(_) | Value::Number(_)))
        .cloned()
        .unwrap_or(Value::Null);
    let request: JsonRpcRequest = serde_json::from_value(raw).map_err(|_| {
        Box::new(JsonRpcResponse::error(
            fallback_id.clone(),
            JsonRpcError::new(ERROR_INVALID_REQUEST, "invalid JSON-RPC 2.0 request object"),
        ))
    })?;
    if let Err(error) = request.validate() {
        return Err(Box::new(JsonRpcResponse::error(fallback_id, error)));
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_initialize_and_notification_requests() {
        let init_bytes = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#;
        let parsed = parse_bounded_request(init_bytes, MAX_REQUEST_BODY_BYTES).unwrap();
        assert_eq!(parsed.method, "initialize");
        assert_eq!(parsed.id, Some(json!(1)));
        assert!(!parsed.is_notification());

        let notify_bytes = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let notification = parse_bounded_request(notify_bytes, MAX_REQUEST_BODY_BYTES).unwrap();
        assert!(notification.is_notification());
    }

    #[test]
    fn rejects_oversized_malformed_and_invalid_version_requests() {
        let oversized = vec![b' '; 64];
        let err = parse_bounded_request(&oversized, 32).unwrap_err();
        assert_eq!(err.error.unwrap().code, ERROR_INVALID_REQUEST);

        let parse_err = parse_bounded_request(b"{not-json", MAX_REQUEST_BODY_BYTES).unwrap_err();
        assert_eq!(parse_err.error.unwrap().code, ERROR_PARSE);

        let bad_version = br#"{"jsonrpc":"1.0","id":"req-1","method":"ping"}"#;
        let ver_err = parse_bounded_request(bad_version, MAX_REQUEST_BODY_BYTES).unwrap_err();
        assert_eq!(ver_err.id, json!("req-1"));
        assert_eq!(ver_err.error.unwrap().code, ERROR_INVALID_REQUEST);

        let bad_id = br#"{"jsonrpc":"2.0","id":[1,2],"method":"ping"}"#;
        let id_err = parse_bounded_request(bad_id, MAX_REQUEST_BODY_BYTES).unwrap_err();
        assert_eq!(id_err.id, Value::Null);
        assert_eq!(id_err.error.unwrap().code, ERROR_INVALID_REQUEST);
    }

    #[test]
    fn tool_call_result_omits_is_error_when_false() {
        let ok = serde_json::to_value(ToolCallResult::json(json!({"status": "ok"}))).unwrap();
        assert!(ok.get("isError").is_none());
        assert_eq!(ok["structuredContent"]["status"], "ok");

        let err = serde_json::to_value(ToolCallResult::error("denied")).unwrap();
        assert_eq!(err["isError"], true);
    }
}
