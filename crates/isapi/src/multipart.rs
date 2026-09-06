use std::fmt;

use crate::error::Kind;
use crate::{Error, Event, XML_SIZE_BYTES_MAX};

const INPUT_SIZE_BYTES_MAX: usize = 64 * 1024;
const HEADER_SIZE_BYTES_MAX: usize = 8 * 1024;
const PART_SIZE_BYTES_MAX: usize = 1024 * 1024;
const BUFFER_SIZE_BYTES_MAX: usize =
    PART_SIZE_BYTES_MAX + HEADER_SIZE_BYTES_MAX + INPUT_SIZE_BYTES_MAX;
const BOUNDARY_SIZE_BYTES_MAX: usize = 70;

/// The declared media kind of an alert-stream part, independent of event semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PartKind {
    /// XML alarm data, not yet parsed as an event.
    Xml,
    /// JSON alarm data whose explicit event fields can be decoded independently.
    Json,
    /// JPEG attachment bytes, not decoded or implicitly associated with an event.
    Jpeg,
    /// An unrecognized media type whose bytes are preserved.
    Other,
}

struct Headers {
    kind: PartKind,
    media_type: String,
    charset: Option<String>,
    content_id: Option<String>,
    name: Option<String>,
    filename: Option<String>,
    length: Option<usize>,
}

impl Headers {
    const fn body_limit(&self) -> usize {
        match self.kind {
            PartKind::Xml | PartKind::Json => XML_SIZE_BYTES_MAX,
            PartKind::Jpeg | PartKind::Other => PART_SIZE_BYTES_MAX,
        }
    }
}

/// One complete MIME part with original payload bytes and selected headers.
pub struct Part {
    headers: Headers,
    body: Vec<u8>,
}

impl Part {
    /// Wraps one bounded HTTP body with its declared media type.
    ///
    /// # Errors
    /// Rejects invalid media types, header injection and oversized bodies.
    pub fn from_body(content_type: &str, body: Vec<u8>) -> Result<Self, Error> {
        if content_type.len() > HEADER_SIZE_BYTES_MAX || content_type.contains(['\r', '\n']) {
            return Err(Error::new(Kind::Protocol));
        }
        let header = format!(
            "Content-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        Ok(Self {
            headers: parse_headers(header.as_bytes())?,
            body,
        })
    }

    /// Returns the opaque form-data field name, not a filesystem path.
    pub fn name(&self) -> Option<&str> {
        self.headers.name.as_deref()
    }

    /// Returns the opaque form-data filename, not a filesystem path.
    pub fn filename(&self) -> Option<&str> {
        self.headers.filename.as_deref()
    }

    /// Transfers ownership of the exact payload bytes without decoding them.
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }

    /// Returns the media category declared by the part's Content-Type.
    pub const fn kind(&self) -> PartKind {
        self.headers.kind
    }

    /// Returns the normalized media type without parameters.
    pub fn media_type(&self) -> &str {
        &self.headers.media_type
    }

    /// Returns the normalized charset label when the part declares one.
    pub fn charset(&self) -> Option<&str> {
        self.headers.charset.as_deref()
    }

    /// Returns the opaque Content-ID, which must not be interpreted as a filesystem path.
    pub fn content_id(&self) -> Option<&str> {
        self.headers.content_id.as_deref()
    }

    /// Returns the exact part body without MIME delimiters.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Parses XML or JSON notifications while leaving images and other parts opaque.
    ///
    /// # Errors
    /// Rejects unsupported or conflicting charsets and any error from [`Event::parse`].
    pub fn event(&self) -> Result<Option<Event>, Error> {
        if !matches!(self.kind(), PartKind::Xml | PartKind::Json) {
            return Ok(None);
        }
        match self.kind() {
            PartKind::Json => Event::parse_json_charset(&self.body, self.charset()).map(Some),
            _ => Event::parse_xml_charset(&self.body, self.charset()).map(Some),
        }
    }
}

impl fmt::Debug for Part {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Part")
            .field("kind", &self.kind())
            .field("body_size_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

enum Phase {
    Boundary,
    Headers,
    Body(Headers),
    Done,
    Failed,
}

/// Incrementally decodes a bounded multipart stream without any I/O or internal queue.
///
/// Feed at most 64 KiB per [`Self::push`] and drain [`Self::next_part`] before
/// feeding again. At most 1 MiB plus 72 KiB of input is buffered. XML/JSON bodies
/// are capped at 256 KiB, other parts at 1 MiB, and headers at 8 KiB/32 fields.
pub struct Decoder {
    boundary: Vec<u8>,
    delimiter: Vec<u8>,
    buffer: Vec<u8>,
    phase: Phase,
    scan: usize,
    padding_size_bytes: usize,
}

impl Decoder {
    /// Creates a parser using the actual response Content-Type and boundary parameter.
    ///
    /// # Errors
    /// Requires multipart/mixed, multipart/x-mixed-replace or multipart/form-data and one boundary.
    pub fn new(content_type: impl AsRef<str>) -> Result<Self, Error> {
        let content_type = content_type.as_ref();
        if content_type.len() > HEADER_SIZE_BYTES_MAX {
            return Err(Error::new(Kind::Limit));
        }
        let mime: mime::Mime = content_type
            .parse()
            .map_err(|_| Error::new(Kind::Protocol))?;
        if mime.type_() != mime::MULTIPART
            || !matches!(
                mime.subtype().as_str(),
                "mixed" | "x-mixed-replace" | "form-data"
            )
        {
            return Err(Error::new(Kind::Protocol));
        }
        let boundaries: Vec<_> = mime
            .params()
            .filter(|(name, _)| name == &mime::BOUNDARY)
            .collect();
        if boundaries.len() != 1 || !valid_boundary(boundaries[0].1.as_str()) {
            return Err(Error::new(Kind::Protocol));
        }
        let boundary = format!("--{}", boundaries[0].1).into_bytes();
        let delimiter = [b"\r\n".as_slice(), boundary.as_slice()].concat();
        Ok(Self {
            boundary,
            delimiter,
            buffer: Vec::with_capacity(8192),
            phase: Phase::Boundary,
            scan: 0,
            padding_size_bytes: 0,
        })
    }

    /// Adds a bounded chunk without interpreting or emitting any event.
    ///
    /// # Errors
    /// Rejects chunks over 64 KiB, buffer overflow, and input after a parser failure.
    pub fn push(&mut self, bytes: impl AsRef<[u8]>) -> Result<(), Error> {
        if matches!(self.phase, Phase::Failed) {
            return Err(Error::new(Kind::Protocol));
        }
        let bytes = bytes.as_ref();
        if bytes.len() > INPUT_SIZE_BYTES_MAX
            || bytes.len() > BUFFER_SIZE_BYTES_MAX.saturating_sub(self.buffer.len())
        {
            self.phase = Phase::Failed;
            return Err(Error::new(Kind::Limit));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Returns one complete part, or `None` when more input is needed or the stream ended.
    ///
    /// # Errors
    /// Malformed framing, ambiguous headers, and resource exhaustion terminate this decoder.
    pub fn next_part(&mut self) -> Result<Option<Part>, Error> {
        let result = self.parse_next();
        if result.is_err() {
            self.phase = Phase::Failed;
        }
        result
    }

    /// Checks that a drained stream did not end inside a MIME part.
    ///
    /// A clean framing boundary does not mean that camera motion has stopped.
    ///
    /// # Errors
    /// Rejects undrained data, partial headers/bodies, and a prior parser failure.
    pub fn finish(&self) -> Result<(), Error> {
        if (matches!(self.phase, Phase::Boundary) && self.buffer.is_empty())
            || (matches!(self.phase, Phase::Done)
                && (self.buffer.is_empty() || self.buffer == b"\r\n"))
        {
            return Ok(());
        }
        Err(Error::new(Kind::Protocol))
    }

    /// Reports that the multipart closing delimiter was consumed.
    pub const fn is_finished(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    fn parse_next(&mut self) -> Result<Option<Part>, Error> {
        for _ in 0..3 {
            match self.phase {
                Phase::Boundary => {
                    if !self.read_boundary()? {
                        return Ok(None);
                    }
                }
                Phase::Headers => {
                    if !self.read_headers()? {
                        return Ok(None);
                    }
                }
                Phase::Body(_) => return self.read_body(),
                Phase::Done if self.buffer.len() <= 2 && b"\r\n".starts_with(&self.buffer) => {
                    return Ok(None);
                }
                Phase::Done | Phase::Failed => return Err(Error::new(Kind::Protocol)),
            }
        }
        Ok(None)
    }

    fn read_boundary(&mut self) -> Result<bool, Error> {
        let padding = self
            .buffer
            .chunks_exact(2)
            .take_while(|pair| *pair == b"\r\n")
            .count()
            * 2;
        self.padding_size_bytes += padding;
        if self.padding_size_bytes > HEADER_SIZE_BYTES_MAX {
            return Err(Error::new(Kind::Limit));
        }
        self.consume(padding);
        if self.buffer.is_empty() || self.buffer == b"\r" {
            return Ok(false);
        }
        let prefix_len = self.boundary.len().min(self.buffer.len());
        if self.buffer[..prefix_len] != self.boundary[..prefix_len] {
            return Err(Error::new(Kind::Protocol));
        }
        if self.buffer.len() < self.boundary.len() + 2 {
            return Ok(false);
        }
        self.phase = match &self.buffer[self.boundary.len()..self.boundary.len() + 2] {
            b"\r\n" => Phase::Headers,
            b"--" => Phase::Done,
            _ => return Err(Error::new(Kind::Protocol)),
        };
        self.consume(self.boundary.len() + 2);
        self.padding_size_bytes = 0;
        Ok(true)
    }

    fn read_headers(&mut self) -> Result<bool, Error> {
        let search_end = self.buffer.len().min(HEADER_SIZE_BYTES_MAX + 1);
        let marker = self.buffer[self.scan.min(search_end)..search_end]
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n");
        if let Some(marker) = marker {
            let header_end = self.scan + marker + 4;
            if header_end > HEADER_SIZE_BYTES_MAX {
                return Err(Error::new(Kind::Limit));
            }
            let headers = parse_headers(&self.buffer[..header_end])?;
            self.consume(header_end);
            self.phase = Phase::Body(headers);
            return Ok(true);
        }
        if self.buffer.len() > HEADER_SIZE_BYTES_MAX {
            return Err(Error::new(Kind::Limit));
        }
        self.scan = self.buffer.len().saturating_sub(3);
        Ok(false)
    }

    fn read_body(&mut self) -> Result<Option<Part>, Error> {
        let Phase::Body(headers) = &self.phase else {
            unreachable!("body parsing requires a parsed header block");
        };
        let length = if let Some(length) = headers.length {
            if self.buffer.len() < length {
                return Ok(None);
            }
            length
        } else {
            let limit = headers.body_limit();
            let Some(length) = self.find_delimiter() else {
                if self.buffer.len() > limit + self.delimiter.len() + 2 {
                    return Err(Error::new(Kind::Limit));
                }
                return Ok(None);
            };
            if length > limit {
                return Err(Error::new(Kind::Limit));
            }
            length
        };
        let Phase::Body(headers) = std::mem::replace(&mut self.phase, Phase::Boundary) else {
            unreachable!("the body phase remains active until emission");
        };
        let body = self.buffer[..length].to_vec();
        self.consume(length);
        Ok(Some(Part { headers, body }))
    }

    fn find_delimiter(&mut self) -> Option<usize> {
        while self.scan + self.delimiter.len() <= self.buffer.len() {
            let Some(offset) = self.buffer[self.scan..]
                .windows(self.delimiter.len())
                .position(|bytes| bytes == self.delimiter)
            else {
                self.scan = self.buffer.len().saturating_sub(self.delimiter.len() - 1);
                return None;
            };
            self.scan += offset;
            let suffix = self.scan + self.delimiter.len();
            if suffix + 2 > self.buffer.len() {
                return None;
            }
            if matches!(&self.buffer[suffix..suffix + 2], b"\r\n" | b"--") {
                return Some(self.scan);
            }
            self.scan += 1;
        }
        self.scan = self.buffer.len().saturating_sub(self.delimiter.len() - 1);
        None
    }

    fn consume(&mut self, length: usize) {
        self.buffer.drain(..length);
        self.scan = 0;
    }
}

impl fmt::Debug for Decoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Decoder")
            .field("buffer_size_bytes", &self.buffer.len())
            .finish_non_exhaustive()
    }
}

fn valid_boundary(boundary: &str) -> bool {
    !boundary.is_empty()
        && boundary.len() <= BOUNDARY_SIZE_BYTES_MAX
        && !boundary.ends_with(' ')
        && boundary
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&byte))
}

fn parse_headers(bytes: &[u8]) -> Result<Headers, Error> {
    let mut slots = [httparse::EMPTY_HEADER; 32];
    let httparse::Status::Complete((_, headers)) =
        httparse::parse_headers(bytes, &mut slots).map_err(|_| Error::new(Kind::Protocol))?
    else {
        return Err(Error::new(Kind::Protocol));
    };
    let media_type =
        one_header(headers, "content-type")?.ok_or_else(|| Error::new(Kind::Protocol))?;
    let mime: mime::Mime = media_type.parse().map_err(|_| Error::new(Kind::Protocol))?;
    let charset: Vec<_> = mime
        .params()
        .filter(|(name, _)| name == &mime::CHARSET)
        .collect();
    if charset.len() > 1 {
        return Err(Error::new(Kind::Protocol));
    }
    let length = one_header(headers, "content-length")?
        .map(|value| {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Error::new(Kind::Protocol));
            }
            value.parse::<usize>().map_err(|_| Error::new(Kind::Limit))
        })
        .transpose()?;
    let (name, filename) = disposition(one_header(headers, "content-disposition")?)?;
    let headers = Headers {
        kind: match mime.essence_str() {
            "application/xml" | "text/xml" => PartKind::Xml,
            "application/json" => PartKind::Json,
            "image/jpeg" => PartKind::Jpeg,
            _ => PartKind::Other,
        },
        media_type: mime.essence_str().to_owned(),
        charset: charset
            .first()
            .map(|(_, value)| value.as_str().to_ascii_lowercase()),
        content_id: one_header(headers, "content-id")?.map(str::to_owned),
        name,
        filename,
        length,
    };
    if headers
        .length
        .is_some_and(|length| length > headers.body_limit())
    {
        return Err(Error::new(Kind::Limit));
    }
    Ok(headers)
}

fn disposition(value: Option<&str>) -> Result<(Option<String>, Option<String>), Error> {
    let Some(value) = value else {
        return Ok((None, None));
    };
    let parsed: mime::Mime = format!("application/{value}")
        .parse()
        .map_err(|_| Error::new(Kind::Protocol))?;
    if !matches!(
        parsed.subtype().as_str(),
        "form-data" | "attachment" | "inline"
    ) {
        return Err(Error::new(Kind::Protocol));
    }
    let mut name = None;
    let mut filename = None;
    for (key, value) in parsed.params() {
        let slot = match key.as_str() {
            "name" => &mut name,
            "filename" => &mut filename,
            _ => continue,
        };
        if slot.is_some()
            || value.as_str().len() > 256
            || value.as_str().chars().any(char::is_control)
        {
            return Err(Error::new(Kind::Protocol));
        }
        *slot = Some(value.as_str().to_owned());
    }
    Ok((name, filename))
}

fn one_header<'header>(
    headers: &'header [httparse::Header<'header>],
    name: &str,
) -> Result<Option<&'header str>, Error> {
    let mut found = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name));
    let value = found.next();
    if found.next().is_some() {
        return Err(Error::new(Kind::Protocol));
    }
    value
        .map(|header| std::str::from_utf8(header.value).map_err(|_| Error::new(Kind::Protocol)))
        .transpose()
}
