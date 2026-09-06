use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ureq::http::Response;

use super::Talk;
use crate::blocking::{Control, REQUEST_TIMEOUT, make_agent};
use crate::error::Kind;
use crate::management::{AudioCodec, ResponseStatus};
use crate::{Error, XML_SIZE_BYTES_MAX};

const READ_BYTES_MAX: usize = 1600;
const IDLE_TIMEOUT: Duration = Duration::from_secs(2);

/// A bounded camera microphone stream with independent cancellation and deadlines.
pub struct Microphone {
    reader: Option<ureq::BodyReader<'static>>,
    codec: AudioCodec,
    control: Control,
    deadline: Instant,
    remaining_bytes: u64,
    ended: bool,
}

impl Talk {
    /// Opens this session's only microphone receive connection.
    ///
    /// A speaker handle and this handle can run on different threads. Returned
    /// bytes use the camera's reported microphone codec, without RTP or WAV headers.
    ///
    /// # Errors
    /// Rejects duplicate handles, unavailable microphones, unsupported codecs,
    /// expired sessions, mismatched media types and transport failures.
    pub fn microphone(&mut self) -> Result<Microphone, Error> {
        if self.is_stopped() {
            return Err(Error::new(Kind::Cancelled));
        }
        if self.microphone_opened
            || !self.channel.microphone_supported()
            || self.channel.input_codec().sample_rate_hz().is_none()
        {
            return Err(Error::new(Kind::InvalidInput));
        }
        self.microphone_opened = true;
        let setup = self.deadline.min(Instant::now() + REQUEST_TIMEOUT);
        let control = Control::new(Arc::clone(&self.cancelled), Some(setup));
        let agent = make_agent(control.clone());
        let response = self.client.open(
            &self.session.receive_request(self.channel.id())?,
            Some(self.deadline),
            &agent,
        )?;
        let codec = self.channel.input_codec().clone();
        let response = validate_receive(response, &codec)?;
        let remaining_bytes = u64::try_from(
            self.deadline
                .saturating_duration_since(Instant::now())
                .as_micros()
                / 125,
        )
        .map_err(|_| Error::new(Kind::Limit))?;
        if response
            .body()
            .content_length()
            .is_some_and(|length| length > remaining_bytes)
        {
            return Err(Error::new(Kind::Limit));
        }
        let reader = response
            .into_body()
            .into_with_config()
            .limit(remaining_bytes + 1)
            .reader();
        Ok(Microphone {
            reader: Some(reader),
            codec,
            control,
            deadline: self.deadline,
            remaining_bytes,
            ended: false,
        })
    }
}

impl Microphone {
    /// Returns the camera microphone's exact G.711 codec.
    pub const fn codec(&self) -> &AudioCodec {
        &self.codec
    }

    /// Reads up to 1600 encoded samples, or returns zero after a clean stream end.
    ///
    /// # Errors
    /// Returns cancellation, a two-second idle timeout, a session-lifetime timeout,
    /// exhausted byte budgets and malformed or truncated HTTP bodies. Any failure
    /// closes the reader; callers must not retry partial media on a new connection.
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize, Error> {
        let result = self.read_inner(output);
        if result.is_err() || matches!(result, Ok(0)) && !output.is_empty() {
            self.reader.take();
        }
        result
    }

    fn read_inner(&mut self, output: &mut [u8]) -> Result<usize, Error> {
        self.control
            .set_deadline(Some(self.deadline.min(Instant::now() + IDLE_TIMEOUT)));
        self.control.check()?;
        if output.is_empty() || self.ended {
            return Ok(0);
        }
        if self.remaining_bytes == 0 {
            return Err(Error::new(Kind::Limit));
        }
        let length = output
            .len()
            .min(READ_BYTES_MAX)
            .min(usize::try_from(self.remaining_bytes + 1).map_err(|_| Error::new(Kind::Limit))?);
        let count = self
            .reader
            .as_mut()
            .ok_or_else(|| Error::new(Kind::Cancelled))?
            .read(&mut output[..length])?;
        if count as u64 > self.remaining_bytes {
            return Err(Error::new(Kind::Limit));
        }
        self.remaining_bytes -= count as u64;
        self.ended = count == 0;
        Ok(count)
    }
}

fn validate_receive(
    response: Response<ureq::Body>,
    codec: &AudioCodec,
) -> Result<Response<ureq::Body>, Error> {
    let status = response.status().as_u16();
    let headers = response.headers();
    let mut transfer = headers.get_all("transfer-encoding").iter();
    if transfer.next().is_some_and(|value| {
        value.to_str().is_err()
            || !value
                .to_str()
                .is_ok_and(|value| value.trim().eq_ignore_ascii_case("chunked"))
    }) || transfer.next().is_some()
    {
        return Err(Error::new(Kind::Protocol));
    }
    if headers.get_all("content-type").iter().count() != 1
        || headers.get_all("content-length").iter().count() > 1
        || headers.contains_key("content-length") && headers.contains_key("transfer-encoding")
    {
        return Err(Error::new(Kind::Protocol));
    }
    let media = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| Error::new(Kind::Protocol))?
        .parse::<mime::Mime>()
        .map_err(|_| Error::new(Kind::Protocol))?;
    if matches!(
        media.essence_str(),
        "application/xml" | "text/xml" | "application/json"
    ) {
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
        return Err(Error::new(Kind::Protocol));
    }
    if status != 200 {
        return Err(Error::new(Kind::HttpStatus(status)));
    }
    let valid = match media.essence_str() {
        "application/octet-stream" => true,
        "audio/basic" | "audio/pcmu" => codec == &AudioCodec::G711Ulaw,
        "audio/pcma" => codec == &AudioCodec::G711Alaw,
        _ => false,
    };
    if !valid
        || media
            .get_param("rate")
            .is_some_and(|value| value.as_str().parse::<u32>() != Ok(8000))
        || media
            .get_param("channels")
            .is_some_and(|value| value != "1")
    {
        return Err(Error::new(Kind::Protocol));
    }
    Ok(response)
}

impl fmt::Debug for Microphone {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Microphone")
            .field("codec", &self.codec)
            .field("remaining_bytes", &self.remaining_bytes)
            .finish_non_exhaustive()
    }
}
