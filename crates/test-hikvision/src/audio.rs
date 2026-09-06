use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{CapturedRequest, Shared};

pub fn handle(
    socket: &mut TcpStream,
    request: &CapturedRequest,
    shared: &Shared,
) -> anyhow::Result<bool> {
    let Some((path, query)) = request.target().split_once('?') else {
        return Ok(false);
    };
    if path != "/ISAPI/System/TwoWayAudio/channels/1/audioData" {
        return Ok(false);
    }
    let pairs = url::form_urlencoded::parse(query.as_bytes()).collect::<Vec<_>>();
    let session = shared.state.lock().unwrap().audio_session;
    let valid = session.is_some_and(|session| {
        pairs.len() == 1 && pairs[0].0 == "sessionId" && pairs[0].1 == session.to_string()
    });
    let forbidden = if request.method() == "PUT" {
        "lineOutForbidden"
    } else {
        "micInForbidden"
    };
    let disabled = {
        let state = shared.state.lock().unwrap();
        let resource = &state.resources["/ISAPI/System/TwoWayAudio/channels/1"];
        super::resources::xml_field(&resource.body, forbidden)?.as_deref() == Some("true")
    };
    if !valid || disabled || !matches!(request.method(), "PUT" | "GET") {
        for (_, bytes) in super::resources::status(409, 4, "invalidOperation").fragments {
            socket.write_all(&bytes)?;
        }
        return Ok(true);
    }
    if request.method() == "GET" {
        receive(socket, session, shared)?;
    } else {
        send(socket, request, session, shared)?;
    }
    Ok(true)
}

fn receive(socket: &mut TcpStream, session: Option<u64>, shared: &Shared) -> anyhow::Result<()> {
    let (bytes, codec) = {
        let state = shared.state.lock().unwrap();
        let body = &state.resources["/ISAPI/System/TwoWayAudio/channels/1"].body;
        let codec = super::resources::xml_field(body, "audioInboundCompressionType")?
            .or(super::resources::xml_field(body, "audioCompressionType")?);
        (state.audio_input.clone(), codec)
    };
    let media = match codec.as_deref() {
        Some("G.711ulaw") => "audio/basic",
        Some("G.711alaw") => "audio/pcma",
        _ => "application/octet-stream",
    };
    socket.write_all(
        format!("HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let (state, _) = shared
        .changed
        .wait_timeout_while(
            shared.state.lock().unwrap(),
            Duration::from_secs(2),
            |state| {
                state.audio_output.len() < state.audio_input_after_output
                    && state.audio_session == session
                    && !shared.stopped.load(Ordering::Acquire)
            },
        )
        .unwrap();
    if state.audio_session != session || shared.stopped.load(Ordering::Acquire) {
        return Ok(());
    }
    anyhow::ensure!(
        state.audio_output.len() >= state.audio_input_after_output,
        "fake microphone gate timed out"
    );
    drop(state);
    socket.write_all(&bytes)?;
    let (guard, _) = shared
        .changed
        .wait_timeout_while(
            shared.state.lock().unwrap(),
            Duration::from_secs(300),
            |state| state.audio_session == session && !shared.stopped.load(Ordering::Acquire),
        )
        .unwrap();
    drop(guard);
    Ok(())
}

fn send(
    socket: &mut TcpStream,
    request: &CapturedRequest,
    session: Option<u64>,
    shared: &Shared,
) -> anyhow::Result<()> {
    if request.header("content-length") != Some("0")
        || request.header("content-type") != Some("application/octet-stream")
        || !request.body().is_empty()
    {
        for (_, bytes) in super::resources::status(400, 4, "invalidOperation").fragments {
            socket.write_all(&bytes)?;
        }
        return Ok(());
    }
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n")?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut buffer = [0; 1600];
    while Instant::now() < deadline && !shared.stopped.load(Ordering::Acquire) {
        if shared.state.lock().unwrap().audio_session != session {
            break;
        }
        match socket.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let mut state = shared.state.lock().unwrap();
                if state.audio_session != session {
                    break;
                }
                anyhow::ensure!(
                    state.audio_output.len() + count <= 2_400_000,
                    "fake audio capture exceeds its limit"
                );
                state.audio_output.extend_from_slice(&buffer[..count]);
                shared.changed.notify_all();
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
