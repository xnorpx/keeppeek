use std::fmt;
use std::io::Read;
use std::time::{Duration, Instant};

use digest_auth::{AuthContext, HttpMethod, WwwAuthenticateHeader};
use ureq::http::HeaderMap;
use url::Url;

use super::{Endpoint, NOTIFICATION_XML_SIZE_BYTES_MAX, ProtocolError, Request, xml};
use crate::soap::{auth::username_token::UsernameToken, client::Credentials};

/// Bounds work on a camera-supplied duration before parsing.
const DURATION_TEXT_BYTES_MAX: usize = 64;
/// Keeps the XSD parser's unchecked integers and decimal denominator within u64.
const DURATION_DIGITS_MAX: usize = 18;
/// Bounds the initial request, challenge response and stale-nonce retry.
pub(super) const AUTH_ATTEMPTS_MAX: usize = 3;

/// Camera-reported maximum pull timeout and message count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PullLimits {
    pub timeout: Duration,
    pub messages: u32,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Failure {
    Authentication,
    Http(u16),
    Network,
    Protocol,
    Expired,
    Limits(PullLimits),
}

/// A typed event transport failure with no private payload or URL text.
#[derive(Clone, Copy, Debug)]
pub struct ClientError(pub(super) Failure);

impl ClientError {
    /// Reports a rejected authentication exchange.
    pub const fn is_authentication(self) -> bool {
        matches!(self.0, Failure::Authentication)
    }
    /// Reports an expired or unknown subscription.
    pub const fn is_expired(self) -> bool {
        matches!(self.0, Failure::Expired)
    }
    /// Returns limits supplied by a PullMessages fault.
    pub const fn pull_limits(self) -> Option<PullLimits> {
        if let Failure::Limits(limits) = self.0 {
            Some(limits)
        } else {
            None
        }
    }
    /// Returns the HTTP error status when available.
    pub const fn http_status(self) -> Option<u16> {
        if let Failure::Http(status) = self.0 {
            Some(status)
        } else {
            None
        }
    }
}
impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.0 {
            Failure::Authentication => "ONVIF event authentication failed",
            Failure::Http(_) => "camera rejected ONVIF event request",
            Failure::Network => "ONVIF event network request failed or timed out",
            Failure::Protocol => "invalid ONVIF event protocol response",
            Failure::Expired => "ONVIF subscription expired",
            Failure::Limits(_) => "ONVIF pull limits exceeded",
        })
    }
}
impl std::error::Error for ClientError {}
impl From<ProtocolError> for ClientError {
    fn from(_: ProtocolError) -> Self {
        Self(Failure::Protocol)
    }
}

/// A bounded HTTP adapter for one camera's event and subscription endpoints.
///
/// Digest state covers only the current origin, including its scheme and port.
/// Changing origins clears the challenge and starts a new client nonce.
pub struct Client {
    pub(super) camera: Endpoint,
    pub(super) credentials: Credentials,
    pub(super) agent: ureq::Agent,
    pub(super) challenge: Option<WwwAuthenticateHeader>,
    pub(super) nonce: String,
    digest_origin: Option<url::Origin>,
}

impl Client {
    /// Prepares a private camera client without making network requests.
    ///
    /// # Errors
    /// Rejects oversized or malformed credentials. HTTPS verifies certificates using platform roots.
    pub fn new(camera: Endpoint, credentials: Credentials) -> Result<Self, ClientError> {
        if credentials.username.len() > 256
            || credentials.password.len() > 4096
            || credentials.username.contains(['"', '\\'])
            || credentials.username.chars().any(char::is_control)
        {
            return Err(ClientError(Failure::Authentication));
        }
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .max_response_header_size(16 * 1024)
            .input_buffer_size(8192)
            .output_buffer_size(8192)
            .max_idle_connections(1)
            .max_idle_connections_per_host(1)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .disable_verification(false)
                    .build(),
            )
            .build();
        Ok(Self {
            camera,
            credentials,
            agent: config.into(),
            challenge: None,
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            digest_origin: None,
        })
    }

    /// Executes an event request within one shared timeout, including authentication.
    ///
    /// Always includes a WS-Security UsernameToken and uses HTTP Digest when challenged.
    /// These authentication layers are independent, including after JPEG requests.
    /// Cameras must accept the token alongside Digest; there is no token-free fallback.
    /// Digest signs the exact path, query, method and current SOAP body.
    /// Only an authentication challenge permits resending a request. Other failures
    /// have an unknown server outcome and are not retried here.
    ///
    /// # Errors
    /// Returns typed authentication, HTTP, SOAP fault, timeout and malformed-data errors.
    pub fn execute(
        &mut self,
        request: &Request,
        timeout: Duration,
    ) -> Result<Vec<u8>, ClientError> {
        if timeout.is_zero()
            || timeout > Duration::from_secs(15)
            || self.camera.resolve(request.endpoint.as_str()).is_err()
        {
            return Err(ClientError(Failure::Protocol));
        }
        let deadline = Instant::now() + timeout;
        let url = self.prepare_auth(&request.endpoint)?;
        for attempt in 0..AUTH_ATTEMPTS_MAX {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(ClientError(Failure::Network))?;
            let token =
                UsernameToken::new(&self.credentials.username, &self.credentials.password, None);
            let body =
                request.envelope(Some(&token), &format!("urn:uuid:{}", uuid::Uuid::new_v4()))?;
            let mut wire = self
                .agent
                .post(request.endpoint.as_str())
                .header(
                    "Content-Type",
                    format!(
                        "application/soap+xml; charset=utf-8; action=\"{}\"",
                        request.action
                    ),
                )
                .header("accept-encoding", "identity");
            if let Some(challenge) = &mut self.challenge {
                let target = &url[url::Position::BeforePath..url::Position::AfterQuery];
                let mut context = AuthContext::new_with_method(
                    self.credentials.username.as_str(),
                    self.credentials.password.as_str(),
                    target,
                    Some(body.as_bytes()),
                    HttpMethod::POST,
                );
                context.set_custom_cnonce(self.nonce.clone());
                let authorization = challenge
                    .respond(&context)
                    .map_err(|_| ClientError(Failure::Authentication))?;
                wire = wire.header("Authorization", authorization.to_string());
            }
            let response = wire
                .config()
                .timeout_global(Some(remaining))
                .build()
                .send(&body)
                .map_err(|_| ClientError(Failure::Network))?;
            validate_unique_headers(response.headers())?;
            if response.status().as_u16() == 401 {
                self.accept_challenge(response.headers(), attempt)?;
                continue;
            }
            return read_response(response);
        }
        Err(ClientError(Failure::Authentication))
    }

    pub(super) fn prepare_auth(&mut self, endpoint: &Endpoint) -> Result<Url, ClientError> {
        let url = Url::parse(endpoint.as_str()).map_err(|_| ClientError(Failure::Protocol))?;
        let origin = url.origin();
        if self.digest_origin.as_ref() != Some(&origin) {
            self.challenge = None;
            self.nonce = uuid::Uuid::new_v4().simple().to_string();
            self.digest_origin = Some(origin);
        }
        Ok(url)
    }

    pub(super) fn accept_challenge(
        &mut self,
        headers: &HeaderMap,
        attempt: usize,
    ) -> Result<(), ClientError> {
        let challenge = headers
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.starts_with("Digest "))
            .ok_or(ClientError(Failure::Authentication))?;
        let parsed =
            digest_auth::parse(challenge).map_err(|_| ClientError(Failure::Authentication))?;
        if attempt + 1 >= AUTH_ATTEMPTS_MAX {
            return Err(ClientError(Failure::Authentication));
        }
        if let Some(previous) = &self.challenge {
            let same_realm = previous.realm == parsed.realm;
            if (same_realm && (!parsed.stale || parsed.nonce == previous.nonce))
                || (!same_realm && attempt != 0)
            {
                return Err(ClientError(Failure::Authentication));
            }
            if !same_realm {
                self.nonce = uuid::Uuid::new_v4().simple().to_string();
            }
        }
        self.challenge = Some(parsed);
        Ok(())
    }
}

fn read_response(response: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>, ClientError> {
    let status = response.status().as_u16();
    let soap = has_media_type(response.headers(), "application/soap+xml");
    if status != 200 && (!matches!(status, 400 | 500) || !soap) {
        return Err(ClientError(Failure::Http(status)));
    }
    validate_body_headers(
        response.headers(),
        "application/soap+xml",
        NOTIFICATION_XML_SIZE_BYTES_MAX as u64,
    )?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_with_config()
        .reader()
        .take(NOTIFICATION_XML_SIZE_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ClientError(Failure::Network))?;
    let root = xml::parse(&bytes, NOTIFICATION_XML_SIZE_BYTES_MAX)?;
    let root = xml::payload(&root)?;
    if xml::matches(root, xml::SOAP, "Fault") {
        return Err(fault(root));
    }
    if status != 200 {
        return Err(ClientError(Failure::Http(status)));
    }
    Ok(bytes)
}

fn has_media_type(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

pub(super) fn validate_unique_headers(headers: &HeaderMap) -> Result<(), ClientError> {
    for name in [
        "content-type",
        "content-length",
        "content-encoding",
        "transfer-encoding",
        "www-authenticate",
        "location",
    ] {
        if headers.get_all(name).iter().count() > 1 {
            return Err(ClientError(Failure::Protocol));
        }
    }
    Ok(())
}

pub(super) fn validate_body_headers(
    headers: &HeaderMap,
    expected: &str,
    size_bytes_max: u64,
) -> Result<(), ClientError> {
    if !has_media_type(headers, expected) {
        return Err(ClientError(Failure::Protocol));
    }
    if headers
        .get("content-encoding")
        .is_some_and(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        return Err(ClientError(Failure::Protocol));
    }
    if let Some(encoding) = headers.get("transfer-encoding")
        && (!encoding.as_bytes().eq_ignore_ascii_case(b"chunked")
            || headers.contains_key("content-length"))
    {
        return Err(ClientError(Failure::Protocol));
    }
    if let Some(length) = headers.get("content-length") {
        let text = length
            .to_str()
            .map_err(|_| ClientError(Failure::Protocol))?;
        if !text.bytes().all(|byte| byte.is_ascii_digit())
            || text
                .parse::<u64>()
                .map_err(|_| ClientError(Failure::Protocol))?
                > size_bytes_max
        {
            return Err(ClientError(Failure::Protocol));
        }
    }
    Ok(())
}

fn fault(root: &xmltree::Element) -> ClientError {
    let mut pending = vec![root];
    let mut expired = false;
    let mut authentication = false;
    for _ in 0..8192 {
        let Some(node) = pending.pop() else {
            break;
        };
        if xml::matches(node, xml::EVENTS, "PullMessagesFaultResponse")
            && let Ok(limits) = parse_limits(node)
        {
            return ClientError(Failure::Limits(limits));
        }
        if xml::matches(node, xml::SOAP, "Value")
            && let Ok(value) = xml::text(node)
        {
            let local = value.rsplit(':').next().unwrap_or_default();
            expired |= matches!(local, "ResourceUnknown" | "ResourceUnknownFault");
            authentication |= matches!(local, "NotAuthorized" | "FailedAuthentication");
        }
        pending.extend(
            node.children
                .iter()
                .filter_map(xmltree::XMLNode::as_element),
        );
    }
    ClientError(if authentication {
        Failure::Authentication
    } else if expired {
        Failure::Expired
    } else {
        Failure::Protocol
    })
}

fn parse_limits(node: &xmltree::Element) -> Result<PullLimits, ProtocolError> {
    let text = xml::field(node, xml::EVENTS, "MaxTimeout")?;
    if text.len() > DURATION_TEXT_BYTES_MAX
        || text.bytes().filter(u8::is_ascii_digit).count() > DURATION_DIGITS_MAX
    {
        return Err(ProtocolError("invalid pull duration"));
    }
    let duration = text
        .parse::<xsd_types::types::duration::Duration>()
        .map_err(|_| ProtocolError("invalid pull duration"))?;
    if duration.is_negative
        || duration.years > 0
        || duration.months > 0
        || duration.days > 0
        || duration.hours > 0
        || duration.minutes > 2
        || !duration.seconds.is_finite()
        || duration.seconds < 0.0
        || duration.seconds > 120.0
    {
        return Err(ProtocolError("invalid pull duration"));
    }
    let seconds = Duration::try_from_secs_f64(duration.seconds)
        .map_err(|_| ProtocolError("invalid pull duration"))?;
    let timeout = Duration::from_secs(duration.minutes * 60)
        .checked_add(seconds)
        .ok_or(ProtocolError("invalid pull duration"))?;
    if timeout > Duration::from_secs(120) {
        return Err(ProtocolError("invalid pull duration"));
    }
    let messages = xml::field(node, xml::EVENTS, "MaxMessageLimit")?
        .parse::<u32>()
        .map_err(|_| ProtocolError("invalid pull message limit"))?;
    if timeout.is_zero() || messages == 0 {
        return Err(ProtocolError("empty pull limits"));
    }
    Ok(PullLimits {
        timeout: timeout.min(Duration::from_secs(10)),
        messages: messages.min(256),
    })
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventClient")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{Client, Credentials, Endpoint};
    use ureq::tls::{RootCerts, TlsProvider};

    #[test]
    fn native_tls_uses_platform_roots_and_verification() {
        let camera = Endpoint::new("https://127.0.0.1/onvif/events").unwrap();
        let client = Client::new(
            camera,
            Credentials {
                username: "test".to_owned(),
                password: "test".to_owned(),
            },
        )
        .unwrap();
        let tls = client.agent.config().tls_config();
        assert!(matches!(tls.provider(), TlsProvider::NativeTls));
        assert!(!tls.disable_verification());
        assert!(matches!(tls.root_certs(), RootCerts::PlatformVerifier));
    }
}
