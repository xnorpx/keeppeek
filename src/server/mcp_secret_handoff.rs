//! Loopback-only single-use secret entry handoff for MCP camera onboarding.
//!
//! Allows an Administrator MCP client to request secret entry by key name
//! without ever sending raw secret values through the LLM or MCP transport.
//! The operator submits secret values directly to KeepPeek over loopback
//! `GET/POST /mcp/secret-entry`, where values are written atomically to
//! `secrets.toml` and reloaded live.

use super::{
    ApiPrincipal, ControlCommandError, ServerState, api_status, record_access_audit, service_error,
    unix_time_ms,
};
use crate::{
    access::{AccessRole, ClientClassificationReason},
    api::proto,
    config::{
        self,
        secrets_store::{
            load_secrets_file_with_lock, set_secret_entry_with_lock, validate_secret_key,
            validate_secret_value,
        },
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rouille::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    sync::Mutex,
};
use subtle::ConstantTimeEq;
use url::{Host, Url};
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_PENDING_HANDOFFS: usize = 16;
const MAX_STORED_HANDOFFS: usize = 64;
const MAX_KEYS_PER_HANDOFF: usize = 16;
const MAX_PURPOSE_CHARS: usize = 160;
const MIN_TTL_SECONDS: u64 = 60;
const DEFAULT_TTL_SECONDS: u64 = 180;
const MAX_TTL_SECONDS: u64 = 300;
const MAX_FORM_BODY_BYTES: usize = 32 * 1024;
const TOKEN_BYTES: usize = 32;
const SECRET_FIELD_PREFIX: &str = "secret_";
const PROXY_HEADERS: [&str; 7] = [
    "Forwarded",
    "X-Forwarded-For",
    "X-Forwarded-Host",
    "X-Forwarded-Proto",
    "X-Real-IP",
    "CF-Connecting-IP",
    "True-Client-IP",
];

const _: () = {
    assert!(MAX_PENDING_HANDOFFS > 0);
    assert!(MAX_STORED_HANDOFFS >= MAX_PENDING_HANDOFFS);
    assert!(MIN_TTL_SECONDS <= DEFAULT_TTL_SECONDS && DEFAULT_TTL_SECONDS <= MAX_TTL_SECONDS);
    assert!(TOKEN_BYTES == 32);
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandoffStatus {
    Pending,
    Completed,
    Expired,
}

impl HandoffStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct RequestedSecretKey {
    key: String,
    purpose: String,
}

#[derive(Debug, Clone)]
struct HandoffRecord {
    handoff_id: String,
    token_digest: [u8; 32],
    requested_keys: Vec<RequestedSecretKey>,
    configured_keys: Vec<String>,
    principal_id: String,
    status: HandoffStatus,
    expires_at_ms: i64,
}

/// Bounded in-memory store for single-use loopback secret entry handoffs.
#[derive(Debug, Default)]
pub(super) struct HandoffStore {
    records: Mutex<Vec<HandoffRecord>>,
}

impl HandoffStore {
    pub(super) fn new() -> Self {
        Self::default()
    }

    fn create(
        &self,
        principal_id: String,
        requested_keys: Vec<RequestedSecretKey>,
        ttl_seconds: u64,
        now_ms: i64,
    ) -> Result<(String, Zeroizing<String>, i64), ControlCommandError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        expire_stale_records(&mut records, now_ms);
        let active_pending = records
            .iter()
            .filter(|record| record.status == HandoffStatus::Pending)
            .count();
        if active_pending >= MAX_PENDING_HANDOFFS {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                429,
                "maximum pending secret entry handoffs reached; complete or wait for existing requests",
            ));
        }
        let raw_token = Zeroizing::new(URL_SAFE_NO_PAD.encode(rand::random::<[u8; TOKEN_BYTES]>()));
        let token_digest = sha256_bytes(raw_token.as_bytes());
        let handoff_id = Uuid::new_v4().to_string();
        let ttl_ms = i64::try_from(ttl_seconds.saturating_mul(1_000)).unwrap_or(i64::MAX);
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        if records.len() >= MAX_STORED_HANDOFFS {
            evict_oldest_non_pending(&mut records);
        }
        records.push(HandoffRecord {
            handoff_id: handoff_id.clone(),
            token_digest,
            requested_keys,
            configured_keys: Vec::new(),
            principal_id,
            status: HandoffStatus::Pending,
            expires_at_ms,
        });
        Ok((handoff_id, raw_token, expires_at_ms))
    }

    fn status(&self, handoff_id: &str, now_ms: i64) -> Option<HandoffRecord> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        expire_stale_records(&mut records, now_ms);
        records
            .iter()
            .find(|record| record.handoff_id == handoff_id)
            .cloned()
    }

    fn lookup_by_token(&self, token: &str, now_ms: i64) -> Option<HandoffRecord> {
        let digest = sha256_bytes(token.as_bytes());
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        expire_stale_records(&mut records, now_ms);
        find_record_index_ct(&records, &digest).map(|idx| records[idx].clone())
    }

    fn complete_by_token(
        &self,
        token: &str,
        configured_keys: Vec<String>,
        now_ms: i64,
    ) -> Result<HandoffRecord, (u16, &'static str)> {
        let digest = sha256_bytes(token.as_bytes());
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        expire_stale_records(&mut records, now_ms);
        let Some(index) = find_record_index_ct(&records, &digest) else {
            return Err((403, "secret entry token is invalid or already used"));
        };
        let record = &mut records[index];
        match record.status {
            HandoffStatus::Pending => {
                record.status = HandoffStatus::Completed;
                record.configured_keys = configured_keys;
                // Zero the token digest immediately so replay cannot match this record again.
                record.token_digest = [0u8; 32];
                Ok(record.clone())
            }
            HandoffStatus::Expired => Err((410, "secret entry link has expired")),
            HandoffStatus::Completed => Err((410, "secret entry link has already been used")),
        }
    }
}

fn sha256_bytes(input: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(input);
    let mut output = [0u8; 32];
    output.copy_from_slice(&digest);
    output
}

fn expire_stale_records(records: &mut [HandoffRecord], now_ms: i64) {
    for record in records {
        if record.status == HandoffStatus::Pending && now_ms >= record.expires_at_ms {
            record.status = HandoffStatus::Expired;
        }
    }
}

fn evict_oldest_non_pending(records: &mut Vec<HandoffRecord>) {
    if let Some(index) = records
        .iter()
        .position(|record| record.status != HandoffStatus::Pending)
    {
        records.remove(index);
    }
}

fn find_record_index_ct(records: &[HandoffRecord], digest: &[u8; 32]) -> Option<usize> {
    let mut matched = None;
    for (index, record) in records.iter().enumerate() {
        // Skip zeroed digests so completed tokens never match.
        if record.token_digest == [0u8; 32] {
            continue;
        }
        if bool::from(record.token_digest.ct_eq(digest)) {
            matched = Some(index);
        }
    }
    matched
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestSecretEntryArgs {
    keys: Vec<RequestedKeyInput>,
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestedKeyInput {
    key: String,
    purpose: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetSecretEntryStatusArgs {
    handoff_id: String,
}

/// MCP tool: creates a bounded single-use loopback secret entry handoff.
pub(super) fn request_secret_entry(
    state: &ServerState,
    principal: &ApiPrincipal,
    host_port: Option<u16>,
    args: &Value,
) -> Result<Value, ControlCommandError> {
    require_admin(principal)?;
    let parsed: RequestSecretEntryArgs = serde_json::from_value(args.clone()).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("invalid request_secret_entry params: {error}"),
        )
    })?;
    let ttl_seconds = parsed.ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS);
    if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl_seconds) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("ttl_seconds must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS}"),
        ));
    }
    let requested_keys = validate_requested_keys(parsed.keys)?;
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let (handoff_id, token, expires_at_ms) =
        state
            .mcp
            .handoffs()
            .create(principal.id(), requested_keys.clone(), ttl_seconds, now_ms)?;
    let port = if state.port != 0 {
        state.port
    } else {
        host_port.unwrap_or(8081)
    };
    let local_ui_url = format!("http://127.0.0.1:{port}/mcp/secret-entry?token={}", *token);
    let cli_commands = build_cli_commands(state, &requested_keys);
    record_access_audit(
        state,
        now_ms,
        Some(&principal.id()),
        Some(principal.role),
        "mcp_secret_entry_requested",
        Some(&handoff_id),
        "pending",
        ClientClassificationReason::DirectLocal,
    );
    Ok(json!({
        "handoff_id": handoff_id,
        "expires_at_ms": expires_at_ms,
        "local_ui_url": local_ui_url,
        "cli_commands": cli_commands,
        "keys": requested_keys,
    }))
}

/// MCP tool: returns the value-free status of a secret entry handoff.
pub(super) fn get_secret_entry_status(
    state: &ServerState,
    principal: &ApiPrincipal,
    args: &Value,
) -> Result<Value, ControlCommandError> {
    require_admin(principal)?;
    let parsed: GetSecretEntryStatusArgs =
        serde_json::from_value(args.clone()).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("invalid get_secret_entry_status params: {error}"),
            )
        })?;
    let handoff_id = parsed.handoff_id.trim();
    if handoff_id.is_empty() || handoff_id.len() > 128 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "handoff_id must contain 1 to 128 characters",
        ));
    }
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let Some(record) = state.mcp.handoffs().status(handoff_id, now_ms) else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "secret entry handoff was not found",
        ));
    };
    let requested_keys: Vec<&str> = record
        .requested_keys
        .iter()
        .map(|entry| entry.key.as_str())
        .collect();
    Ok(json!({
        "handoff_id": record.handoff_id,
        "status": record.status.as_str(),
        "requested_keys": requested_keys,
        "configured_keys": record.configured_keys,
        "expires_at_ms": record.expires_at_ms,
    }))
}

fn require_admin(principal: &ApiPrincipal) -> Result<(), ControlCommandError> {
    if matches!(principal.role, AccessRole::Administrator) {
        Ok(())
    } else {
        Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "Administrator role is required for secret entry handoff tools",
        ))
    }
}

fn validate_requested_keys(
    inputs: Vec<RequestedKeyInput>,
) -> Result<Vec<RequestedSecretKey>, ControlCommandError> {
    if inputs.is_empty() || inputs.len() > MAX_KEYS_PER_HANDOFF {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("keys must contain 1 to {MAX_KEYS_PER_HANDOFF} entries"),
        ));
    }
    let mut seen = HashSet::with_capacity(inputs.len());
    let mut validated = Vec::with_capacity(inputs.len());
    for entry in inputs {
        let key = entry.key.trim().to_owned();
        validate_secret_key(&key).map_err(|error| {
            ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string())
        })?;
        if !seen.insert(key.clone()) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("duplicate secret key requested: {key}"),
            ));
        }
        let purpose = entry.purpose.trim().to_owned();
        if purpose.is_empty()
            || purpose.chars().count() > MAX_PURPOSE_CHARS
            || purpose.chars().any(char::is_control)
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("purpose must contain 1 to {MAX_PURPOSE_CHARS} printable characters"),
            ));
        }
        validated.push(RequestedSecretKey { key, purpose });
    }
    Ok(validated)
}

fn build_cli_commands(state: &ServerState, keys: &[RequestedSecretKey]) -> Vec<String> {
    let config_flag = state
        .camera_config_path
        .as_deref()
        .map(|path| format!(" --config \"{}\"", path.display()))
        .unwrap_or_default();
    keys.iter()
        .map(|entry| format!("keeppeek secret set{config_flag} {}", entry.key))
        .collect()
}

/// Handles `GET /mcp/secret-entry` on loopback with strict security headers.
pub(super) fn handle_get(request: &Request, state: &ServerState) -> Response {
    if !state.mcp_enabled() {
        return service_error(404, "not found");
    }
    if let Err(response) = validate_loopback_request(request, false) {
        return with_security_headers(response);
    }
    let Some(token) = request.get_param("token") else {
        return with_security_headers(api_status(400, "secret entry token is required"));
    };
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let Some(record) = state.mcp.handoffs().lookup_by_token(&token, now_ms) else {
        return with_security_headers(api_status(
            403,
            "secret entry token is invalid or already used",
        ));
    };
    match record.status {
        HandoffStatus::Pending => {
            let html = render_entry_form_html(&token, &record.requested_keys);
            with_security_headers(Response::html(html))
        }
        HandoffStatus::Expired => {
            with_security_headers(api_status(410, "secret entry link has expired"))
        }
        HandoffStatus::Completed => {
            with_security_headers(api_status(410, "secret entry link has already been used"))
        }
    }
}

/// Handles `POST /mcp/secret-entry` on loopback, storing secrets atomically without echoing values.
pub(super) fn handle_post(request: &Request, state: &ServerState) -> Response {
    if !state.mcp_enabled() {
        return service_error(404, "not found");
    }
    if let Err(response) = validate_loopback_request(request, true) {
        return with_security_headers(response);
    }
    let body = match read_bounded_body(request, MAX_FORM_BODY_BYTES) {
        Ok(body) => body,
        Err(response) => return with_security_headers(response),
    };
    let (token, submitted, is_json) = match parse_submission(request, &body) {
        Ok(parsed) => parsed,
        Err(response) => return with_security_headers(response),
    };
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let Some(record) = state.mcp.handoffs().lookup_by_token(&token, now_ms) else {
        return with_security_headers(api_status(
            403,
            "secret entry token is invalid or already used",
        ));
    };
    if let Err(response) = check_record_pending(&record) {
        return with_security_headers(response);
    }
    if let Err(response) = validate_submission_keys(&record, &submitted) {
        return with_security_headers(response);
    }
    let configured_keys = match persist_and_reload_secrets(state, &submitted) {
        Ok(keys) => keys,
        Err(response) => return with_security_headers(response),
    };
    let completed = match state
        .mcp
        .handoffs()
        .complete_by_token(&token, configured_keys, now_ms)
    {
        Ok(completed) => completed,
        Err((status, message)) => return with_security_headers(api_status(status, message)),
    };
    let target = completed.configured_keys.join(",");
    record_access_audit(
        state,
        now_ms,
        Some(&completed.principal_id),
        Some(AccessRole::Administrator),
        "mcp_secret_entry_completed",
        Some(&target),
        "success",
        ClientClassificationReason::DirectLocal,
    );
    let response = if is_json {
        Response::json(&json!({
            "status": "completed",
            "handoff_id": completed.handoff_id,
            "configured_keys": completed.configured_keys,
        }))
    } else {
        Response::html(render_confirmation_html(&completed.configured_keys))
    };
    with_security_headers(response)
}

/// Returns `404` when MCP is disabled or `405` for unsupported HTTP methods on `/mcp/secret-entry`.
pub(super) fn handle_unsupported_method(state: &ServerState) -> Response {
    if !state.mcp_enabled() {
        return service_error(404, "not found");
    }
    with_security_headers(
        api_status(405, "secret entry endpoint supports GET and POST only")
            .with_additional_header("Allow", "GET, POST"),
    )
}

fn validate_loopback_request(request: &Request, check_origin: bool) -> Result<(), Response> {
    if !request.remote_addr().ip().is_loopback() {
        return Err(api_status(
            403,
            "secret entry endpoint is restricted to loopback clients",
        ));
    }
    if PROXY_HEADERS
        .iter()
        .any(|header| request.header(header).is_some())
    {
        return Err(api_status(
            403,
            "proxied requests are not allowed on /mcp/secret-entry",
        ));
    }
    let Some(host) = request.header("Host").map(str::trim) else {
        return Err(api_status(403, "Host header is required"));
    };
    let parsed = Url::parse(&format!("http://{host}"))
        .map_err(|_| api_status(403, "Host header is invalid"))?;
    let host_is_loopback = match parsed.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if !host_is_loopback
        || parsed.path() != "/"
        || parsed.query().is_some()
        || !parsed.username().is_empty()
    {
        return Err(api_status(
            403,
            "Host header must be localhost or a loopback IP",
        ));
    }
    if check_origin && let Some(origin) = request.header("Origin") {
        let expected = parsed.origin().ascii_serialization();
        if !origin.eq_ignore_ascii_case(&expected) {
            return Err(api_status(
                403,
                "cross-origin requests are not allowed on /mcp/secret-entry",
            ));
        }
    }
    Ok(())
}

fn with_security_headers(response: Response) -> Response {
    response
        .with_additional_header("Cache-Control", "no-store")
        .with_additional_header("Referrer-Policy", "no-referrer")
        .with_additional_header("X-Frame-Options", "DENY")
        .with_additional_header("X-Content-Type-Options", "nosniff")
        .with_additional_header(
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        )
}

fn read_bounded_body(request: &Request, max_bytes: usize) -> Result<Zeroizing<Vec<u8>>, Response> {
    let Some(body) = request.data() else {
        return Err(api_status(400, "request body is required"));
    };
    let mut bytes = Zeroizing::new(Vec::new());
    let cap = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if body.take(cap).read_to_end(&mut bytes).is_err() {
        return Err(api_status(400, "unable to read request body"));
    }
    if bytes.len() > max_bytes {
        return Err(api_status(413, "secret entry submission is too large"));
    }
    Ok(bytes)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonSecretSubmission {
    token: String,
    secrets: BTreeMap<String, String>,
}

type ParsedSubmission = (Zeroizing<String>, BTreeMap<String, Zeroizing<String>>, bool);

fn parse_submission(request: &Request, body: &[u8]) -> Result<ParsedSubmission, Response> {
    let media_type = request
        .header("Content-Type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default();
    if media_type.eq_ignore_ascii_case("application/json") {
        let parsed: JsonSecretSubmission = serde_json::from_slice(body)
            .map_err(|_| api_status(400, "invalid JSON secret submission"))?;
        let secrets = parsed
            .secrets
            .into_iter()
            .map(|(key, value)| (key, Zeroizing::new(value)))
            .collect();
        return Ok((Zeroizing::new(parsed.token), secrets, true));
    }
    if !media_type.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
        return Err(api_status(
            415,
            "Content-Type must be application/x-www-form-urlencoded or application/json",
        ));
    }
    let mut token = Zeroizing::new(String::new());
    let mut secrets = BTreeMap::new();
    for (name, value) in url::form_urlencoded::parse(body) {
        if name == "token" {
            *token = value.into_owned();
        } else if let Some(key) = name.strip_prefix(SECRET_FIELD_PREFIX) {
            secrets.insert(key.to_owned(), Zeroizing::new(value.into_owned()));
        } else {
            return Err(api_status(400, "unexpected form field in submission"));
        }
    }
    if token.is_empty() {
        return Err(api_status(400, "secret entry token is required"));
    }
    Ok((token, secrets, false))
}

fn check_record_pending(record: &HandoffRecord) -> Result<(), Response> {
    match record.status {
        HandoffStatus::Pending => Ok(()),
        HandoffStatus::Expired => Err(api_status(410, "secret entry link has expired")),
        HandoffStatus::Completed => Err(api_status(410, "secret entry link has already been used")),
    }
}

fn validate_submission_keys(
    record: &HandoffRecord,
    submitted: &BTreeMap<String, Zeroizing<String>>,
) -> Result<(), Response> {
    if submitted.is_empty() {
        return Err(api_status(400, "at least one secret value is required"));
    }
    let allowed: HashSet<&str> = record
        .requested_keys
        .iter()
        .map(|entry| entry.key.as_str())
        .collect();
    for (key, value) in submitted {
        if !allowed.contains(key.as_str()) {
            return Err(api_status(
                400,
                "submitted key does not match requested secret keys",
            ));
        }
        if let Err(error) = validate_secret_key(key) {
            return Err(api_status(400, &error.to_string()));
        }
        if let Err(error) = validate_secret_value(value) {
            return Err(api_status(400, &error.to_string()));
        }
    }
    Ok(())
}

fn persist_and_reload_secrets(
    state: &ServerState,
    submitted: &BTreeMap<String, Zeroizing<String>>,
) -> Result<Vec<String>, Response> {
    let Some(config_path) = state.camera_config_path.as_deref() else {
        return Err(api_status(409, "secrets.toml persistence is unavailable"));
    };
    let secrets_path = config::secrets_path(config_path);
    let mut configured = Vec::with_capacity(submitted.len());
    for (key, value) in submitted {
        set_secret_entry_with_lock(&secrets_path, &state.config_update, key, value)
            .map_err(|_| api_status(500, "unable to store secret entry"))?;
        if let Some(logging) = &state.logging {
            logging.redact_secret(value);
        }
        configured.push(key.clone());
    }
    if let Ok(loaded) = load_secrets_file_with_lock(&secrets_path, &state.config_update) {
        state.mcp.refresh_secrets(config_path, loaded);
    }
    Ok(configured)
}

fn render_entry_form_html(token: &str, keys: &[RequestedSecretKey]) -> String {
    let mut fields = String::new();
    for entry in keys {
        let key = escape_html(&entry.key);
        let purpose = escape_html(&entry.purpose);
        fields.push_str(&format!(
            "<label><strong>{key}</strong><span>{purpose}</span><input type=\"password\" name=\"{SECRET_FIELD_PREFIX}{key}\" autocomplete=\"off\" required></label>"
        ));
    }
    let escaped_token = escape_html(token);
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>KeepPeek Secret Entry</title><style>body{{font-family:system-ui,sans-serif;max-width:34rem;margin:2rem auto;padding:0 1rem;background:#111;color:#eee}}label{{display:block;margin:1rem 0}}span{{display:block;color:#aaa;font-size:.9rem;margin:.2rem 0 .4rem}}input[type=password]{{width:100%;padding:.6rem;box-sizing:border-box;background:#1d1d1d;color:#fff;border:1px solid #444;border-radius:4px}}button{{margin-top:1rem;padding:.6rem 1.2rem;background:#2563eb;color:#fff;border:0;border-radius:4px;cursor:pointer}}</style></head><body><h1>KeepPeek Secret Entry</h1><p>Values are written directly to <code>secrets.toml</code> and are never sent to MCP clients.</p><form method=\"post\" action=\"/mcp/secret-entry\"><input type=\"hidden\" name=\"token\" value=\"{escaped_token}\">{fields}<button type=\"submit\">Save Secrets</button></form></body></html>"
    )
}

fn render_confirmation_html(configured_keys: &[String]) -> String {
    let items = configured_keys
        .iter()
        .map(|key| format!("<li><code>{}</code></li>", escape_html(key)))
        .collect::<Vec<_>>()
        .join("");
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>KeepPeek Secrets Saved</title><style>body{{font-family:system-ui,sans-serif;max-width:34rem;margin:2rem auto;padding:0 1rem;background:#111;color:#eee}}</style></head><body><h1>Secrets Saved</h1><p>Updated keys in <code>secrets.toml</code> and reloaded KeepPeek:</p><ul>{items}</ul><p>You can close this tab and return to your assistant.</p></body></html>"
    )
}

fn escape_html(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_keys() -> Vec<RequestedSecretKey> {
        vec![RequestedSecretKey {
            key: "CAM_PASS".to_owned(),
            purpose: "Camera password".to_owned(),
        }]
    }

    #[test]
    fn handoff_store_enforces_expiry_single_use_and_pending_cap() {
        let store = HandoffStore::new();
        let (handoff_id, token, expires_at_ms) = store
            .create("admin:test".to_owned(), sample_keys(), 60, 1_000)
            .expect("create handoff");
        assert_eq!(expires_at_ms, 61_000);
        assert_eq!(
            store.status(&handoff_id, 60_999).unwrap().status,
            HandoffStatus::Pending
        );
        assert_eq!(
            store.status(&handoff_id, 61_000).unwrap().status,
            HandoffStatus::Expired
        );
        let expired_err = store
            .complete_by_token(&token, vec!["CAM_PASS".to_owned()], 61_000)
            .expect_err("expired token must fail");
        assert_eq!(expired_err.0, 410);

        let mut tokens = Vec::new();
        for _ in 0..MAX_PENDING_HANDOFFS {
            let (_, raw_token, _) = store
                .create("admin:test".to_owned(), sample_keys(), 120, 70_000)
                .expect("pending slot available");
            tokens.push(raw_token);
        }
        let overflow = store
            .create("admin:test".to_owned(), sample_keys(), 120, 70_000)
            .expect_err("17th pending handoff must be rejected");
        assert_eq!(overflow._http_status, 429);

        let completed = store
            .complete_by_token(&tokens[0], vec!["CAM_PASS".to_owned()], 70_001)
            .expect("valid token completes once");
        assert_eq!(completed.status, HandoffStatus::Completed);
        let replay_err = store
            .complete_by_token(&tokens[0], vec!["CAM_PASS".to_owned()], 70_002)
            .expect_err("replayed token must fail");
        assert_eq!(replay_err.0, 403);
    }

    #[test]
    fn html_escaping_neutralizes_special_characters() {
        assert_eq!(
            escape_html("<script>\"'&</script>"),
            "&lt;script&gt;&quot;&#x27;&amp;&lt;/script&gt;"
        );
    }
}
