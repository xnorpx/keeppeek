//! Bounded HTTP health probing for KeepPeek servers.
//!
//! Before navigating the webview to a server URL, KeepPeek Desktop sends a
//! bounded `GET /auth/session` probe with redirects disabled so unreachable
//! servers, TLS errors, and non-KeepPeek endpoints produce actionable recovery
//! messages inside the local launcher.

use std::fmt;
use std::io::Read;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::server_url::ServerOrigin;

/// Minimum allowed server probe timeout in milliseconds.
pub const MIN_PROBE_TIMEOUT_MS: u64 = 500;
/// Default server probe timeout in milliseconds.
pub const DEFAULT_PROBE_TIMEOUT_MS: u64 = 5_000;
/// Maximum allowed server probe timeout in milliseconds.
pub const MAX_PROBE_TIMEOUT_MS: u64 = 30_000;
/// Maximum response body size read from `/auth/session` in bytes.
pub const MAX_PROBE_RESPONSE_BYTES: usize = 64 * 1024;

/// Configuration bounds for a server connectivity probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOptions {
    /// Total probe timeout in milliseconds.
    pub timeout_ms: u64,
    /// Maximum number of response body bytes read before rejecting the payload.
    pub max_response_bytes: usize,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            timeout_ms: DEFAULT_PROBE_TIMEOUT_MS,
            max_response_bytes: MAX_PROBE_RESPONSE_BYTES,
        }
    }
}

/// Verified KeepPeek server probe summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerProbeResult {
    /// Canonical origin that answered the probe.
    pub origin: ServerOrigin,
    /// True when the server classifies this client as trusted-local.
    pub local: bool,
    /// True when bearer access-key sign-in is enabled.
    pub bearer_enabled: bool,
    /// Number of external sign-in methods advertised by the server.
    pub method_count: usize,
    /// Observed probe round-trip latency in milliseconds.
    pub latency_ms: u64,
}

/// Classified connectivity failure returned by [`probe_server`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProbeError {
    /// The server could not be reached over the network.
    Unreachable { detail: String },
    /// The probe exceeded the configured timeout.
    TimedOut { timeout_ms: u64 },
    /// TLS handshake or certificate verification failed.
    TlsFailure { detail: String },
    /// Remote server rejected plain HTTP with HTTP 426.
    HttpsRequired,
    /// Server attempted an HTTP redirect from `/auth/session`.
    UnexpectedRedirect { status: u16 },
    /// Server returned a non-200 HTTP status.
    UnexpectedStatus { status: u16 },
    /// Server returned a 200 response that does not match KeepPeek's session JSON.
    NotKeepPeekServer { detail: String },
}

impl ProbeError {
    /// Returns a clear user-facing summary with an actionable recovery hint.
    #[must_use]
    pub fn recovery_message(&self, origin: &ServerOrigin) -> String {
        match self {
            Self::Unreachable { detail } => {
                let local_hint = if origin.is_loopback() {
                    "Start KeepPeek on this machine (keeppeek, KeepPeekService, or KeepPeek.app) and click Retry."
                } else {
                    "Verify the server address, port, and network or VPN connection, then click Retry."
                };
                format!("Cannot connect to {origin} ({detail}). {local_hint}")
            }
            Self::TimedOut { timeout_ms } => format!(
                "Timed out waiting for {origin} after {timeout_ms} ms. Confirm KeepPeek is running and reachable, then click Retry."
            ),
            Self::TlsFailure { detail } => format!(
                "TLS verification failed for {origin} ({detail}). Install or trust the server certificate in the OS trust store, then retry."
            ),
            Self::HttpsRequired => format!(
                "{origin} requires HTTPS for remote access (HTTP 426). Enter an https:// server URL or connect from a trusted local network."
            ),
            Self::UnexpectedRedirect { status } => format!(
                "{origin} returned redirect HTTP {status} for /auth/session. Enter the direct canonical KeepPeek server URL."
            ),
            Self::UnexpectedStatus { status } => format!(
                "{origin} returned HTTP {status} for /auth/session. Confirm that KeepPeek is serving on this address and port."
            ),
            Self::NotKeepPeekServer { detail } => format!(
                "{origin} did not return a valid KeepPeek session response ({detail}). Confirm the port belongs to KeepPeek."
            ),
        }
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable { detail } => write!(formatter, "server unreachable: {detail}"),
            Self::TimedOut { timeout_ms } => {
                write!(formatter, "server probe timed out after {timeout_ms} ms")
            }
            Self::TlsFailure { detail } => write!(formatter, "TLS verification failed: {detail}"),
            Self::HttpsRequired => write!(formatter, "server requires HTTPS (HTTP 426)"),
            Self::UnexpectedRedirect { status } => {
                write!(formatter, "unexpected redirect HTTP {status}")
            }
            Self::UnexpectedStatus { status } => {
                write!(formatter, "unexpected HTTP status {status}")
            }
            Self::NotKeepPeekServer { detail } => {
                write!(formatter, "response is not a KeepPeek server: {detail}")
            }
        }
    }
}

impl std::error::Error for ProbeError {}

/// Probes `<origin>/auth/session` with bounded time and response size.
///
/// # Errors
///
/// Returns a [`ProbeError`] describing network, TLS, timeout, HTTP status, or
/// payload validation failures.
///
/// # Panics
///
/// Panics if `options.timeout_ms` is outside
/// `MIN_PROBE_TIMEOUT_MS..=MAX_PROBE_TIMEOUT_MS` or if
/// `options.max_response_bytes` is zero or exceeds `MAX_PROBE_RESPONSE_BYTES`.
pub fn probe_server(
    origin: &ServerOrigin,
    options: ProbeOptions,
) -> Result<ServerProbeResult, ProbeError> {
    assert!(
        (MIN_PROBE_TIMEOUT_MS..=MAX_PROBE_TIMEOUT_MS).contains(&options.timeout_ms),
        "probe timeout_ms must be between {MIN_PROBE_TIMEOUT_MS} and {MAX_PROBE_TIMEOUT_MS}, got {}",
        options.timeout_ms
    );
    assert!(
        (1..=MAX_PROBE_RESPONSE_BYTES).contains(&options.max_response_bytes),
        "probe max_response_bytes must be between 1 and {MAX_PROBE_RESPONSE_BYTES}, got {}",
        options.max_response_bytes
    );

    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(options.timeout_ms)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build(),
    );

    let started = Instant::now();
    let probe_url = origin.session_probe_url();
    let response = agent
        .get(probe_url.as_str())
        .header("Accept", "application/json")
        .call()
        .map_err(|error| classify_transport_error(&error, options.timeout_ms))?;

    let status = response.status().as_u16();
    if status == 426 {
        return Err(ProbeError::HttpsRequired);
    }
    if (300..400).contains(&status) {
        return Err(ProbeError::UnexpectedRedirect { status });
    }
    if status != 200 {
        return Err(ProbeError::UnexpectedStatus { status });
    }

    let body = read_bounded_body(
        response.into_body().into_reader(),
        options.max_response_bytes,
    )?;
    let parsed = parse_session_payload(&body)?;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    Ok(ServerProbeResult {
        origin: origin.clone(),
        local: parsed.local,
        bearer_enabled: parsed.bearer_enabled,
        method_count: parsed.methods.len(),
        latency_ms,
    })
}

#[derive(Deserialize)]
struct KeepPeekSessionPayload {
    local: bool,
    bearer_enabled: bool,
    methods: Vec<serde_json::Value>,
}

fn parse_session_payload(bytes: &[u8]) -> Result<KeepPeekSessionPayload, ProbeError> {
    let payload: KeepPeekSessionPayload =
        serde_json::from_slice(bytes).map_err(|error| ProbeError::NotKeepPeekServer {
            detail: format!("invalid JSON payload: {error}"),
        })?;
    if payload.methods.len() > 64 {
        return Err(ProbeError::NotKeepPeekServer {
            detail: "authentication method list exceeds 64 entries".to_owned(),
        });
    }
    Ok(payload)
}

fn read_bounded_body<R: Read>(mut reader: R, max_bytes: usize) -> Result<Vec<u8>, ProbeError> {
    let limit = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    let mut buffer = Vec::new();
    reader
        .by_ref()
        .take(limit.saturating_add(1))
        .read_to_end(&mut buffer)
        .map_err(|error| ProbeError::Unreachable {
            detail: error.to_string(),
        })?;
    if buffer.len() > max_bytes {
        return Err(ProbeError::NotKeepPeekServer {
            detail: format!("response exceeded {max_bytes} bytes"),
        });
    }
    Ok(buffer)
}

fn classify_transport_error(error: &ureq::Error, timeout_ms: u64) -> ProbeError {
    match error {
        ureq::Error::Timeout(_) => ProbeError::TimedOut { timeout_ms },
        ureq::Error::Tls(detail) => ProbeError::TlsFailure {
            detail: detail.to_string(),
        },
        other => {
            let text = other.to_string();
            let lower = text.to_ascii_lowercase();
            if lower.contains("tls") || lower.contains("certificate") || lower.contains("ssl") {
                ProbeError::TlsFailure { detail: text }
            } else if lower.contains("timed out") || lower.contains("timeout") {
                ProbeError::TimedOut { timeout_ms }
            } else {
                ProbeError::Unreachable { detail: text }
            }
        }
    }
}
