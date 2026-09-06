use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use digest_auth::{Algorithm, AlgorithmType, AuthContext, AuthorizationHeader, Qop};
use subtle::ConstantTimeEq;

use super::{CapturedRequest, IO_TIMEOUT, PULL_WAIT_MAX, Shared, soap};
use crate::Reply;

const NONCE: &str = "90123456789abcdef0123456789abcdef";
const HEADERS_BYTES_MAX: usize = 16 * 1024;
const READ_WAIT_MAX: Duration = Duration::from_millis(50);
pub(super) const BODY_BYTES_MAX: usize = 256 * 1024;

pub(super) fn handle(mut socket: TcpStream, shared: &Shared) -> anyhow::Result<()> {
    let mut request = read(&mut socket, shared)?;
    request.authenticated = authenticate(&request, shared);
    shared.capture(request.clone());
    let reply = if request.authenticated {
        let scripted = shared.state.lock().unwrap().responses.pop_front();
        scripted.unwrap_or_else(|| soap::route(&request, shared))
    } else {
        Reply::raw(format!("HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"fake-onvif\", nonce=\"{NONCE}\", algorithm=MD5, qop=\"auth\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes())
    };
    let started = Instant::now();
    let deadline = started + IO_TIMEOUT;
    for (delay, bytes) in reply.fragments {
        if shared.stopped() || (!delay.is_zero() && shared.wait(delay)) {
            return Ok(());
        }
        write(&mut socket, &bytes, deadline)?;
    }
    if reply.hold {
        shared.wait(PULL_WAIT_MAX.saturating_sub(started.elapsed()));
    }
    Ok(())
}

fn read(socket: &mut TcpStream, shared: &Shared) -> anyhow::Result<CapturedRequest> {
    let deadline = Instant::now() + IO_TIMEOUT;
    let bytes = read_headers(socket, deadline, shared)?;
    let mut slots = [httparse::EMPTY_HEADER; 32];
    let mut parsed = httparse::Request::new(&mut slots);
    let httparse::Status::Complete(offset) = parsed.parse(&bytes)? else {
        anyhow::bail!("incomplete fake ONVIF request headers");
    };
    let mut headers = BTreeMap::new();
    for header in parsed.headers {
        let previous = headers.insert(
            header.name.to_ascii_lowercase(),
            std::str::from_utf8(header.value)?.to_owned(),
        );
        anyhow::ensure!(previous.is_none(), "duplicate fake ONVIF request header");
    }
    anyhow::ensure!(
        !headers.contains_key("transfer-encoding"),
        "explicit request length required"
    );
    let length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    anyhow::ensure!(
        length <= BODY_BYTES_MAX,
        "fake ONVIF request body limit exceeded"
    );
    let mut body = vec![0; length];
    let available = (bytes.len() - offset).min(length);
    body[..available].copy_from_slice(&bytes[offset..offset + available]);
    let mut filled = available;
    while filled < length {
        filled += read_some(socket, &mut body[filled..], deadline, shared)?;
    }
    Ok(CapturedRequest {
        method: parsed.method.unwrap_or_default().to_owned(),
        target: parsed.path.unwrap_or_default().to_owned(),
        headers,
        body,
        authenticated: false,
    })
}

fn read_headers(
    socket: &mut TcpStream,
    deadline: Instant,
    shared: &Shared,
) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(4096);
    let mut chunk = [0; 1024];
    while !bytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        anyhow::ensure!(
            bytes.len() < HEADERS_BYTES_MAX,
            "fake ONVIF request header limit exceeded"
        );
        let capacity = chunk.len().min(HEADERS_BYTES_MAX - bytes.len());
        let read = read_some(socket, &mut chunk[..capacity], deadline, shared)?;
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "fake ONVIF I/O deadline expired"))
}

fn read_some(
    socket: &mut TcpStream,
    bytes: &mut [u8],
    deadline: Instant,
    shared: &Shared,
) -> io::Result<usize> {
    loop {
        if shared.stopped() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "fake ONVIF device stopped",
            ));
        }
        socket.set_read_timeout(Some(remaining(deadline)?.min(READ_WAIT_MAX)))?;
        let result = socket.read(bytes);
        if shared.stopped() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "fake ONVIF device stopped",
            ));
        }
        match result {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete fake ONVIF request",
                ));
            }
            Ok(count) => return Ok(count),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
}

fn write(socket: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        socket.set_write_timeout(Some(remaining(deadline)?))?;
        let count = socket.write(bytes)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "fake ONVIF socket stopped accepting data",
            ));
        }
        bytes = &bytes[count..];
    }
    Ok(())
}

fn authenticate(request: &CapturedRequest, shared: &Shared) -> bool {
    let Some(value) = request.header("authorization") else {
        return false;
    };
    let Ok(mut auth) = AuthorizationHeader::parse(value) else {
        return false;
    };
    if request.method != "POST"
        || auth.realm != "fake-onvif"
        || auth.nonce != NONCE
        || auth.username != shared.config.username
        || auth.uri != request.target
        || auth.qop != Some(Qop::AUTH)
        || auth
            .cnonce
            .as_deref()
            .is_none_or(|value| value.is_empty() || value.len() > 128)
        || auth.algorithm != Algorithm::new(AlgorithmType::MD5, false)
    {
        return false;
    }
    let supplied = auth.response.clone();
    auth.digest(&AuthContext::new_post(
        shared.config.username.as_str(),
        shared.config.password.as_str(),
        request.target.as_str(),
        Some(request.body.as_slice()),
    ));
    if !bool::from(supplied.as_bytes().ct_eq(auth.response.as_bytes())) {
        return false;
    }
    let key = auth
        .cnonce
        .expect("validated fake ONVIF client nonce exists");
    let mut state = shared.state.lock().unwrap();
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
