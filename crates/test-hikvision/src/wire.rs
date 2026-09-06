use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use digest_auth::{Algorithm, AlgorithmType, AuthContext, AuthorizationHeader, HttpMethod, Qop};
use subtle::ConstantTimeEq;

use super::{REQUESTS_MAX, Reply, Shared};

const NONCE: &str = "0123456789abcdef0123456789abcdef";

/// One captured fixture request. Explicit accessors expose wire evidence for assertions.
#[derive(Clone)]
pub struct CapturedRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    authenticated: bool,
}

impl CapturedRequest {
    /// Returns the received HTTP method.
    pub fn method(&self) -> &str {
        &self.method
    }
    /// Returns the exact received path and query.
    pub fn target(&self) -> &str {
        &self.target
    }
    /// Returns a named header for protocol assertions. Never log real credentials.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
    /// Returns the request body for independent serialization assertions.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Reports actual Digest verification, not merely the presence of a header.
    pub const fn authenticated(&self) -> bool {
        self.authenticated
    }
}
impl fmt::Debug for CapturedRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedRequest")
            .field("method", &self.method)
            .field("authenticated", &self.authenticated)
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

pub fn handle(mut socket: TcpStream, shared: &Shared) -> anyhow::Result<()> {
    let mut request = read(&socket)?;
    request.authenticated = authenticate(&request, shared);
    let scripted = {
        let mut state = shared.state.lock().unwrap();
        anyhow::ensure!(
            state.requests.len() < REQUESTS_MAX,
            "fake request history exhausted"
        );
        state.requests.push(request.clone());
        state.scripts.pop_front()
    };
    shared.changed.notify_all();
    let reply = if let Some(reply) = scripted {
        reply
    } else if shared.digest && !request.authenticated {
        Reply::raw(format!("HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"fake-hikvision\", nonce=\"{NONCE}\", algorithm=MD5, qop=\"auth\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes())
    } else if super::audio::handle(&mut socket, &request, shared)? {
        return Ok(());
    } else {
        super::resources::route(&request, &mut shared.state.lock().unwrap())?
    };
    shared.changed.notify_all();
    for (delay, bytes) in reply.fragments {
        anyhow::ensure!(
            delay <= Duration::from_secs(60),
            "fake response delay exceeds limit"
        );
        if (!delay.is_zero() && shared.wait(delay)) || shared.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        socket.write_all(&bytes)?;
    }
    if reply.hold {
        shared.wait(Duration::from_secs(60));
    }
    Ok(())
}

fn read(socket: &TcpStream) -> anyhow::Result<CapturedRequest> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut reader = BufReader::new(socket);
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(
            bytes.len() < 16 * 1024 && Instant::now() < deadline,
            "fake request headers exceeded limits"
        );
        reader.read_exact(&mut byte)?;
        bytes.push(byte[0]);
    }
    let mut slots = [httparse::EMPTY_HEADER; 32];
    let mut parsed = httparse::Request::new(&mut slots);
    anyhow::ensure!(
        parsed.parse(&bytes)?.is_complete(),
        "fake received incomplete headers"
    );
    let mut headers = BTreeMap::new();
    for header in parsed.headers {
        anyhow::ensure!(
            headers
                .insert(
                    header.name.to_ascii_lowercase(),
                    std::str::from_utf8(header.value)?.to_owned()
                )
                .is_none(),
            "fake received duplicate headers"
        );
    }
    anyhow::ensure!(
        !headers.contains_key("transfer-encoding"),
        "fake requires explicit request length"
    );
    let length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    anyhow::ensure!(length <= 256 * 1024, "fake request body exceeded limit");
    let mut body = vec![0; length];
    for chunk in body.chunks_mut(8192) {
        anyhow::ensure!(Instant::now() < deadline, "fake request deadline expired");
        reader.read_exact(chunk)?;
    }
    Ok(CapturedRequest {
        method: parsed.method.unwrap_or_default().to_owned(),
        target: parsed.path.unwrap_or_default().to_owned(),
        headers,
        body,
        authenticated: false,
    })
}

fn authenticate(request: &CapturedRequest, shared: &Shared) -> bool {
    let Some(value) = request.header("authorization") else {
        return false;
    };
    let Ok(mut auth) = AuthorizationHeader::parse(value) else {
        return false;
    };
    if auth.realm != "fake-hikvision"
        || auth.nonce != NONCE
        || auth.username != shared.username
        || auth.uri != request.target
        || auth.qop != Some(Qop::AUTH)
        || auth.cnonce.is_none()
        || auth.algorithm != Algorithm::new(AlgorithmType::MD5, false)
    {
        return false;
    }
    let method = match request.method.as_str() {
        "GET" => HttpMethod::GET,
        "PUT" => HttpMethod::PUT,
        "POST" => HttpMethod::POST,
        "DELETE" => HttpMethod::DELETE,
        _ => return false,
    };
    let supplied = auth.response.clone();
    let context = AuthContext::new_with_method(
        shared.username.as_str(),
        shared.password.as_str(),
        request.target.as_str(),
        Some(request.body.as_slice()),
        method,
    );
    auth.digest(&context);
    if !bool::from(supplied.as_bytes().ct_eq(auth.response.as_bytes())) {
        return false;
    }
    let mut state = shared.state.lock().unwrap();
    let key = auth.cnonce.expect("verified callback client nonce exists");
    if state.digest_counts.len() >= 256 && !state.digest_counts.contains_key(&key) {
        return false;
    }
    let count = state.digest_counts.entry(key).or_default();
    if auth.nc <= *count {
        return false;
    }
    *count = auth.nc;
    true
}
