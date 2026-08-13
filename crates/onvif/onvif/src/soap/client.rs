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
    #[instrument(skip_all, fields(uri = self.config.uri.as_str()))]
    fn request(&self, message: &str) -> Result<String, Error> {
        match self.config.auth_type {
            AuthType::Any => match self.request_with_digest(message) {
                Ok(success) => Ok(success),
                Err(Error::Authorization(e)) => {
                    debug!(
                        "Failed to authorize with Digest auth: {e}. Trying UsernameToken auth ..."
                    );
                    self.request_with_username_token(message)
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

        debug!(?auth_type, %redirections, "About to make request.");

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
            debug!("Digest headers added");
        }

        trace!("Request body: {soap_msg}");

        let response = request.send(soap_msg).map_err(Self::map_ureq_error)?;

        let status = response.status();

        debug!("Response status: {status}");

        if status.is_success() {
            if let RequestAuthType::Digest(digest) = auth_type {
                digest.set_success();
            }

            let text = Self::read_response_body(response)?;
            trace!("Response body: {text}");
            let response =
                soap::unsoap(&text).map_err(|error| Error::Protocol(format!("{error:?}")))?;

            if let Some(response_patcher) = &self.config.response_patcher {
                let patched = response_patcher(&response)
                    .map_err(|error| Error::Protocol(format!("Patching failed: {error}")))?;
                trace!("Response (SOAP unwrapped, patched): {patched}");
                Ok(patched)
            } else {
                Ok(response)
            }
        } else if status == ureq::http::StatusCode::UNAUTHORIZED {
            match auth_type {
                RequestAuthType::Digest(digest) if !digest.is_failed() => {
                    let challenge = response
                        .headers()
                        .get(ureq::http::header::WWW_AUTHENTICATE)
                        .ok_or_else(|| {
                            Error::Authorization("missing WWW-Authenticate header".to_string())
                        })?
                        .to_str()
                        .map_err(|error| Error::Authorization(error.to_string()))?
                        .to_owned();
                    digest.set_401(challenge);
                }
                _ => {
                    if let Ok(text) = Self::read_response_body(response) {
                        trace!("Got Unauthorized with body: {text}");
                    }

                    return Err(Error::Authorization("Unauthorized".to_string()));
                }
            }

            self.request_recursive(message, uri, auth_type, redirections)
        } else if status.is_redirection() {
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

            debug!("Redirecting to {new_url} ...");

            self.request_recursive(message, &new_url, auth_type, redirections + 1)
        } else {
            if let Ok(text) = Self::read_response_body(response) {
                trace!("Got HTTP error with body: {text}");
                if let Err(soap::Error::Fault(fault)) = soap::unsoap(&text)
                    && fault.is_unauthorized()
                {
                    return Err(Error::Authorization("Unauthorized".to_string()));
                }
            }

            Err(Error::Other(status.to_string()))
        }
    }

    fn read_response_body(response: ureq::http::Response<ureq::Body>) -> Result<String, Error> {
        let mut body = response.into_body();
        body.read_to_string()
            .map_err(|error| Error::Protocol(error.to_string()))
    }

    fn map_ureq_error(error: ureq::Error) -> Error {
        match error {
            ureq::Error::Timeout(timeout) => Error::Timeout(timeout.to_string()),
            error @ (ureq::Error::HostNotFound | ureq::Error::ConnectionFailed) => {
                Error::Connection(error.to_string())
            }
            ureq::Error::Io(error) => match error.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                    Error::Timeout(error.to_string())
                }
                _ => Error::Connection(error.to_string()),
            },
            error @ (ureq::Error::RedirectFailed | ureq::Error::TooManyRedirects) => {
                Error::Redirection(error.to_string())
            }
            ureq::Error::Protocol(error) => Error::Protocol(error.to_string()),
            error => Error::Other(error.to_string()),
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
}
