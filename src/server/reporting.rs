//! Optional HTTPS bug reporting grant issuance and non-sensitive audit endpoints.

use super::{AuthenticatedApiRequest, ServerState, api_status, record_access_audit, unix_time_ms};
use crate::config::{self, reporting::Settings};
use rouille::{Request, Response};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use url::Url;

const MAX_GRANT_REQUEST_BYTES: usize = 128 * 1024;
const MAX_AUDIT_REQUEST_BYTES: usize = 512;
const MAX_GRANT_RESPONSE_BYTES: usize = 8 * 1024;
const MAX_DIAGNOSTICS_BYTES: u64 = 29_360_128;
const MAX_SCREEN_BYTES: u64 = 104_857_600;
const MAX_TOTAL_ARTIFACT_BYTES: u64 = 134_217_728;
const MAX_EVIDENCE_WARNINGS: usize = 16;
const MAX_IN_FLIGHT_GRANTS: usize = 4;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);

const ALLOWED_EVIDENCE_WARNINGS: &[&str] = &[
    "browser-log-window-incomplete",
    "server-log-window-incomplete",
    "server-log-filter-active",
    "server-log-stream-gap",
    "server-log-generation-changed",
    "server-log-generation-unverified",
    "server-logs-unavailable",
    "diagnostics-logging-settings-unavailable",
    "diagnostics-health-unavailable",
    "diagnostics-runtime-config-unavailable",
    "diagnostics-cameras-unavailable",
    "diagnostics-metrics-unavailable",
    "diagnostics-package-unavailable",
];

#[derive(Debug, Eq, PartialEq)]
pub enum TransportError {
    Timeout,
    Unavailable,
    InvalidResponse,
}

#[derive(Debug)]
pub struct UpstreamResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub trait GrantTransport: Send + Sync {
    fn create_grant(
        &self,
        origin: &str,
        bearer: Option<&str>,
        body: &[u8],
    ) -> Result<UpstreamResponse, TransportError>;
}

struct HttpsGrantTransport {
    agent: ureq::Agent,
}

impl HttpsGrantTransport {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .max_response_header_size(32_768)
            .timeout_global(Some(UPSTREAM_TIMEOUT))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl GrantTransport for HttpsGrantTransport {
    fn create_grant(
        &self,
        origin: &str,
        bearer: Option<&str>,
        body: &[u8],
    ) -> Result<UpstreamResponse, TransportError> {
        verify_canonical_https_origin(origin)?;
        let url = format!("{origin}/v1/reports");
        let mut request = self
            .agent
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        if let Some(secret) = bearer {
            request = request.header("Authorization", &format!("Bearer {secret}"));
        }
        let response = request.send(body).map_err(|error| match error {
            ureq::Error::Timeout(_) => TransportError::Timeout,
            _ => TransportError::Unavailable,
        })?;
        let status = response.status().as_u16();
        if response.status().is_redirection() {
            return Err(TransportError::InvalidResponse);
        }
        let bytes = response
            .into_body()
            .into_with_config()
            .limit(MAX_GRANT_RESPONSE_BYTES as u64 + 1)
            .read_to_vec()
            .map_err(|_| TransportError::InvalidResponse)?;
        if bytes.len() > MAX_GRANT_RESPONSE_BYTES {
            return Err(TransportError::InvalidResponse);
        }
        Ok(UpstreamResponse {
            status,
            body: bytes,
        })
    }
}

#[derive(Clone)]
pub struct Runtime {
    settings: Option<Settings>,
    transport: Arc<dyn GrantTransport>,
    in_flight: Arc<AtomicUsize>,
}

impl Runtime {
    pub fn new(settings: Option<Settings>) -> Self {
        Self {
            settings,
            transport: Arc::new(HttpsGrantTransport::new()),
            in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[cfg(test)]
    pub fn with_transport(settings: Option<Settings>, transport: Arc<dyn GrantTransport>) -> Self {
        Self {
            settings,
            transport,
            in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }
}

struct InFlightGuard {
    counter: Arc<AtomicUsize>,
}

impl InFlightGuard {
    fn try_acquire(counter: &Arc<AtomicUsize>) -> Option<Self> {
        let mut current = counter.load(Ordering::Acquire);
        loop {
            if current >= MAX_IN_FLIGHT_GRANTS {
                return None;
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(Self {
                        counter: Arc::clone(counter),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Serialize)]
struct ReportingStatusResponse {
    configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retention_days: Option<u8>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReportingArtifact {
    #[serde(rename = "type")]
    mime_type: String,
    bytes: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReportingArtifacts {
    diagnostics: ReportingArtifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    screen: Option<ReportingArtifact>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReportingManifest {
    version: u8,
    title: String,
    description: String,
    reproduction: String,
    expected: String,
    actual: String,
    #[serde(rename = "evidenceWarnings")]
    evidence_warnings: Vec<String>,
    artifacts: ReportingArtifacts,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportingGrantRequest {
    #[serde(rename = "idempotencyKey")]
    idempotency_key: String,
    manifest: ReportingManifest,
}

#[derive(Serialize)]
struct UpstreamGrantRequest<'a> {
    #[serde(rename = "idempotencyKey")]
    idempotency_key: &'a str,
    manifest: &'a ReportingManifest,
    #[serde(rename = "retentionDays")]
    retention_days: u8,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReportingGrantResponse {
    id: String,
    grant: String,
    expires: i64,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReportingAuditKind {
    Start,
    Cancel,
    LocalDownload,
    Complete,
    Failed,
}

impl ReportingAuditKind {
    const fn action_name(self) -> &'static str {
        match self {
            Self::Start => "bug_report_start",
            Self::Cancel => "bug_report_cancel",
            Self::LocalDownload => "bug_report_local_download",
            Self::Complete => "bug_report_complete",
            Self::Failed => "bug_report_failed",
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReportingAuditMode {
    LogsOnly,
    Screen,
}

impl ReportingAuditMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::LogsOnly => "logs_only",
            Self::Screen => "screen",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportingAuditRequest {
    event: ReportingAuditKind,
    #[serde(default)]
    mode: Option<ReportingAuditMode>,
    #[serde(default)]
    report_id: Option<String>,
}

pub(super) fn status(state: &ServerState) -> Response {
    let payload = state.reporting.settings.as_ref().map_or(
        ReportingStatusResponse {
            configured: false,
            origin: None,
            retention_days: None,
        },
        |settings| ReportingStatusResponse {
            configured: true,
            origin: Some(settings.origin.clone()),
            retention_days: Some(settings.retention_days),
        },
    );
    Response::json(&payload).with_additional_header("Cache-Control", "no-store")
}

pub(super) fn create_grant(
    request: &Request,
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
) -> Response {
    if !has_json_content_type(request) {
        return no_store(api_status(415, "request body must use application/json"));
    }
    let Some(settings) = state.reporting.settings.as_ref() else {
        audit(state, identity, "bug_report_submit", None, "unconfigured");
        return no_store(api_status(503, "remote bug reporting is not configured"));
    };
    let parsed: ReportingGrantRequest = match read_json_body(request, MAX_GRANT_REQUEST_BYTES) {
        Ok(value) => value,
        Err(response) => {
            audit(
                state,
                identity,
                "bug_report_submit",
                None,
                "invalid_request",
            );
            return response;
        }
    };
    if let Err(message) = validate_grant_request(&parsed) {
        audit(
            state,
            identity,
            "bug_report_submit",
            None,
            "invalid_request",
        );
        return no_store(api_status(400, message));
    }
    let bearer = match resolve_credential(state, settings) {
        Ok(secret) => secret,
        Err(()) => {
            audit(state, identity, "bug_report_submit", None, "failed");
            return no_store(api_status(503, "reporting credentials are unavailable"));
        }
    };
    let Some(_permit) = InFlightGuard::try_acquire(&state.reporting.in_flight) else {
        audit(state, identity, "bug_report_submit", None, "rate_limited");
        return no_store(api_status(
            429,
            "too many bug report submissions are in progress",
        ));
    };
    forward_grant_request(state, identity, settings, bearer.as_deref(), &parsed)
}

pub(super) fn record_audit(
    request: &Request,
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
) -> Response {
    if !has_json_content_type(request) {
        return no_store(api_status(415, "request body must use application/json"));
    }
    let parsed: ReportingAuditRequest = match read_json_body(request, MAX_AUDIT_REQUEST_BYTES) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if let Some(report_id) = parsed.report_id.as_deref()
        && !is_valid_report_id(report_id)
    {
        return no_store(api_status(400, "invalid report identifier"));
    }
    let outcome = parsed.mode.map_or("recorded", ReportingAuditMode::as_str);
    audit(
        state,
        identity,
        parsed.event.action_name(),
        parsed.report_id.as_deref(),
        outcome,
    );
    no_store(Response::empty_204())
}

fn forward_grant_request(
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
    settings: &Settings,
    bearer: Option<&str>,
    parsed: &ReportingGrantRequest,
) -> Response {
    let payload = UpstreamGrantRequest {
        idempotency_key: &parsed.idempotency_key,
        manifest: &parsed.manifest,
        retention_days: settings.retention_days,
    };
    let Ok(body) = serde_json::to_vec(&payload) else {
        audit(state, identity, "bug_report_submit", None, "failed");
        return no_store(api_status(500, "unable to encode report grant request"));
    };
    let response = match state
        .reporting
        .transport
        .create_grant(&settings.origin, bearer, &body)
    {
        Ok(response) => response,
        Err(TransportError::Timeout) => {
            audit(state, identity, "bug_report_submit", None, "timeout");
            return no_store(api_status(504, "reporting service timed out"));
        }
        Err(TransportError::Unavailable | TransportError::InvalidResponse) => {
            audit(state, identity, "bug_report_submit", None, "upstream_error");
            return no_store(api_status(502, "reporting service is unavailable"));
        }
    };
    handle_upstream_response(state, identity, response)
}

fn handle_upstream_response(
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
    response: UpstreamResponse,
) -> Response {
    match response.status {
        200 => {
            let Ok(grant) = serde_json::from_slice::<ReportingGrantResponse>(&response.body) else {
                audit(state, identity, "bug_report_submit", None, "upstream_error");
                return no_store(api_status(
                    502,
                    "reporting service returned an invalid grant",
                ));
            };
            if !validate_grant_response(&grant) {
                audit(state, identity, "bug_report_submit", None, "upstream_error");
                return no_store(api_status(
                    502,
                    "reporting service returned an invalid grant",
                ));
            }
            if let Some(logging) = &state.logging {
                logging.redact_secret(&grant.grant);
            }
            audit(
                state,
                identity,
                "bug_report_submit",
                Some(&grant.id),
                "success",
            );
            no_store(Response::json(&grant))
        }
        409 => {
            audit(state, identity, "bug_report_submit", None, "conflict");
            no_store(api_status(
                409,
                "report submission conflicts with an existing report",
            ))
        }
        400 | 413 | 415 | 422 => {
            audit(
                state,
                identity,
                "bug_report_submit",
                None,
                "invalid_request",
            );
            no_store(api_status(
                400,
                "reporting service rejected the report manifest",
            ))
        }
        401 | 403 => {
            audit(state, identity, "bug_report_submit", None, "upstream_error");
            no_store(api_status(
                502,
                "reporting service rejected recorder authentication",
            ))
        }
        _ => {
            audit(state, identity, "bug_report_submit", None, "upstream_error");
            no_store(api_status(502, "reporting service is unavailable"))
        }
    }
}

fn resolve_credential(state: &ServerState, settings: &Settings) -> Result<Option<String>, ()> {
    let Some(reference) = settings.credential.as_deref() else {
        return Ok(None);
    };
    let Some(config_path) = state.camera_config_path.as_deref() else {
        return Err(());
    };
    let resolved = config::resolve_secret_references(config_path, reference).map_err(|_| ())?;
    if resolved.trim().is_empty() || resolved.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(());
    }
    if let Some(logging) = &state.logging {
        logging.redact_secret(&resolved);
    }
    Ok(Some(resolved))
}

fn read_json_body<T: for<'de> Deserialize<'de>>(
    request: &Request,
    max_bytes: usize,
) -> Result<T, Response> {
    if let Some(length) = request
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        && length > max_bytes as u64
    {
        return Err(no_store(api_status(413, "request body exceeds size limit")));
    }
    let Some(body) = request.data() else {
        return Err(no_store(api_status(400, "request body is required")));
    };
    let mut bytes = Vec::new();
    let read_result = body.take(max_bytes as u64 + 1).read_to_end(&mut bytes);
    if read_result.is_err() {
        return Err(no_store(api_status(400, "unable to read request body")));
    }
    if bytes.len() > max_bytes {
        return Err(no_store(api_status(413, "request body exceeds size limit")));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| no_store(api_status(400, "invalid JSON request body")))
}

fn validate_grant_request(request: &ReportingGrantRequest) -> Result<(), &'static str> {
    if !is_valid_idempotency_key(&request.idempotency_key) {
        return Err("invalid idempotency key");
    }
    let manifest = &request.manifest;
    if manifest.version != 1 {
        return Err("unsupported report manifest version");
    }
    validate_text_field(&manifest.title, 120)?;
    validate_text_field(&manifest.description, 4_000)?;
    validate_text_field(&manifest.reproduction, 4_000)?;
    validate_text_field(&manifest.expected, 2_000)?;
    validate_text_field(&manifest.actual, 2_000)?;
    validate_evidence_warnings(&manifest.evidence_warnings)?;
    validate_artifacts(&manifest.artifacts)
}

fn validate_text_field(value: &str, max_chars: usize) -> Result<(), &'static str> {
    if value.trim().is_empty() || value.chars().count() > max_chars {
        return Err("invalid report text field");
    }
    Ok(())
}

fn validate_evidence_warnings(warnings: &[String]) -> Result<(), &'static str> {
    if warnings.len() > MAX_EVIDENCE_WARNINGS {
        return Err("too many evidence warnings");
    }
    let mut seen = HashSet::with_capacity(warnings.len());
    for warning in warnings {
        if !ALLOWED_EVIDENCE_WARNINGS.contains(&warning.as_str()) || !seen.insert(warning.as_str())
        {
            return Err("invalid evidence warning");
        }
    }
    Ok(())
}

fn validate_artifacts(artifacts: &ReportingArtifacts) -> Result<(), &'static str> {
    if artifacts.diagnostics.mime_type != "application/gzip"
        || artifacts.diagnostics.bytes == 0
        || artifacts.diagnostics.bytes > MAX_DIAGNOSTICS_BYTES
    {
        return Err("invalid diagnostics artifact description");
    }
    let mut total = artifacts.diagnostics.bytes;
    if let Some(screen) = &artifacts.screen {
        if !matches!(screen.mime_type.as_str(), "video/webm" | "video/mp4")
            || screen.bytes == 0
            || screen.bytes > MAX_SCREEN_BYTES
        {
            return Err("invalid screen artifact description");
        }
        total = total.saturating_add(screen.bytes);
    }
    if total > MAX_TOTAL_ARTIFACT_BYTES {
        return Err("combined artifacts exceed the 128 MiB limit");
    }
    Ok(())
}

fn validate_grant_response(grant: &ReportingGrantResponse) -> bool {
    is_valid_report_id(&grant.id)
        && !grant.grant.is_empty()
        && grant.grant.len() <= 4_096
        && !grant
            .grant
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        && grant.expires > 0
}

fn verify_canonical_https_origin(origin: &str) -> Result<(), TransportError> {
    if origin.len() > 512 {
        return Err(TransportError::InvalidResponse);
    }
    let url = Url::parse(origin).map_err(|_| TransportError::InvalidResponse)?;
    if url.scheme() != "https" || url.origin().ascii_serialization() != origin {
        return Err(TransportError::InvalidResponse);
    }
    Ok(())
}

fn is_valid_idempotency_key(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn is_valid_report_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn has_json_content_type(request: &Request) -> bool {
    request
        .header("Content-Type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|mime| mime.eq_ignore_ascii_case("application/json"))
}

fn audit(
    state: &ServerState,
    identity: &AuthenticatedApiRequest,
    action: &str,
    target_id: Option<&str>,
    result: &str,
) {
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

fn no_store(response: Response) -> Response {
    response.with_additional_header("Cache-Control", "no-store")
}

#[cfg(test)]
mod tests;
