//! A loopback-only Hikvision HTTP test device, independent of KeepPeek and the ISAPI client.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::Duration;

mod audio;
mod callback;
pub mod onvif;
mod resources;
mod wire;

pub use callback::CallbackResponse;
pub use wire::CapturedRequest;

const REQUESTS_MAX: usize = 256;
const CONNECTIONS_MAX: usize = 16;
const SCRIPT_BYTES_MAX: usize = 16 * 1024 * 1024;

/// A scripted response with optional delayed fragments or an indefinitely idle body.
#[derive(Clone, Debug)]
pub struct Reply {
    fragments: Vec<(Duration, Vec<u8>)>,
    hold: bool,
}

impl Reply {
    /// Sends a sequence of MIME parts using the same boundary as the fake alert endpoint.
    pub fn alert(parts: impl IntoIterator<Item = EventPart>, close: bool) -> Self {
        let mut bytes = b"HTTP/1.1 200 OK\r\nContent-Type: multipart/mixed; boundary=camera\r\nConnection: close\r\n\r\n".to_vec();
        for part in parts {
            part.append(&mut bytes);
        }
        if close {
            bytes.extend_from_slice(b"--camera--\r\n");
        }
        Self::raw(bytes)
    }
    /// Sends exact wire bytes, including deliberately malformed HTTP for negative tests.
    pub fn raw(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            fragments: vec![(Duration::ZERO, bytes.into())],
            hold: false,
        }
    }
    /// Sends a normal bounded HTTP response and closes its connection.
    pub fn http(status: u16, media_type: &str, body: impl AsRef<[u8]>) -> Self {
        let body = body.as_ref();
        let mut bytes = format!("HTTP/1.1 {status} Test\r\nContent-Type: {media_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
        bytes.extend_from_slice(body);
        Self::raw(bytes)
    }
    /// Appends a fragment after an interruptible delay, measured from the previous fragment.
    pub fn then(mut self, delay: Duration, bytes: impl Into<Vec<u8>>) -> Self {
        self.fragments.push((delay, bytes.into()));
        self
    }
    /// Keeps the response open after its last fragment until the fixture is dropped.
    pub const fn hold_open(mut self) -> Self {
        self.hold = true;
        self
    }
    /// Splits all bytes into small writes to exercise incremental client framing.
    pub fn fragmented(mut self, chunk_bytes: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (1..=65_536).contains(&chunk_bytes),
            "invalid fake-camera fragment size"
        );
        let mut fragments = Vec::new();
        for (delay, bytes) in self.fragments {
            for (index, chunk) in bytes.chunks(chunk_bytes).enumerate() {
                fragments.push((
                    if index == 0 { delay } else { Duration::ZERO },
                    chunk.to_vec(),
                ));
            }
        }
        self.fragments = fragments;
        Ok(self)
    }
}

/// A synthetic XML, JSON or JPEG MIME part shared by pull and callback scenarios.
#[derive(Clone, Debug)]
pub struct EventPart {
    content_type: String,
    content_id: Option<String>,
    body: Vec<u8>,
}

impl EventPart {
    /// Preserves the supplied content type and bytes, including legacy encodings.
    pub fn new(content_type: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            content_type: content_type.into(),
            content_id: None,
            body: body.into(),
        }
    }
    /// Associates a synthetic JPEG through an explicit Content-ID.
    pub fn identified(mut self, id: impl Into<String>) -> Self {
        self.content_id = Some(id.into());
        self
    }
    /// Creates a standard synthetic classified motion notification on channel one.
    pub fn motion(active: bool) -> Self {
        Self::new("application/xml", format!("<EventNotificationAlert><eventType>VMD</eventType><eventState>{}</eventState><channelID>1</channelID><detectionTarget>human</detectionTarget></EventNotificationAlert>", if active { "active" } else { "inactive" }).into_bytes())
    }
    /// Encodes a complete multipart/form-data callback with a fixed fixture boundary.
    pub fn callback_body(parts: impl IntoIterator<Item = Self>) -> Vec<u8> {
        let mut body = Vec::new();
        for part in parts {
            part.append(&mut body);
        }
        body.extend_from_slice(b"--camera--\r\n");
        body
    }
    fn append(self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(
            format!(
                "--camera\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
                self.content_type,
                self.body.len()
            )
            .as_bytes(),
        );
        if let Some(id) = self.content_id {
            bytes.extend_from_slice(format!("Content-ID: {id}\r\n").as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        bytes.extend(self.body);
        bytes.extend_from_slice(b"\r\n");
    }
}

/// Builds a stateful fake device or a scripted protocol-fault scenario.
pub struct Builder {
    username: String,
    password: String,
    digest: bool,
    scripts: VecDeque<Reply>,
    streams: VecDeque<Reply>,
}

impl Builder {
    /// Sets fixture-only credentials accepted by the fake Digest verifier.
    pub fn credentials(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = username.into();
        self.password = password.into();
        self
    }
    /// Enables or disables authentication for tests that target unrelated transport failures.
    pub const fn digest(mut self, enabled: bool) -> Self {
        self.digest = enabled;
        self
    }
    /// Queues a wire reply for the next request, before normal device routing or authentication.
    pub fn replies(mut self, replies: impl IntoIterator<Item = Reply>) -> Self {
        self.scripts.extend(replies);
        self
    }
    /// Sets one response per authenticated alert-stream connection, enabling reconnect scenarios.
    pub fn alert_streams(mut self, replies: impl IntoIterator<Item = Reply>) -> Self {
        self.streams.extend(replies);
        self
    }
    /// Starts on an OS-assigned IPv4 loopback port. No physical camera can be contacted.
    pub fn start(self) -> anyhow::Result<FakeHikvision> {
        anyhow::ensure!(
            !self.username.is_empty() && self.username.len() <= 128 && self.password.len() <= 256,
            "invalid fake credentials"
        );
        anyhow::ensure!(
            self.scripts.len() + self.streams.len() <= 128
                && self
                    .scripts
                    .iter()
                    .chain(&self.streams)
                    .flat_map(|reply| &reply.fragments)
                    .map(|(_, bytes)| bytes.len())
                    .sum::<usize>()
                    <= SCRIPT_BYTES_MAX,
            "fake-camera script exceeds limits"
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            stopped: AtomicBool::new(false),
            changed: Condvar::new(),
            state: Mutex::new(State {
                scripts: self.scripts,
                streams: self.streams,
                requests: Vec::new(),
                sockets: BTreeMap::new(),
                resources: resources::defaults()?,
                digest_counts: BTreeMap::new(),
                next_socket: 0,
                audio_session: None,
                next_audio_session: 1,
                audio_output: Vec::new(),
                audio_input: vec![0xff; 160],
                audio_input_after_output: 0,
            }),
            username: self.username,
            password: self.password,
            digest: self.digest,
        });
        let worker = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("fake-hikvision".to_owned())
            .spawn(move || serve(listener, &worker))?;
        Ok(FakeHikvision {
            address,
            shared,
            handle: Some(handle),
        })
    }
}

impl fmt::Debug for Builder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Builder")
            .field("script_count", &self.scripts.len())
            .finish_non_exhaustive()
    }
}

/// A stateful fake Hikvision HTTP device with deterministic RAII shutdown.
pub struct FakeHikvision {
    address: SocketAddr,
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl FakeHikvision {
    /// Uses Digest authentication and the fixture credentials `test` / `test` by default.
    pub fn builder() -> Builder {
        Builder {
            username: "test".to_owned(),
            password: "test".to_owned(),
            digest: true,
            scripts: VecDeque::new(),
            streams: VecDeque::new(),
        }
    }
    /// Returns the exact ephemeral listen address.
    pub const fn address(&self) -> SocketAddr {
        self.address
    }
    /// Returns a credential-free loopback HTTP origin.
    pub fn origin(&self) -> String {
        format!("http://{}", self.address)
    }
    /// Returns a bounded snapshot of received requests. Debug output redacts their bodies and headers.
    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.shared.state.lock().unwrap().requests.clone()
    }
    /// Waits for an observable request count, never for an arbitrary sleep interval.
    pub fn wait_for_requests(
        &self,
        count: usize,
        timeout: Duration,
    ) -> anyhow::Result<Vec<CapturedRequest>> {
        anyhow::ensure!(
            count <= REQUESTS_MAX,
            "fake request count exceeds history capacity"
        );
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(self.shared.state.lock().unwrap(), timeout, |state| {
                state.requests.len() < count && !self.shared.stopped.load(Ordering::Acquire)
            })
            .unwrap();
        anyhow::ensure!(
            state.requests.len() >= count,
            "fake camera did not receive expected requests"
        );
        Ok(state.requests.clone())
    }
    /// Queues a bounded response fault without restarting the fake camera.
    pub fn enqueue(&self, reply: Reply) -> anyhow::Result<()> {
        self.enqueue_reply(reply, false)
    }
    /// Queues a response for the next authenticated alert-stream subscription.
    pub fn enqueue_alert(&self, reply: Reply) -> anyhow::Result<()> {
        self.enqueue_reply(reply, true)
    }
    fn enqueue_reply(&self, reply: Reply, alert: bool) -> anyhow::Result<()> {
        let mut state = self.shared.state.lock().unwrap();
        let bytes = state
            .scripts
            .iter()
            .chain(&state.streams)
            .chain(std::iter::once(&reply))
            .flat_map(|reply| &reply.fragments)
            .map(|(_, bytes)| bytes.len())
            .sum::<usize>();
        anyhow::ensure!(
            state.scripts.len() + state.streams.len() < 128 && bytes <= SCRIPT_BYTES_MAX,
            "fake script capacity exceeded"
        );
        if alert {
            state.streams.push_back(reply);
        } else {
            state.scripts.push_back(reply);
        }
        Ok(())
    }
    /// Returns the current raw configuration bytes for independent read-back assertions.
    pub fn resource(&self, resource: &str) -> Option<Vec<u8>> {
        self.shared
            .state
            .lock()
            .unwrap()
            .resources
            .get(resource)
            .map(|resource| resource.body.clone())
    }

    /// Reports whether the synthetic audio channel currently has a session owner.
    pub fn audio_active(&self) -> bool {
        self.shared.state.lock().unwrap().audio_session.is_some()
    }

    /// Returns the original speaker bytes received after the latest audio handshake.
    pub fn audio_output(&self) -> Vec<u8> {
        self.shared.state.lock().unwrap().audio_output.clone()
    }

    /// Sets the original encoded microphone bytes for the next receive connection.
    ///
    /// # Errors
    /// Rejects more than five minutes of 8 kHz G.711 audio.
    pub fn set_audio_input(&self, bytes: &[u8]) -> anyhow::Result<()> {
        self.set_audio_input_after_output(bytes, 0)
    }

    /// Withholds microphone bytes until the speaker has received the requested count.
    ///
    /// # Errors
    /// Rejects byte counts greater than five minutes of 8 kHz G.711 audio.
    pub fn set_audio_input_after_output(&self, bytes: &[u8], minimum: usize) -> anyhow::Result<()> {
        anyhow::ensure!(
            bytes.len() <= 2_400_000,
            "fake microphone bytes exceed limit"
        );
        anyhow::ensure!(minimum <= 2_400_000, "fake microphone gate exceeds limit");
        let mut state = self.shared.state.lock().unwrap();
        state.audio_input = bytes.to_vec();
        state.audio_input_after_output = minimum;
        Ok(())
    }

    /// Waits at most 60 seconds for bounded speaker-byte evidence.
    pub fn wait_for_audio_bytes(&self, minimum: usize, timeout: Duration) -> bool {
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(
                self.shared.state.lock().unwrap(),
                timeout.min(Duration::from_secs(60)),
                |state| {
                    state.audio_output.len() < minimum
                        && !self.shared.stopped.load(Ordering::Acquire)
                },
            )
            .unwrap();
        state.audio_output.len() >= minimum
    }

    /// Replaces a synthetic XML resource without sending a camera command.
    ///
    /// # Errors
    /// Rejects invalid paths, oversized bodies and more than 128 resources.
    pub fn set_resource(&self, path: &str, body: impl AsRef<[u8]>) -> anyhow::Result<()> {
        let body = body.as_ref();
        anyhow::ensure!(
            path.starts_with("/ISAPI/")
                && path.len() <= 4096
                && !path.contains(['?', '#', '\\'])
                && !path.chars().any(char::is_control),
            "invalid fake resource path"
        );
        anyhow::ensure!(body.len() <= 256 * 1024, "fake resource body exceeds limit");
        let mut state = self.shared.state.lock().unwrap();
        anyhow::ensure!(
            state.resources.contains_key(path) || state.resources.len() < 128,
            "fake resource capacity exceeded"
        );
        state.resources.insert(
            path.to_owned(),
            resources::Resource {
                body: body.to_vec(),
                media: "application/xml",
            },
        );
        Ok(())
    }
}

impl fmt::Debug for FakeHikvision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeHikvision")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Drop for FakeHikvision {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        for socket in self.shared.state.lock().unwrap().sockets.values() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        self.shared.changed.notify_all();
        if let Some(handle) = self.handle.take() {
            handle.join().expect("fake camera coordinator panicked");
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    stopped: AtomicBool,
    username: String,
    password: String,
    digest: bool,
}
struct State {
    scripts: VecDeque<Reply>,
    streams: VecDeque<Reply>,
    requests: Vec<CapturedRequest>,
    sockets: BTreeMap<u64, TcpStream>,
    resources: BTreeMap<String, resources::Resource>,
    digest_counts: BTreeMap<String, u32>,
    next_socket: u64,
    audio_session: Option<u64>,
    next_audio_session: u64,
    audio_output: Vec<u8>,
    audio_input: Vec<u8>,
    audio_input_after_output: usize,
}

impl Shared {
    fn wait(&self, duration: Duration) -> bool {
        let (guard, _) = self
            .changed
            .wait_timeout_while(self.state.lock().unwrap(), duration, |_| {
                !self.stopped.load(Ordering::Acquire)
            })
            .unwrap();
        drop(guard);
        self.stopped.load(Ordering::Acquire)
    }
}

fn serve(listener: TcpListener, shared: &Arc<Shared>) {
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    while !shared.stopped.load(Ordering::Acquire) {
        for index in (0..workers.len()).rev() {
            if workers[index].is_finished() {
                workers
                    .remove(index)
                    .join()
                    .expect("fake camera worker panicked");
            }
        }
        match listener.accept() {
            Ok((socket, _)) if workers.len() < CONNECTIONS_MAX => {
                let Some(id) = register(shared, &socket) else {
                    continue;
                };
                let worker = Arc::clone(shared);
                workers.push(std::thread::spawn(move || {
                    let _ = wire::handle(socket, &worker);
                    worker.state.lock().unwrap().sockets.remove(&id);
                    worker.changed.notify_all();
                }));
            }
            Ok((socket, _)) => {
                let _ = socket.shutdown(Shutdown::Both);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                shared.wait(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
    for worker in workers {
        worker.join().expect("fake camera worker panicked");
    }
}

fn register(shared: &Shared, socket: &TcpStream) -> Option<u64> {
    socket.set_nonblocking(false).ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    socket.set_nodelay(true).ok()?;
    let socket = socket.try_clone().ok()?;
    let mut state = shared.state.lock().unwrap();
    let id = state.next_socket;
    state.next_socket += 1;
    state.sockets.insert(id, socket);
    Some(id)
}

#[cfg(test)]
mod socket_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn accepted_connection_waits_for_delayed_request_headers() {
        let camera = FakeHikvision::builder().digest(false).start().unwrap();
        let mut client = TcpStream::connect(camera.address()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let started = std::time::Instant::now();
        let mut peek = [0];
        let waiting = client
            .peek(&mut peek)
            .expect_err("server must keep the idle connection open");
        assert!(matches!(
            waiting.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        assert!(started.elapsed() >= Duration::from_millis(80));
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.write_all(b"GET /ISAPI/System/deviceInfo HTTP/1.1\r\nHost: camera\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert_eq!(camera.requests().len(), 1);
    }
}
