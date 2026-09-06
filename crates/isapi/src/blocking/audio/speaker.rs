use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ureq::http::Response;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{Connector, NativeTlsConnector, NextTimeout, Transport};

use super::Talk;
use super::capture::Capture;
use super::tcp::AudioTcp;
use crate::blocking::{Control, Interruptible, REQUEST_TIMEOUT, agent_config};
use crate::error::Kind;
use crate::management::{AudioCodec, ResponseStatus};
use crate::{Error, XML_SIZE_BYTES_MAX};

const FRAME_BYTES_MAX: usize = 1600;
const WRITE_TIMEOUT: Duration = Duration::from_millis(100);

/// A paced raw G.711 speaker connection, separate from the HTTP connection pool.
pub struct Speaker {
    transport: Option<Box<dyn Transport>>,
    codec: AudioCodec,
    control: Control,
    deadline: Instant,
    next_send: Instant,
    remaining_bytes: u64,
}

impl Talk {
    /// Opens this session's only speaker upload connection.
    ///
    /// The camera must accept the ISAPI empty PUT handshake before any audio is
    /// sent. The returned handle may move to a worker thread. Closing or dropping
    /// this session cancels the handle before it can write further bytes.
    ///
    /// # Errors
    /// Rejects duplicate handles, unavailable speakers, unsupported codecs and
    /// expired sessions. Also returns handshake and transport errors.
    pub fn speaker(&mut self) -> Result<Speaker, Error> {
        if self.is_stopped() {
            return Err(Error::new(Kind::Cancelled));
        }
        if self.speaker_opened
            || !self.channel.speaker_supported()
            || self.channel.output_codec().sample_rate_hz().is_none()
        {
            return Err(Error::new(Kind::InvalidInput));
        }
        self.speaker_opened = true;
        let setup = self.deadline.min(Instant::now() + REQUEST_TIMEOUT);
        let control = Control::new(Arc::clone(&self.cancelled), Some(setup));
        let capture = Capture::default();
        let connector =
            ().chain(AudioTcp(control.clone()))
                .chain(Interruptible::new(control.clone()))
                .chain(NativeTlsConnector::default())
                .chain(capture.clone());
        let agent = ureq::Agent::with_parts(agent_config(0), connector, DefaultResolver::default());
        let response = self.client.open(
            &self.session.send_request(self.channel.id())?,
            Some(setup),
            &agent,
        )?;
        validate_upload(response)?;
        drop(agent);
        let transport = capture.take().ok_or_else(|| Error::new(Kind::Protocol))?;
        control.set_deadline(Some(self.deadline));
        let remaining_bytes = u64::try_from(
            self.deadline
                .saturating_duration_since(Instant::now())
                .as_micros()
                / 125,
        )
        .map_err(|_| Error::new(Kind::Limit))?;
        Ok(Speaker {
            transport: Some(transport),
            codec: self.channel.output_codec().clone(),
            control,
            deadline: self.deadline,
            next_send: Instant::now(),
            remaining_bytes,
        })
    }
}

impl Speaker {
    /// Returns the exact G.711 codec required by this speaker.
    pub const fn codec(&self) -> &AudioCodec {
        &self.codec
    }

    /// Sends 1..1600 encoded samples with real-time pacing at 8 kHz.
    ///
    /// No WAV, RTP, ADPCM or HTTP chunk framing is added. A failed write closes
    /// the connection and is never retried, because a prefix may have been sent.
    ///
    /// # Errors
    /// Rejects oversized frames, an exhausted byte budget, cancellation, expired
    /// deadlines and transport failures. A write can wait at most 100 ms.
    pub fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let result = self.send_inner(bytes);
        if result.is_err() {
            self.transport.take();
        }
        result
    }

    fn send_inner(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.control.set_deadline(Some(self.deadline));
        self.control.check()?;
        if bytes.is_empty()
            || bytes.len() > FRAME_BYTES_MAX
            || bytes.len() as u64 > self.remaining_bytes
        {
            return Err(Error::new(Kind::Limit));
        }
        while Instant::now() < self.next_send {
            self.control.check()?;
            std::thread::park_timeout(
                self.next_send
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(25)),
            );
        }
        self.control.check()?;
        let send_started = Instant::now();
        self.control
            .set_deadline(Some(self.deadline.min(send_started + WRITE_TIMEOUT)));
        let transport = self
            .transport
            .as_mut()
            .ok_or_else(|| Error::new(Kind::Cancelled))?;
        let output = transport.buffers().output();
        if bytes.len() > output.len() {
            return Err(Error::new(Kind::Limit));
        }
        output[..bytes.len()].copy_from_slice(bytes);
        transport.transmit_output(
            bytes.len(),
            NextTimeout {
                after: WRITE_TIMEOUT.into(),
                reason: ureq::Timeout::SendBody,
            },
        )?;
        self.control.check()?;
        self.control.set_deadline(Some(self.deadline));
        self.remaining_bytes -= bytes.len() as u64;
        self.next_send = advance_clock(
            self.next_send,
            send_started,
            Duration::from_micros(bytes.len() as u64 * 125),
        );
        Ok(())
    }
}

fn validate_upload(response: Response<ureq::Body>) -> Result<(), Error> {
    let http_status = response.status().as_u16();
    let headers = response.headers();
    if headers.get_all("content-type").iter().count() > 1
        || headers.get_all("content-length").iter().count() > 1
        || headers.contains_key("transfer-encoding")
    {
        return Err(Error::new(Kind::Protocol));
    }
    let declared_media = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok());
    if declared_media.is_none() && response.body().content_length() != Some(0) {
        return Err(Error::new(Kind::Protocol));
    }
    let media = declared_media
        .unwrap_or("application/octet-stream")
        .parse::<mime::Mime>()
        .map_err(|_| Error::new(Kind::Protocol))?;
    if !matches!(
        media.essence_str(),
        "application/octet-stream" | "application/xml" | "text/xml" | "application/json"
    ) {
        return Err(Error::new(Kind::Protocol));
    }
    let has_body = response.body().content_length() != Some(0);
    let closing = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"))
        });
    if has_body
        && matches!(
            media.essence_str(),
            "application/xml" | "text/xml" | "application/json"
        )
    {
        let mut body = Vec::new();
        response
            .into_body()
            .into_with_config()
            .reader()
            .take(XML_SIZE_BYTES_MAX as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() > XML_SIZE_BYTES_MAX {
            return Err(Error::new(Kind::Limit));
        }
        ResponseStatus::parse(media.as_ref(), &body)?.ensure_success()?;
    } else if has_body && response.body().content_length().is_some() {
        return Err(Error::new(Kind::Protocol));
    }
    if http_status != 200 {
        return Err(Error::new(Kind::HttpStatus(http_status)));
    }
    if closing {
        return Err(Error::new(Kind::Protocol));
    }
    Ok(())
}

fn advance_clock(scheduled: Instant, started: Instant, duration: Duration) -> Instant {
    if started.saturating_duration_since(scheduled) > Duration::from_millis(200) {
        started + duration
    } else {
        scheduled + duration
    }
}

impl fmt::Debug for Speaker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Speaker")
            .field("codec", &self.codec)
            .field("remaining_bytes", &self.remaining_bytes)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_clock_absorbs_write_latency_without_accumulating_delay() {
        let base = Instant::now();
        let frame = Duration::from_millis(20);
        let next = advance_clock(base, base + Duration::from_millis(5), frame);
        assert_eq!(next, base + frame);
        let next = advance_clock(next, next + Duration::from_millis(5), frame);
        assert_eq!(next, base + 2 * frame);
        let late = advance_clock(next, base + Duration::from_millis(500), frame);
        assert_eq!(late, base + Duration::from_millis(520));
    }
}
