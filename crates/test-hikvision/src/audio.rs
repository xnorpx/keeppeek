use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{CapturedRequest, Shared};

const PEER_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

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
    let mut deadline = Instant::now() + Duration::from_secs(300);
    let mut buffer = [0; 1600];
    while Instant::now() < deadline && !shared.stopped.load(Ordering::Acquire) {
        if shared.state.lock().unwrap().audio_session != session {
            if !shared.observe_upload_closure {
                break;
            }
            deadline = deadline.min(Instant::now() + PEER_CLOSE_TIMEOUT);
        }
        match socket.read(&mut buffer) {
            Ok(0) => {
                note_peer_closure(shared);
                break;
            }
            Ok(count) => {
                let mut state = shared.state.lock().unwrap();
                if state.audio_session != session {
                    if shared.observe_upload_closure {
                        continue;
                    }
                    break;
                }
                anyhow::ensure!(
                    state.audio_output.len() + count <= 2_400_000,
                    "fake audio capture exceeds its limit"
                );
                state.speaker_signal.record(&buffer[..count]);
                state.audio_output.extend_from_slice(&buffer[..count]);
                shared.changed.notify_all();
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error)
                if shared.observe_upload_closure
                    && error.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                note_peer_closure(shared);
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn note_peer_closure(shared: &Shared) {
    if shared.observe_upload_closure && !shared.stopped.load(Ordering::Acquire) {
        let mut state = shared.state.lock().unwrap();
        state.audio_uploads_closed_by_peer = state.audio_uploads_closed_by_peer.saturating_add(1);
    }
}

#[derive(Default)]
pub struct SpeakerSignal {
    pub(super) audible: usize,
    pub(super) crossings: usize,
    previous_positive: Option<bool>,
}

impl SpeakerSignal {
    fn record(&mut self, samples: &[u8]) {
        // The fixture negotiates mu-law. Exact zero has two sign encodings.
        // Keep those samples in the elapsed time without counting their signs.
        for &sample in samples {
            let magnitude = sample & 0x7f;
            self.audible += usize::from(magnitude < 0x60);
            if magnitude != 0x7f {
                let positive = sample & 0x80 != 0;
                self.crossings +=
                    usize::from(self.previous_positive.is_some_and(|last| last != positive));
                self.previous_positive = Some(positive);
            }
        }
    }
}

#[cfg(test)]
mod speaker_tests {
    use super::*;

    #[test]
    fn speaker_crossings_ignore_signed_zero_and_span_upload_chunks() {
        let mut signal = SpeakerSignal::default();
        signal.record(&[0x80, 0x7f, 0xff, 0xfe]);
        signal.record(&[0x7e, 0xff]);
        signal.record(&[0x00, 0x80]);
        assert_eq!(signal.crossings, 2);
        assert_eq!(signal.audible, 3);
    }
}
