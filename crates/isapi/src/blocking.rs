//! Blocking HTTP transport for the Sans-I/O request and authentication core.
//!
//! Each client targets one origin, ignores proxy environment variables, rejects
//! redirects, and bounds request execution. HTTPS uses verified platform TLS.
//! Camera HTTP is supported for isolated networks; Digest does not encrypt traffic.

use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ureq::RequestExt;
use ureq::http::{Request as HttpRequest, Response};
use ureq::tls::{TlsConfig, TlsProvider};
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{Connector, NativeTlsConnector, TcpConnector};

mod audio;
mod interrupt;
mod management;
#[doc(inline)]
pub use audio::{Microphone, Speaker, Talk};
use interrupt::{Control, Interruptible};

use crate::error::Kind;
use crate::{
    Authorization, Credentials, Decoder, Error, Part, Request, Session, XML_SIZE_BYTES_MAX,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HEADER_SIZE_BYTES_MAX: usize = 16 * 1024;
const TRANSPORT_BUFFER_SIZE_BYTES: usize = 8 * 1024;
const AUTH_ATTEMPTS_MAX: usize = 3;
const STREAM_LIFETIME_MAX: Duration = Duration::from_secs(300);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// An origin-bound HTTP adapter with private, reusable Digest authentication state.
pub struct Client {
    origin: String,
    agent: ureq::Agent,
    session: Session,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    idle_timeout: Duration,
}

/// Configures cancellation and notification-progress deadlines for one camera client.
pub struct ClientBuilder {
    origin: String,
    credentials: Credentials,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    idle_timeout: Duration,
}

impl ClientBuilder {
    /// Supplies a quick, nonblocking cancellation check for this client's operations.
    pub fn cancelled(mut self, check: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.cancelled = Arc::new(check);
        self
    }

    /// Sets the maximum wait for a complete notification, including partial-input trickles.
    pub const fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Validates configuration and prepares the transport without connecting.
    ///
    /// # Errors
    /// Rejects invalid origins or credentials and idle timeouts outside (0, 300 seconds].
    pub fn build(self) -> Result<Client, Error> {
        let origin = validate_origin(&self.origin)?;
        let session = Session::new(self.credentials)?;
        if self.idle_timeout.is_zero() || self.idle_timeout > STREAM_LIFETIME_MAX {
            return Err(Error::new(Kind::InvalidInput));
        }
        let control = Control::new(Arc::clone(&self.cancelled), None);
        Ok(Client {
            origin,
            agent: make_agent(control),
            session,
            cancelled: self.cancelled,
            idle_timeout: self.idle_timeout,
        })
    }
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientBuilder")
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Prepares a client without connecting to the camera.
    ///
    /// # Errors
    /// Requires an HTTP(S) origin without credentials, path, query, or fragment,
    /// and credentials accepted by [`Session::new`].
    pub fn new(origin: impl AsRef<str>, credentials: Credentials) -> Result<Self, Error> {
        Self::builder(origin, credentials).build()
    }

    /// Prepares optional transport settings while leaving the protocol core independent of I/O.
    pub fn builder(origin: impl AsRef<str>, credentials: Credentials) -> ClientBuilder {
        ClientBuilder {
            origin: origin.as_ref().to_owned(),
            credentials,
            cancelled: Arc::new(|| false),
            idle_timeout: STREAM_IDLE_TIMEOUT,
        }
    }

    /// Reads an ISAPI resource within 15 seconds and at most 256 KiB.
    ///
    /// # Errors
    /// Returns request, authentication, HTTP status, transport, or size-limit errors.
    pub fn get(&mut self, resource: impl AsRef<str>) -> Result<Vec<u8>, Error> {
        self.execute(&Request::get(resource)?)
    }

    /// Opens one persistent alert response for a caller-bounded total lifetime.
    ///
    /// The lifetime includes authentication and body reads and must not exceed
    /// five minutes. Reconnection is the caller's responsibility. Dropping the
    /// returned stream closes it; no worker, retry timer, or event queue is spawned.
    ///
    /// # Errors
    /// Rejects zero or excessive lifetimes, authentication failures, bad response
    /// media types, and network failures. This method never changes camera settings.
    pub fn alert_stream(&mut self, lifetime: Duration) -> Result<AlertStream, Error> {
        if lifetime.is_zero() || lifetime > STREAM_LIFETIME_MAX {
            return Err(Error::new(Kind::InvalidInput));
        }
        self.open_stream(Some(Instant::now() + lifetime))
    }

    /// Subscribes continuously until cancellation, EOF, or a notification-progress timeout.
    ///
    /// No periodic reconnect is imposed. The caller owns reconnect/backoff policy.
    ///
    /// # Errors
    /// Returns the same connection, authentication, or framing errors as [`Self::alert_stream`].
    pub fn subscribe(&mut self) -> Result<AlertStream, Error> {
        self.open_stream(None)
    }

    fn open_stream(&mut self, deadline: Option<Instant>) -> Result<AlertStream, Error> {
        let setup_deadline = Instant::now() + REQUEST_TIMEOUT;
        let control = Control::new(
            Arc::clone(&self.cancelled),
            Some(deadline.map_or(setup_deadline, |value| value.min(setup_deadline))),
        );
        let agent = make_agent(control.clone());
        let response = self.open(
            &Request::get("/ISAPI/Event/notification/alertStream")?,
            deadline,
            &agent,
        )?;
        if !response.status().is_success() {
            return Err(Error::new(Kind::HttpStatus(response.status().as_u16())));
        }
        let mut types = response.headers().get_all("content-type").iter();
        let content_type = types
            .next()
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| Error::new(Kind::Protocol))?;
        if types.next().is_some() {
            return Err(Error::new(Kind::Protocol));
        }
        let decoder = Decoder::new(content_type)?;
        Ok(AlertStream {
            reader: Some(response.into_body().into_with_config().reader()),
            decoder,
            deadline,
            control,
            idle_timeout: self.idle_timeout,
        })
    }

    /// Executes an explicit request within 15 seconds, without automatic network retries.
    ///
    /// Only an authentication challenge permits resending the request. A failed
    /// write may have taken effect on the camera; callers must read back its state.
    ///
    /// # Errors
    /// Returns authentication, HTTP status, transport, or size-limit errors.
    pub fn execute(&mut self, request: &Request) -> Result<Vec<u8>, Error> {
        let response = self.open(
            request,
            Some(Instant::now() + REQUEST_TIMEOUT),
            &self.agent.clone(),
        )?;
        if !response.status().is_success() {
            return Err(Error::new(Kind::HttpStatus(response.status().as_u16())));
        }
        let mut body = Vec::with_capacity(TRANSPORT_BUFFER_SIZE_BYTES);
        response
            .into_body()
            .into_with_config()
            .reader()
            .take(XML_SIZE_BYTES_MAX as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() > XML_SIZE_BYTES_MAX {
            return Err(Error::new(Kind::Limit));
        }
        Ok(body)
    }

    fn open(
        &mut self,
        request: &Request,
        deadline: Option<Instant>,
        agent: &ureq::Agent,
    ) -> Result<Response<ureq::Body>, Error> {
        for attempt in 0..AUTH_ATTEMPTS_MAX {
            if (self.cancelled)() {
                return Err(Error::new(Kind::Cancelled));
            }
            let client_nonce = format!("{:032x}", rand::random::<u128>());
            let authorization = self.session.authorization(request, &client_nonce)?;
            let response = self.send(request, authorization.as_ref(), deadline, agent)?;
            let status = response.status().as_u16();
            if status == 401 {
                let challenge = response
                    .headers()
                    .get_all("www-authenticate")
                    .iter()
                    .filter_map(|value| value.to_str().ok())
                    .find(|value| {
                        value
                            .split_once(' ')
                            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("Digest"))
                    })
                    .ok_or_else(|| Error::new(Kind::Authentication))?;
                let stale = self.session.handle_challenge(challenge)?;
                if (authorization.is_some() && !stale) || attempt + 1 == AUTH_ATTEMPTS_MAX {
                    return Err(Error::new(Kind::Authentication));
                }
                continue;
            }
            if response
                .headers()
                .get("content-encoding")
                .is_some_and(|value| value != "identity")
            {
                return Err(Error::new(Kind::InvalidInput));
            }
            return Ok(response);
        }
        Err(Error::new(Kind::Authentication))
    }

    fn send(
        &self,
        request: &Request,
        authorization: Option<&Authorization>,
        deadline: Option<Instant>,
        agent: &ureq::Agent,
    ) -> Result<Response<ureq::Body>, Error> {
        let remaining = deadline
            .map(|deadline| {
                deadline
                    .checked_duration_since(Instant::now())
                    .filter(|remaining| !remaining.is_zero())
                    .ok_or_else(|| Error::new(Kind::Io(std::io::ErrorKind::TimedOut)))
            })
            .transpose()?;
        let mut wire = HttpRequest::builder()
            .method(request.method().as_str())
            .uri(format!("{}{}", self.origin, request.resource()))
            .header("accept-encoding", "identity");
        if let Some(content_type) = request.content_type() {
            wire = wire.header("content-type", content_type);
        }
        if let Some(authorization) = authorization {
            wire = wire.header("authorization", authorization.as_str());
        }
        let wire = wire
            .body(request.body())
            .map_err(|_| Error::new(Kind::InvalidInput))?;
        Ok(wire
            .with_agent(agent)
            .configure()
            .timeout_global(remaining)
            .timeout_recv_response(None)
            .run()?)
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Client").finish_non_exhaustive()
    }
}

/// One bounded alert-stream connection; its decoder remains independent of this transport.
pub struct AlertStream {
    reader: Option<ureq::BodyReader<'static>>,
    decoder: Decoder,
    deadline: Option<Instant>,
    control: Control,
    idle_timeout: Duration,
}

impl AlertStream {
    /// Reads one part, or returns `None` after a cleanly framed stream end.
    ///
    /// Cancellation is observed during network waits at intervals of at most 100 ms.
    /// Partial input does not reset the complete-notification timeout.
    ///
    /// # Errors
    /// Returns framing, resource-limit, timeout, or transport failures. An error
    /// closes the response; subsequent calls return `None`.
    pub fn next_part(&mut self) -> Result<Option<Part>, Error> {
        self.next_part_until(Instant::now() + self.idle_timeout)
    }

    /// Reads one part without extending an absolute consumer-owned progress deadline.
    ///
    /// Use this to retain a notification deadline while consuming unrelated image parts.
    /// The configured idle timeout and optional total lifetime also remain enforced.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::next_part`], including an expired caller deadline.
    pub fn next_part_until(&mut self, deadline: Instant) -> Result<Option<Part>, Error> {
        if self.reader.is_none() {
            return Ok(None);
        }
        let result = self.read_next(deadline);
        if !matches!(&result, Ok(Some(_))) {
            self.reader = None;
        }
        result
    }

    fn read_next(&mut self, deadline: Instant) -> Result<Option<Part>, Error> {
        let mut buffer = [0; TRANSPORT_BUFFER_SIZE_BYTES];
        let progress_deadline = (Instant::now() + self.idle_timeout).min(deadline);
        self.control.set_deadline(Some(
            self.deadline
                .map_or(progress_deadline, |value| value.min(progress_deadline)),
        ));
        loop {
            self.control.check()?;
            if let Some(part) = self.decoder.next_part()? {
                return Ok(Some(part));
            }
            if self.decoder.is_finished() && self.decoder.finish().is_ok() {
                return Ok(None);
            }
            let count = self
                .reader
                .as_mut()
                .expect("read_next requires an open response")
                .read(&mut buffer)?;
            if count == 0 {
                self.decoder.finish()?;
                return Ok(None);
            }
            self.decoder.push(&buffer[..count])?;
        }
    }
}

fn make_agent(control: Control) -> ureq::Agent {
    let connector =
        ().chain(TcpConnector::default())
            .chain(Interruptible::new(control))
            .chain(NativeTlsConnector::default());
    ureq::Agent::with_parts(agent_config(1), connector, DefaultResolver::default())
}

fn agent_config(idle_connections: usize) -> ureq::config::Config {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_resolve(Some(CONNECT_TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_send_request(Some(CONNECT_TIMEOUT))
        .timeout_send_body(Some(CONNECT_TIMEOUT))
        .max_response_header_size(HEADER_SIZE_BYTES_MAX)
        .input_buffer_size(TRANSPORT_BUFFER_SIZE_BYTES)
        .output_buffer_size(TRANSPORT_BUFFER_SIZE_BYTES)
        .max_idle_connections(idle_connections)
        .max_idle_connections_per_host(idle_connections)
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .build(),
        )
        .build()
}

impl fmt::Debug for AlertStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AlertStream")
            .finish_non_exhaustive()
    }
}

fn validate_origin(origin: &str) -> Result<String, Error> {
    if origin.len() > 4096
        || origin.contains(['@', '\\', '#'])
        || origin.chars().any(char::is_whitespace)
        || origin.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(Error::new(Kind::InvalidInput));
    }
    let url = url::Url::parse(origin).map_err(|_| Error::new(Kind::InvalidInput))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::new(Kind::InvalidInput));
    }
    Ok(url.origin().ascii_serialization())
}
