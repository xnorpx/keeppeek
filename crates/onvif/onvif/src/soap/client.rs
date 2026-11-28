#![allow(clippy::large_enum_variant)]

use crate::soap::{
    self,
    auth::{digest::Digest, username_token::UsernameToken},
};
use schema::transport::{Error, Transport};
use std::{
    fmt::{Debug, Formatter},
    sync::{Arc, Mutex},
    time::Duration,
};
use tracing::{debug, instrument, trace};
use ureq::tls::{RootCerts, TlsConfig};
use url::Url;

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    config: Config,
    digest_auth_state: Arc<Mutex<Digest>>,
}

#[derive(Clone)]
pub struct ClientBuilder {
    agent: Option<ureq::Agent>,
    config: Config,
}

impl ClientBuilder {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn new(uri: &Url) -> Self {
        Self {
            agent: None,
            config: Config {
                uri: uri.clone(),
                credentials: None,
                response_patcher: None,
                auth_type: AuthType::Any,
                reuse_digest_auth_headers: false,
                timeout: Self::DEFAULT_TIMEOUT,
                fix_time_gap: None,
                tls_verification: TlsVerification::Strict,
                follow_redirects: true,
                response_size_limit: None,
            },
        }
    }

    /// Uses a caller-provided ureq agent instead of the default agent.
    pub fn agent(mut self, agent: ureq::Agent) -> Self {
        self.agent = Some(agent);
        self
    }

    pub fn credentials(mut self, credentials: Option<Credentials>) -> Self {
        self.config.credentials = credentials;
        self
    }

    pub fn response_patcher(mut self, response_patcher: Option<ResponsePatcher>) -> Self {
        self.config.response_patcher = response_patcher;
        self
    }

    pub const fn auth_type(mut self, auth_type: AuthType) -> Self {
        self.config.auth_type = auth_type;
        self
    }

    pub const fn reuse_digest_auth_headers(mut self, reuse_digest_auth_headers: bool) -> Self {
        self.config.reuse_digest_auth_headers = reuse_digest_auth_headers;
        self
    }

    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.config.timeout = timeout;
        self
    }

    /// Disables application-level redirects when an endpoint must remain pinned.
    pub const fn follow_redirects(mut self, follow: bool) -> Self {
        self.config.follow_redirects = follow;
        self
    }

    /// Limits response bytes before SOAP deserialization.
    pub const fn response_size_limit(mut self, size_bytes: u64) -> Self {
        self.config.response_size_limit = Some(size_bytes);
        self
    }

    pub const fn fix_time_gap(mut self, time_gap: Option<chrono::Duration>) -> Self {
        self.config.fix_time_gap = time_gap;
        self
    }

    /// Sets how the default agent verifies HTTPS server certificates.
    pub const fn tls_verification(mut self, tls_verification: TlsVerification) -> Self {
        self.config.tls_verification = tls_verification;
        self
    }

    pub fn build(self) -> Client {
        let Self { agent, config } = self;
        let agent = agent.unwrap_or_else(|| Self::default_agent(&config));

        let digest = Digest::new(
            &config.uri,
            &config.credentials,
            config.reuse_digest_auth_headers,
        );

        Client {
            agent,
            config,
            digest_auth_state: Arc::new(Mutex::new(digest)),
        }
    }

    fn default_agent(config: &Config) -> ureq::Agent {
        let tls_config = match config.tls_verification {
            TlsVerification::Strict => TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
            TlsVerification::AcceptInvalidCertificates => TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .disable_verification(true)
                .build(),
        };

        let agent_config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(config.timeout))
            .tls_config(tls_config)
            .build();

        ureq::Agent::new_with_config(agent_config)
    }
}

#[derive(Clone)]
struct Config {
    uri: Url,
    credentials: Option<Credentials>,
    response_patcher: Option<ResponsePatcher>,
    auth_type: AuthType,
    reuse_digest_auth_headers: bool,
    timeout: Duration,
    fix_time_gap: Option<chrono::Duration>,
    tls_verification: TlsVerification,
    follow_redirects: bool,
    response_size_limit: Option<u64>,
}

#[derive(Clone, Debug)]
pub enum AuthType {
    /// First try to authorize with Digest and in case of error try UsernameToken auth
    Any,
    /// Use only Digest auth
    Digest,
    /// Use only UsernameToken auth
    UsernameToken,
}

/// Controls certificate verification for HTTPS camera endpoints.
#[derive(Clone, Copy, Debug, Default)]
pub enum TlsVerification {
    /// Verify the server certificate using the platform verifier.
    #[default]
    Strict,
    /// Accept invalid certificates for cameras with self-signed HTTPS endpoints.
    AcceptInvalidCertificates,
}

#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl Debug for Credentials {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{} [password hidden]", self.username))
    }
}

pub type ResponsePatcher = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

#[derive(Debug)]
enum RequestAuthType<'a> {
    Digest(&'a mut Digest),
    UsernameToken,
}

impl Transport for Client {
    #[instrument(skip_all, fields(uri = %SafeUrl(&self.config.uri)))]
    fn request(&self, message: &str) -> Result<String, Error> {
        match self.config.auth_type {
            AuthType::Any => match self.request_with_digest(message) {
                Ok(success) => Ok(success),
                Err(Error::Authorization(digest_error)) => {
                    debug!(
                        "Failed to authorize with Digest auth: {digest_error}. Trying UsernameToken auth ..."
                    );
                    self.request_with_username_token(message)
                        .map_err(|error| match error {
                            Error::Authorization(username_token_error) => {
                                Error::Authorization(format!(
                                    "Digest: {digest_error}, UsernameToken: {username_token_error}"
                                ))
                            }
                            error => error,
                        })
                }
                Err(e) => Err(e),
            },
            AuthType::Digest => self.request_with_digest(message),
            AuthType::UsernameToken => self.request_with_username_token(message),
        }
    }
}

impl Client {
    fn request_with_digest(&self, message: &str) -> Result<String, Error> {
        let mut guard = self
            .digest_auth_state
            .lock()
            .map_err(|_| Error::Other("digest authentication state is poisoned".to_string()))?;
        let mut auth_type = RequestAuthType::Digest(&mut guard);

        self.request_recursive(message, &self.config.uri, &mut auth_type, 0)
    }

    fn request_with_username_token(&self, message: &str) -> Result<String, Error> {
        let mut auth_type = RequestAuthType::UsernameToken;

        self.request_recursive(message, &self.config.uri, &mut auth_type, 0)
    }

    fn request_recursive(
        &self,
        message: &str,
        uri: &Url,
        auth_type: &mut RequestAuthType,
        redirections: u32,
    ) -> Result<String, Error> {
        let username_token = match auth_type {
            RequestAuthType::UsernameToken => self.username_token_auth(),
            _ => None,
        };
        let auth_mode = match auth_type {
            RequestAuthType::Digest(_) => "Digest",
            RequestAuthType::UsernameToken => "UsernameToken",
        };
        let action = soap_operation_name(message);

        debug!(auth_mode, action, %redirections, "About to make SOAP request");
        if tracing::enabled!(tracing::Level::TRACE) {
            let scrubbed = scrub_xml_passwords(message);
            trace!(
                auth_mode,
                action,
                payload = %scrubbed,
                "SOAP request payload"
            );
        }

        let soap_msg =
            soap::soap(message, &username_token).map_err(|e| Error::Protocol(format!("{e:?}")))?;
        let mut request = self
            .agent
            .post(uri.as_str())
            .header("Content-Type", "application/soap+xml; charset=utf-8;");
        if let RequestAuthType::Digest(digest) = auth_type
            && let Some(authorization) = digest
                .authorization_header()
                .map_err(|error| Error::Authorization(error.to_string()))?
        {
            request = request.header("Authorization", authorization);
            trace!(action, "Digest authorization header attached");
        }

        let response = request.send(soap_msg).map_err(Self::map_ureq_error)?;
        let status = response.status();
        debug!(action, status = status.as_u16(), "SOAP response status");
        if !status.is_success()
            && status != ureq::http::StatusCode::UNAUTHORIZED
            && let RequestAuthType::Digest(digest) = auth_type
        {
            digest.discard_challenge();
        }

        if status.is_success() {
            self.handle_soap_success(auth_type, response)
        } else if status == ureq::http::StatusCode::UNAUTHORIZED {
            let ctx = SoapRetry {
                message,
                uri,
                auth_mode,
                action,
                redirections,
            };
            self.handle_soap_unauthorized(ctx, auth_type, status, response)
        } else if status.is_redirection() {
            self.handle_soap_redirect(message, auth_type, action, redirections, response)
        } else {
            self.handle_soap_http_error(action, status, response)
        }
    }

    fn handle_soap_success(
        &self,
        auth_type: &mut RequestAuthType,
        response: ureq::http::Response<ureq::Body>,
    ) -> Result<String, Error> {
        if let RequestAuthType::Digest(digest) = auth_type {
            digest.set_success();
        }
        let text = self.read_response_body(response)?;
        if tracing::enabled!(tracing::Level::TRACE) {
            let scrubbed = scrub_xml_passwords(&text);
            trace!("Response body: {scrubbed}");
        }
        let response =
            soap::unsoap(&text).map_err(|error| Error::Protocol(format!("{error:?}")))?;
        if let Some(response_patcher) = &self.config.response_patcher {
            let patched = response_patcher(&response)
                .map_err(|error| Error::Protocol(format!("Patching failed: {error}")))?;
            if tracing::enabled!(tracing::Level::TRACE) {
                let scrubbed = scrub_xml_passwords(&patched);
                trace!("Response (SOAP unwrapped, patched): {scrubbed}");
            }
            Ok(patched)
        } else {
            Ok(response)
        }
    }

    fn handle_soap_unauthorized(
        &self,
        ctx: SoapRetry<'_>,
        auth_type: &mut RequestAuthType<'_>,
        status: ureq::http::StatusCode,
        response: ureq::http::Response<ureq::Body>,
    ) -> Result<String, Error> {
        let SoapRetry {
            message,
            uri,
            auth_mode,
            action,
            redirections,
        } = ctx;
        match auth_type {
            RequestAuthType::Digest(digest) if !digest.is_failed() => {
                let challenge = response
                    .headers()
                    .get(ureq::http::header::WWW_AUTHENTICATE)
                    .ok_or_else(|| {
                        Error::Authorization("missing WWW-Authenticate header".to_string())
                    })?
                    .to_str()
                    .map_err(|_| {
                        Error::Authorization("invalid WWW-Authenticate header".to_string())
                    })?
                    .to_owned();
                digest.set_401(challenge);
            }
            _ => {
                if tracing::enabled!(tracing::Level::TRACE)
                    && let Ok(text) = self.read_response_body(response)
                {
                    let scrubbed = scrub_xml_passwords(&text);
                    trace!(action, "SOAP Unauthorized body: {scrubbed}");
                }
                debug!(action, auth_mode, "SOAP request unauthorized");
                return Err(Error::Authorization(status.to_string()));
            }
        }
        debug!(action, "Retrying SOAP request after 401 Digest challenge");
        self.request_recursive(message, uri, auth_type, redirections)
    }

    fn handle_soap_redirect(
        &self,
        message: &str,
        auth_type: &mut RequestAuthType,
        action: &str,
        redirections: u32,
        response: ureq::http::Response<ureq::Body>,
    ) -> Result<String, Error> {
        if !self.config.follow_redirects {
            return Err(Error::Redirection("Redirects are disabled".to_owned()));
        }
        if redirections > 0 {
            return Err(Error::Redirection("Redirection limit exceeded".to_string()));
        }
        let new_url = response
            .headers()
            .get(ureq::http::header::LOCATION)
            .ok_or_else(|| Error::Redirection("missing Location header".to_string()))?
            .to_str()
            .map_err(|error| Error::Redirection(error.to_string()))?
            .parse::<Url>()
            .map_err(|error| Error::Redirection(error.to_string()))?;
        debug!(action, new_url = %SafeUrl(&new_url), "Redirecting SOAP request");
        self.request_recursive(message, &new_url, auth_type, redirections + 1)
    }

    fn handle_soap_http_error(
        &self,
        action: &str,
        status: ureq::http::StatusCode,
        response: ureq::http::Response<ureq::Body>,
    ) -> Result<String, Error> {
        let text = self.read_response_body(response).unwrap_or_default();
        if tracing::enabled!(tracing::Level::TRACE) {
            let scrubbed = scrub_xml_passwords(&text);
            trace!(
                action,
                status = status.as_u16(),
                "SOAP HTTP error body: {scrubbed}"
            );
        }
        let Err(soap::Error::Fault(fault)) = soap::unsoap(&text) else {
            debug!(
                action,
                status = status.as_u16(),
                "SOAP HTTP error without fault"
            );
            return Err(Error::Other(status.to_string()));
        };
        let subcode = fault.code.subcode.as_ref().map(|s| s.value.as_str());
        debug!(
            action,
            status = status.as_u16(),
            fault_code = %fault.code.value,
            ?subcode,
            unauthorized = fault.is_unauthorized(),
            "SOAP fault response"
        );
        let message = subcode.map_or_else(
            || status.to_string(),
            |subcode| format!("{status} ({subcode})"),
        );
        if fault.is_unauthorized() {
            Err(Error::Authorization(message))
        } else {
            Err(Error::Other(message))
        }
    }

    fn read_response_body(
        &self,
        response: ureq::http::Response<ureq::Body>,
    ) -> Result<String, Error> {
        let mut body = response.into_body();
        let result = match self.config.response_size_limit {
            Some(size_bytes) => body.with_config().limit(size_bytes).read_to_string(),
            None => body.read_to_string(),
        };
        result.map_err(|error| Error::Protocol(error.to_string()))
    }

    fn map_ureq_error(error: ureq::Error) -> Error {
        let message = match &error {
            ureq::Error::BadUri(raw) => format!("bad uri: {}", sanitize_url_or_redact(raw)),
            ureq::Error::RequireHttpsOnly(raw) => {
                format!("configured for https only: {}", sanitize_url_or_redact(raw))
            }
            ureq::Error::ConnectProxyFailed(raw) => {
                format!("CONNECT proxy failed: {}", sanitize_url_or_redact(raw))
            }
            _ => scrub_urls(&error_chain(&error)),
        };
        match error {
            ureq::Error::Timeout(_) => Error::Timeout(message),
            ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => Error::Connection(message),
            ureq::Error::Io(io_error) => match io_error.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                    Error::Timeout(message)
                }
                _ => Error::Connection(message),
            },
            ureq::Error::RedirectFailed | ureq::Error::TooManyRedirects => {
                Error::Redirection(message)
            }
            ureq::Error::Protocol(_) => Error::Protocol(message),
            _ => Error::Other(message),
        }
    }

    pub fn username_token_auth(&self) -> Option<UsernameToken> {
        self.config
            .credentials
            .as_ref()
            .map(|c| UsernameToken::new(&c.username, &c.password, self.config.fix_time_gap))
    }

    pub const fn set_fix_time_gap(&mut self, time_gap: Option<chrono::Duration>) {
        self.config.fix_time_gap = time_gap;
    }
}

fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    std::iter::successors(Some(error), |error| error.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

fn soap_operation_name(message: &str) -> &str {
    let trimmed = message.trim_start();
    let Some(after_lt) = trimmed.strip_prefix('<') else {
        return "unknown";
    };
    let tag_end = after_lt
        .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '>' | '/'))
        .unwrap_or(after_lt.len());
    let full_tag = &after_lt[..tag_end];
    full_tag.rsplit(':').next().unwrap_or(full_tag)
}

struct SoapRetry<'a> {
    message: &'a str,
    uri: &'a Url,
    auth_mode: &'a str,
    action: &'a str,
    redirections: u32,
}

struct SafeUrl<'a>(&'a Url);

impl std::fmt::Display for SafeUrl<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}://{}",
            self.0.scheme(),
            self.0.host_str().unwrap_or("<none>")
        )?;
        if let Some(port) = self.0.port() {
            write!(f, ":{port}")?;
        }
        f.write_str(self.0.path())?;
        if self.0.query().is_some() {
            f.write_str("?<query>")?;
        }
        Ok(())
    }
}

fn sanitize_url_or_redact(raw: &str) -> String {
    Url::parse(raw.trim()).map_or_else(
        |_| "<redacted>".to_string(),
        |url| SafeUrl(&url).to_string(),
    )
}

fn is_sensitive_xml_tag(tag_name: &str) -> bool {
    let local = tag_name
        .rsplit(':')
        .next()
        .unwrap_or(tag_name)
        .to_ascii_lowercase();
    local.contains("password")
        || local.contains("secret")
        || local.contains("nonce")
        || local.contains("passphrase")
        || local.contains("key")
}

fn find_closing_xml_tag(rest: &str, tag_name: &str) -> Option<usize> {
    let mut pos = 0;
    let mut depth = 1usize;
    while let Some(rel) = rest[pos..].find('<') {
        let lt = pos + rel;
        let tail = &rest[lt..];
        if tail.starts_with("<!--") {
            let end = tail.find("-->")?;
            pos = lt + end + 3;
        } else if tail.starts_with("<![CDATA[") {
            let end = tail.find("]]>")?;
            pos = lt + end + 3;
        } else if let Some(after_slash) = tail.strip_prefix("</") {
            let gt = after_slash.find('>')?;
            if after_slash[..gt].trim() == tag_name {
                depth -= 1;
                if depth == 0 {
                    return Some(lt);
                }
            }
            pos = lt + 2 + gt + 1;
        } else {
            let gt = tail.find('>')?;
            let inner = &tail[1..gt];
            let open_name = inner.split_whitespace().next().unwrap_or("");
            if open_name == tag_name && !inner.ends_with('/') {
                depth = depth.saturating_add(1);
            }
            pos = lt + gt + 1;
        }
    }
    None
}

fn scrub_xml_passwords(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after_lt = &rest[lt..];
        if after_lt.starts_with("<!--") {
            let Some(end) = after_lt.find("-->") else {
                out.push_str(after_lt);
                return scrub_urls(&out);
            };
            out.push_str(&after_lt[..end + 3]);
            rest = &after_lt[end + 3..];
            continue;
        }
        if after_lt.starts_with("<![CDATA[") {
            let Some(end) = after_lt.find("]]>") else {
                out.push_str(after_lt);
                return scrub_urls(&out);
            };
            out.push_str(&after_lt[..end + 3]);
            rest = &after_lt[end + 3..];
            continue;
        }
        let Some(gt) = after_lt.find('>') else {
            out.push_str(after_lt);
            return scrub_urls(&out);
        };
        let open_tag = &after_lt[..=gt];
        out.push_str(open_tag);
        rest = &after_lt[gt + 1..];
        let tag_name = open_tag
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        if !tag_name.is_empty()
            && !tag_name.starts_with('/')
            && !tag_name.starts_with('?')
            && !tag_name.starts_with('!')
            && !open_tag.ends_with("/>")
            && is_sensitive_xml_tag(tag_name)
        {
            out.push_str("[REDACTED]");
            if let Some(close_rel) = find_closing_xml_tag(rest, tag_name) {
                rest = &rest[close_rel..];
            } else {
                return scrub_urls(&out);
            }
        }
    }
    out.push_str(rest);
    scrub_urls(&out)
}

fn scrub_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(rel) = text[pos..].find("://") {
        let sep = pos + rel;
        let scheme_start = text[pos..sep]
            .char_indices()
            .rfind(|&(_, ch)| !(ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')))
            .map_or(pos, |(idx, ch)| pos + idx + ch.len_utf8());
        let scheme = &text[scheme_start..sep];
        if !scheme.starts_with(|ch: char| ch.is_ascii_alphabetic()) {
            out.push_str(&text[pos..sep + 3]);
            pos = sep + 3;
            continue;
        }
        out.push_str(&text[pos..scheme_start]);
        let url_end = text[sep + 3..]
            .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '<' | '>' | '"' | '\''))
            .map_or(text.len(), |idx| sep + 3 + idx);
        let candidate = &text[scheme_start..url_end];
        if let Ok(parsed) = Url::parse(candidate) {
            if parsed.username() != ""
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                out.push_str(&SafeUrl(&parsed).to_string());
            } else {
                out.push_str(candidate);
            }
        } else {
            out.push_str(&redact_malformed_url(candidate));
        }
        pos = url_end;
    }
    out.push_str(&text[pos..]);
    out
}

fn redact_malformed_url(candidate: &str) -> String {
    let Some((scheme, rest)) = candidate.split_once("://") else {
        return "<redacted>".to_string();
    };
    let (base, has_query) = match rest.split_once('?') {
        Some((before, _)) => (before, true),
        None => (rest, false),
    };
    let (base, _) = base.split_once('#').unwrap_or((base, ""));
    let auth_end = base.find('/').unwrap_or(base.len());
    let host_part = base[..auth_end]
        .rsplit_once('@')
        .map_or(&base[..auth_end], |(_, host)| host);
    let mut out = format!("{scheme}://{host_part}{}", &base[auth_end..]);
    if has_query {
        out.push_str("?<query>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        thread,
    };

    const SOAP_RESPONSE: &str = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"><s:Body><m:Reply xmlns:m="urn:test"><m:Value>ok</m:Value></m:Reply></s:Body></s:Envelope>"#;

    fn read_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);

            let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = std::str::from_utf8(&request[..header_end]).unwrap();
            let body_len = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                })
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            if request.len() >= header_end + 4 + body_len {
                break;
            }
        }

        String::from_utf8(request).unwrap()
    }

    fn write_response(stream: &mut TcpStream, status: &str, headers: &[(&str, &str)], body: &str) {
        let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        stream.write_all(response.as_bytes()).unwrap();
    }

    fn client_for(listener: &TcpListener) -> Client {
        let uri = Url::parse(&format!(
            "http://{}/onvif/device_service",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        ClientBuilder::new(&uri).build()
    }

    #[test]
    fn posts_soap_and_unwraps_successful_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = thread::spawn({
            let listener = listener.try_clone().unwrap();
            move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert!(request.starts_with("POST /onvif/device_service HTTP/1.1"));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("content-type: application/soap+xml; charset=utf-8;")
                );
                assert!(request.contains("<s:Envelope"));
                write_response(&mut stream, "200 OK", &[], SOAP_RESPONSE);
            }
        });

        let client = client_for(&listener);
        let response = client.request("<m:Request xmlns:m=\"urn:test\"/>").unwrap();

        assert!(response.contains("Reply"));
        server.join().unwrap();
    }

    #[test]
    fn probe_policy_rejects_redirects_without_contacting_target() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let target_url = format!("http://{}/private", target.local_addr().unwrap());
        let uri = Url::parse(&format!("http://{}/events", listener.local_addr().unwrap())).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            write_response(&mut stream, "302 Found", &[("Location", &target_url)], "");
        });

        let client = ClientBuilder::new(&uri)
            .follow_redirects(false)
            .timeout(Duration::from_secs(1))
            .build();
        let response = client.request("<m:Request xmlns:m=\"urn:test\"/>");

        assert!(matches!(response, Err(Error::Redirection(_))));
        assert_eq!(
            target.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        server.join().unwrap();
    }

    #[test]
    fn probe_policy_bounds_response_before_deserialization() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = Url::parse(&format!("http://{}/events", listener.local_addr().unwrap())).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            write_response(&mut stream, "200 OK", &[], SOAP_RESPONSE);
        });

        let client = ClientBuilder::new(&uri).response_size_limit(64).build();
        let response = client.request("<m:Request xmlns:m=\"urn:test\"/>");

        assert!(matches!(response, Err(Error::Protocol(_))));
        server.join().unwrap();
    }

    #[test]
    fn retries_digest_challenge_with_authorization_header() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = thread::spawn({
            let listener = listener.try_clone().unwrap();
            move || {
                let (mut first, _) = listener.accept().unwrap();
                let first_request = read_request(&mut first);
                assert!(
                    !first_request
                        .to_ascii_lowercase()
                        .contains("authorization: digest")
                );
                write_response(
                    &mut first,
                    "401 Unauthorized",
                    &[(
                        "WWW-Authenticate",
                        "Digest realm=\"camera\", nonce=\"nonce\", qop=\"auth\"",
                    )],
                    "",
                );

                let (mut second, _) = listener.accept().unwrap();
                let second_request = read_request(&mut second);
                assert!(
                    second_request
                        .to_ascii_lowercase()
                        .contains("authorization: digest")
                );
                write_response(&mut second, "200 OK", &[], SOAP_RESPONSE);
            }
        });

        let uri = Url::parse(&format!(
            "http://{}/onvif/device_service",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let client = ClientBuilder::new(&uri)
            .credentials(Some(Credentials {
                username: "admin".to_string(),
                password: "secret".to_string(),
            }))
            .auth_type(AuthType::Digest)
            .build();

        let response = client.request("<m:Request xmlns:m=\"urn:test\"/>").unwrap();

        assert!(response.contains("Reply"));
        server.join().unwrap();
    }

    #[test]
    fn digest_auth_survives_requests_failing_with_400() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = Url::parse(&format!(
            "http://{}/onvif/device_service",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let server = thread::spawn(move || {
            let mut issued_nonces = 0u32;
            let mut unused_nonce = None::<String>;
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                match unused_nonce.take() {
                    Some(nonce) if request.contains(&format!("nonce=\"{nonce}\"")) => {
                        if request.contains("<Bad") {
                            write_response(&mut stream, "400 Bad Request", &[], "");
                        } else {
                            write_response(&mut stream, "200 OK", &[], SOAP_RESPONSE);
                        }
                    }
                    _ => {
                        issued_nonces += 1;
                        let nonce = format!("nonce-{issued_nonces}");
                        let challenge =
                            format!("Digest realm=\"test\", qop=\"auth\", nonce=\"{nonce}\"");
                        unused_nonce = Some(nonce);
                        write_response(
                            &mut stream,
                            "401 Unauthorized",
                            &[("WWW-Authenticate", &challenge)],
                            "",
                        );
                    }
                }
            }
        });

        let client = ClientBuilder::new(&uri)
            .credentials(Some(Credentials {
                username: "admin".to_string(),
                password: "password".to_string(),
            }))
            .auth_type(AuthType::Digest)
            .build();

        for _ in 0..3 {
            let result = client.request("<Bad/>");
            assert!(matches!(result, Err(Error::Other(_))), "{result:?}");
        }

        let result = client.request("<Good/>");
        assert!(result.is_ok(), "{result:?}");
        server.join().unwrap();
    }

    #[test]
    fn error_does_not_restore_rejected_digest_attempts() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = Url::parse(&format!(
            "http://{}/onvif/device_service",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let logins = Arc::new(AtomicUsize::new(0));
        let server_logins = Arc::clone(&logins);
        let server = thread::spawn(move || {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                let has_creds = request
                    .to_ascii_lowercase()
                    .contains("\r\nauthorization: digest ");
                if has_creds && server_logins.fetch_add(1, Ordering::SeqCst) == 1 {
                    write_response(&mut stream, "503 Service Unavailable", &[], "");
                } else {
                    write_response(
                        &mut stream,
                        "401 Unauthorized",
                        &[(
                            "WWW-Authenticate",
                            "Digest realm=\"test\", qop=\"auth\", nonce=\"nonce\"",
                        )],
                        "",
                    );
                }
            }
        });

        let client = ClientBuilder::new(&uri)
            .credentials(Some(Credentials {
                username: "admin".to_string(),
                password: "password".to_string(),
            }))
            .auth_type(AuthType::Digest)
            .build();

        let result = client.request("<Good/>");
        assert!(matches!(result, Err(Error::Other(_))), "{result:?}");
        assert_eq!(logins.load(Ordering::SeqCst), 2);

        let result = client.request("<Good/>");
        assert!(matches!(result, Err(Error::Authorization(_))), "{result:?}");
        assert_eq!(logins.load(Ordering::SeqCst), 4);
        server.join().unwrap();
    }

    #[test]
    fn authorization_error_reports_digest_and_username_token_results() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = Url::parse(&format!(
            "http://{}/onvif/device_service",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let server = thread::spawn(move || {
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                read_request(&mut stream);
                write_response(
                    &mut stream,
                    "401 Unauthorized",
                    &[(
                        "WWW-Authenticate",
                        "Digest realm=\"test\", qop=\"auth\", nonce=\"nonce\"",
                    )],
                    "",
                );
            }
        });

        let client = ClientBuilder::new(&uri)
            .credentials(Some(Credentials {
                username: "admin".to_string(),
                password: "password".to_string(),
            }))
            .build();

        let error = client.request("<Good/>").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Authorization failed: Digest: 401 Unauthorized, UsernameToken: 401 Unauthorized"
        );
        server.join().unwrap();
    }

    #[test]
    fn http_error_reports_fault_subcode() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = Url::parse(&format!(
            "http://{}/onvif/device_service",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let fault = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:ter="http://www.onvif.org/ver10/error">
            <s:Body>
                <s:Fault>
                    <s:Code>
                        <s:Value>s:Sender</s:Value>
                        <s:Subcode><s:Value>ter:InvalidArgVal</s:Value></s:Subcode>
                    </s:Code>
                    <s:Reason><s:Text xml:lang="en">No such profile</s:Text></s:Reason>
                </s:Fault>
            </s:Body>
        </s:Envelope>"#;
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            write_response(&mut stream, "400 Bad Request", &[], fault);
        });

        let client = ClientBuilder::new(&uri).build();
        let error = client.request("<Good/>").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Other: 400 Bad Request (ter:InvalidArgVal)"
        );
        server.join().unwrap();
    }

    #[test]
    fn connection_error_includes_the_cause() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let uri = Url::parse(&format!("http://{address}/onvif/device_service")).unwrap();

        let client = ClientBuilder::new(&uri)
            .timeout(Duration::from_secs(2))
            .build();
        let error = client.request("<Good/>").unwrap_err();
        assert!(
            matches!(error, Error::Connection(_) | Error::Timeout(_)),
            "{error:?}"
        );
        let text = error.to_string();
        assert!(text.contains(':'), "{text}");
    }

    struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            Self(std::sync::Arc::clone(&self.0))
        }
    }

    #[test]
    fn soap_logging_is_tiered_and_redacts_secrets() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = Url::parse(&format!(
            "http://{}/onvif/device_service",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let server = thread::spawn(move || {
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                read_request(&mut stream);
                write_response(
                    &mut stream,
                    "401 Unauthorized",
                    &[(
                        "WWW-Authenticate",
                        r#"Digest realm="onvif", qop="auth", nonce="very-secret-digest-nonce""#,
                    )],
                    "",
                );
            }
        });

        let buf = LogBuf(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(LogBuf(std::sync::Arc::clone(&buf.0)))
            .finish();
        let client = ClientBuilder::new(&uri)
            .timeout(Duration::from_secs(2))
            .credentials(Some(Credentials {
                username: "admin".to_string(),
                password: "super-secret-password".to_string(),
            }))
            .build();
        let error = tracing::subscriber::with_default(subscriber, || {
            client.request("<GetCapabilities/>").unwrap_err()
        });
        assert!(matches!(error, Error::Authorization(_)), "{error:?}");
        server.join().unwrap();

        let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            logs.contains("GetCapabilities"),
            "missing SOAP action: {logs}"
        );
        assert!(logs.contains("SOAP response status"), "{logs}");
        assert!(
            !logs.contains("super-secret-password"),
            "leaked password: {logs}"
        );
        assert!(
            !logs.contains("very-secret-digest-nonce"),
            "leaked Digest nonce: {logs}"
        );
        assert!(
            !logs.contains("PasswordDigest"),
            "leaked WSSE digest: {logs}"
        );
        let scrubbed = scrub_xml_passwords("<SetUser><Password>s3cr3t!</Password></SetUser>");
        assert!(scrubbed.contains("[REDACTED]"));
        assert!(!scrubbed.contains("s3cr3t!"));
    }

    #[test]
    fn scrub_xml_passwords_blocks_cdata_comments_unclosed_and_uris() {
        let cdata = scrub_xml_passwords("<wsse:Password><![CDATA[s3cr3t!]]></wsse:Password>");
        assert_eq!(cdata, "<wsse:Password>[REDACTED]</wsse:Password>");
        assert!(!cdata.contains("s3cr3t!"));

        let commented = scrub_xml_passwords("<Password><!-- </Password> -->s3cr3t!</Password>");
        assert_eq!(commented, "<Password>[REDACTED]</Password>");
        assert!(!commented.contains("s3cr3t!"));

        let unclosed = scrub_xml_passwords("<Password>s3cr3t!");
        assert_eq!(unclosed, "<Password>[REDACTED]");
        assert!(!unclosed.contains("s3cr3t!"));

        let extra_tags = scrub_xml_passwords(
            "<wsse:Nonce>n0nc3!</wsse:Nonce><Passphrase>p4ss</Passphrase><PrivateKey>k3y!</PrivateKey>",
        );
        assert!(!extra_tags.contains("n0nc3!"));
        assert!(!extra_tags.contains("p4ss"));
        assert!(!extra_tags.contains("k3y!"));

        let stream_uri = scrub_xml_passwords(
            r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"><tt:Uri>rtsp://admin:hunter2@192.0.2.10:554/stream1?token=tok123</tt:Uri><Endpoint href="http://u:p@192.0.2.10/snap?secret=tok456"/></s:Envelope>"#,
        );
        assert!(stream_uri.contains(r#"xmlns:s="http://www.w3.org/2003/05/soap-envelope""#));
        assert!(stream_uri.contains("rtsp://192.0.2.10:554/stream1?<query>"));
        assert!(stream_uri.contains("http://192.0.2.10/snap?<query>"));
        assert!(!stream_uri.contains("hunter2"));
        assert!(!stream_uri.contains("tok123"));
        assert!(!stream_uri.contains("tok456"));

        let multibyte = scrub_xml_passwords(
            "<Fault>Error：http://u:p@192.0.2.1/?token=abc —rtsp://u:p@192.0.2.1/s?token=tok</Fault>",
        );
        assert!(multibyte.contains("Error：http://192.0.2.1/?<query>"));
        assert!(multibyte.contains("—rtsp://192.0.2.1/s?<query>"));
        assert!(!multibyte.contains("abc"));
        assert!(!multibyte.contains("tok"));
    }

    #[test]
    fn soap_span_and_response_trace_redact_uri_credentials_and_tokens() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = Url::parse(&format!(
            "http://admin:span-pass@{}/onvif/device_service?token=span-tok",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let response_xml = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:tt="http://www.onvif.org/ver10/schema"><s:Body><trt:GetStreamUriResponse xmlns:trt="http://www.onvif.org/ver10/media/wsdl"><trt:MediaUri><tt:Uri>rtsp://camuser:campass@192.0.2.55:554/h264?token=stream-tok</tt:Uri></trt:MediaUri></trt:GetStreamUriResponse></s:Body></s:Envelope>"#;
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            write_response(&mut stream, "200 OK", &[], response_xml);
        });

        let buf = LogBuf(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(LogBuf(std::sync::Arc::clone(&buf.0)))
            .finish();
        let client = ClientBuilder::new(&uri)
            .timeout(Duration::from_secs(2))
            .build();
        let body = tracing::subscriber::with_default(subscriber, || {
            client.request("<GetStreamUri/>").unwrap()
        });
        assert!(body.contains("stream-tok"));
        server.join().unwrap();

        let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("onvif/device_service?<query>"), "{logs}");
        assert!(
            logs.contains("rtsp://192.0.2.55:554/h264?<query>"),
            "{logs}"
        );
        assert!(!logs.contains("span-pass"), "{logs}");
        assert!(!logs.contains("span-tok"), "{logs}");
        assert!(!logs.contains("camuser"), "{logs}");
        assert!(!logs.contains("campass"), "{logs}");
        assert!(!logs.contains("stream-tok"), "{logs}");
    }

    #[test]
    fn map_ureq_error_redacts_bad_uri_and_https_only_urls() {
        let bad = Client::map_ureq_error(ureq::Error::BadUri(
            "http://user:canary-pw@192.0.2.10/onvif?token=canary-tok".to_string(),
        ));
        let bad_text = bad.to_string();
        assert!(!bad_text.contains("canary-pw"), "{bad_text}");
        assert!(!bad_text.contains("canary-tok"), "{bad_text}");

        let unparseable = Client::map_ureq_error(ureq::Error::BadUri(
            "://unparseable-canary-secret".to_string(),
        ));
        assert!(!unparseable.to_string().contains("canary-secret"));

        let https_only = Client::map_ureq_error(ureq::Error::RequireHttpsOnly(
            "http://user:canary-pw@192.0.2.10/onvif?token=canary-tok".to_string(),
        ));
        let https_text = https_only.to_string();
        assert!(!https_text.contains("canary-pw"), "{https_text}");
        assert!(!https_text.contains("canary-tok"), "{https_text}");
    }
}
