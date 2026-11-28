use super::{
    BrokerOptions, ca_read_failure, network_unavailable, timeout_failure, tls_validation_failure,
};
use crate::event_forwarder::BrokerFailure;
use native_tls::{Certificate, HandshakeError, TlsConnector, TlsStream};
use polling::{Event as PollEvent, Events, Poller};
use std::{
    fs::File,
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    num::NonZeroUsize,
    path::Path,
    time::{Duration, Instant},
};

/// Maximum resolved socket addresses tried during one TCP connect attempt.
const CONNECT_ADDRESS_COUNT_MAX: usize = 32;

/// Maximum bytes read from one configured custom CA file.
pub(super) const CA_FILE_BYTES_MAX: usize = 1_024 * 1_024;

/// Maximum PEM certificate blocks loaded from one custom CA bundle file.
const CA_CERTIFICATE_COUNT_MAX: usize = 256;

/// Maximum handshake retry steps allowed before failing TLS negotiation.
const TLS_HANDSHAKE_STEP_COUNT_MAX: usize = 256;

/// Minimum per-address TCP connect slice when multiple addresses resolve for one host.
const CONNECT_SLICE_MS_MIN: u64 = 250;

const TLS_HANDSHAKE_POLL_KEY: usize = 1;

#[derive(Debug, Clone, Copy, Default)]
struct HandshakeIoNeed {
    read: bool,
    write: bool,
}

pub(super) struct TrackedTcpStream {
    stream: TcpStream,
    need: HandshakeIoNeed,
}

impl TrackedTcpStream {
    const fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            need: HandshakeIoNeed {
                read: false,
                write: false,
            },
        }
    }

    const fn as_tcp(&self) -> &TcpStream {
        &self.stream
    }

    fn take_need(&mut self) -> HandshakeIoNeed {
        std::mem::take(&mut self.need)
    }
}

impl Read for TrackedTcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stream.read(buf) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.need.read = true;
                Err(error)
            }
            result => result,
        }
    }
}

impl Write for TrackedTcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.stream.write(buf) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.need.write = true;
                Err(error)
            }
            result => result,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.stream.flush() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.need.write = true;
                Err(error)
            }
            result => result,
        }
    }
}

struct SocketPollGuard<'a> {
    poller: &'a Poller,
    stream: &'a TcpStream,
}

impl Drop for SocketPollGuard<'_> {
    fn drop(&mut self) {
        let _ = self.poller.delete(self.stream);
    }
}

pub(super) enum BrokerTransport {
    Plain(TcpStream),
    Tls(Box<TlsStream<TrackedTcpStream>>),
}

impl BrokerTransport {
    pub(super) fn connect(
        options: &BrokerOptions,
        deadline: Instant,
    ) -> Result<Self, BrokerFailure> {
        let tcp = connect_tcp(&options.host, options.port, deadline)?;
        if !options.use_tls {
            return Ok(Self::Plain(tcp));
        }
        let connector = build_tls_connector(options.tls_ca_pem.as_deref())?;
        let tls = finish_tls_handshake(&connector, &options.host, tcp, deadline)?;
        Ok(Self::Tls(Box::new(tls)))
    }

    pub(super) fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_read_timeout(timeout),
            Self::Tls(stream) => stream.get_ref().as_tcp().set_read_timeout(timeout),
        }
    }

    pub(super) fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_write_timeout(timeout),
            Self::Tls(stream) => stream.get_ref().as_tcp().set_write_timeout(timeout),
        }
    }

    pub(super) fn shutdown(&mut self, deadline: Instant, send_tls_close_notify: bool) {
        match self {
            Self::Plain(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
            Self::Tls(stream) => {
                if send_tls_close_notify && let Ok(timeout) = socket_timeout(deadline) {
                    let tcp = stream.get_ref().as_tcp();
                    if tcp.set_read_timeout(Some(timeout)).is_ok()
                        && tcp.set_write_timeout(Some(timeout)).is_ok()
                    {
                        let _ = stream.shutdown();
                    }
                }
                let _ = stream.get_ref().as_tcp().shutdown(Shutdown::Both);
            }
        }
    }
}

impl Read for BrokerTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for BrokerTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

pub(super) fn read_ca_file(path: &Path) -> Result<Vec<u8>, BrokerFailure> {
    let file = File::open(path).map_err(|_| ca_read_failure())?;
    let limit = u64::try_from(CA_FILE_BYTES_MAX)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|_| ca_read_failure())?;
    if bytes.len() > CA_FILE_BYTES_MAX {
        return Err(tls_validation_failure());
    }
    Ok(bytes)
}

pub(super) fn socket_timeout(deadline: Instant) -> Result<Duration, BrokerFailure> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(timeout_failure)?;
    // Floor at 1 ms so Winsock SO_RCVTIMEO/SO_SNDTIMEO and POSIX timeval cannot truncate to zero.
    Ok(remaining.max(Duration::from_millis(1)))
}

pub(super) fn is_timeout_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

fn connect_tcp(host: &str, port: u16, deadline: Instant) -> Result<TcpStream, BrokerFailure> {
    let addresses = resolve_addresses(host, port)?;
    if addresses.is_empty() {
        return Err(network_unavailable());
    }
    let mut saw_timeout = false;
    for (index, address) in addresses.iter().enumerate() {
        let remaining = socket_timeout(deadline)?;
        let remaining_addresses = u32::try_from(addresses.len() - index).unwrap_or(1);
        let slice = if remaining_addresses > 1 {
            (remaining / remaining_addresses)
                .max(Duration::from_millis(CONNECT_SLICE_MS_MIN))
                .min(remaining)
        } else {
            remaining
        };
        match TcpStream::connect_timeout(address, slice) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) if is_timeout_error(&error) => saw_timeout = true,
            Err(_) => {}
        }
    }
    if saw_timeout || Instant::now() >= deadline {
        Err(timeout_failure())
    } else {
        Err(network_unavailable())
    }
}

fn resolve_addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, BrokerFailure> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    // ponytail: std ToSocketAddrs is synchronous; avoid spawning uncancellable DNS threads per retry.
    Ok((host, port)
        .to_socket_addrs()
        .map_err(|_| network_unavailable())?
        .take(CONNECT_ADDRESS_COUNT_MAX)
        .collect())
}

fn build_tls_connector(ca_pem: Option<&[u8]>) -> Result<TlsConnector, BrokerFailure> {
    let mut builder = TlsConnector::builder();
    if let Some(pem_bytes) = ca_pem {
        let certificates = parse_pem_certificates(pem_bytes)?;
        builder.disable_built_in_roots(true);
        for certificate in certificates {
            builder.add_root_certificate(certificate);
        }
    }
    builder.build().map_err(|_| tls_validation_failure())
}

fn parse_pem_certificates(pem_bytes: &[u8]) -> Result<Vec<Certificate>, BrokerFailure> {
    // Validate UTF-8 first because native-tls on Windows assumes UTF-8 PEM input.
    let _ = std::str::from_utf8(pem_bytes).map_err(|_| tls_validation_failure())?;
    let certificates =
        Certificate::stack_from_pem(pem_bytes).map_err(|_| tls_validation_failure())?;
    if certificates.is_empty() || certificates.len() > CA_CERTIFICATE_COUNT_MAX {
        return Err(tls_validation_failure());
    }
    Ok(certificates)
}

fn finish_tls_handshake(
    connector: &TlsConnector,
    host: &str,
    tcp: TcpStream,
    deadline: Instant,
) -> Result<TlsStream<TrackedTcpStream>, BrokerFailure> {
    let _ = socket_timeout(deadline)?;
    tcp.set_read_timeout(None)
        .map_err(|_| network_unavailable())?;
    tcp.set_write_timeout(None)
        .map_err(|_| network_unavailable())?;
    tcp.set_nonblocking(true)
        .map_err(|_| network_unavailable())?;

    let poller = Poller::new().map_err(|_| network_unavailable())?;
    let capacity = NonZeroUsize::new(1).expect("event capacity must be non-zero");
    let mut events = Events::with_capacity(capacity);
    let mut result = connector.connect(host, TrackedTcpStream::new(tcp));

    for _ in 0..TLS_HANDSHAKE_STEP_COUNT_MAX {
        match result {
            Ok(stream) => {
                stream
                    .get_ref()
                    .as_tcp()
                    .set_nonblocking(false)
                    .map_err(|_| network_unavailable())?;
                return Ok(stream);
            }
            Err(HandshakeError::WouldBlock(mut mid)) => {
                let need = mid.get_mut().take_need();
                // macOS Secure Transport can pause for certificate evaluation without socket I/O.
                if need.read || need.write {
                    wait_for_socket_ready(
                        &poller,
                        &mut events,
                        mid.get_ref().as_tcp(),
                        need,
                        deadline,
                    )?;
                } else {
                    let _ = socket_timeout(deadline)?;
                }
                result = mid.handshake();
            }
            Err(HandshakeError::Failure(_)) => return Err(tls_validation_failure()),
        }
    }
    Err(timeout_failure())
}

fn wait_for_socket_ready(
    poller: &Poller,
    events: &mut Events,
    stream: &TcpStream,
    need: HandshakeIoNeed,
    deadline: Instant,
) -> Result<(), BrokerFailure> {
    let interest = PollEvent::new(TLS_HANDSHAKE_POLL_KEY, need.read, need.write);
    // SAFETY: `guard` deletes `stream` from `poller` before this function returns or unwinds.
    unsafe {
        poller
            .add(stream, interest)
            .map_err(|_| network_unavailable())?;
    }
    let _guard = SocketPollGuard { poller, stream };
    loop {
        let wait = socket_timeout(deadline)?;
        events.clear();
        match poller.wait(events, Some(wait)) {
            Ok(0) => {
                if Instant::now() >= deadline {
                    return Err(timeout_failure());
                }
            }
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(network_unavailable()),
        }
    }
}
