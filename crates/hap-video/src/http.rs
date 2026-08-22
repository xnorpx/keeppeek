use std::{error::Error as StdError, fmt};

const MAX_HEADERS: usize = 64;
const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Supported HAP HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
}

/// HAP endpoint selected from an HTTP request target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    PairSetup,
    PairVerify,
    Pairings,
    Accessories,
    Characteristics,
    Resource,
    Identify,
    Unknown,
}

/// One complete, owned HAP HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub target: String,
    pub endpoint: Endpoint,
    pub body: Vec<u8>,
}

impl Request {
    /// Parses one request from the start of a caller-managed plaintext buffer.
    pub fn parse(input: &[u8]) -> Result<ParseResult, HttpError> {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut request = httparse::Request::new(&mut headers);
        let header_length = match request.parse(input) {
            Ok(httparse::Status::Complete(length)) => length,
            Ok(httparse::Status::Partial) => {
                if input.len() > MAX_HEADER_BYTES {
                    return Err(HttpError::HeadersTooLarge(input.len()));
                }
                return Ok(ParseResult::NeedMore { total_length: None });
            }
            Err(httparse::Error::TooManyHeaders) => {
                return Err(HttpError::TooManyHeaders);
            }
            Err(_) => return Err(HttpError::Malformed),
        };
        if header_length > MAX_HEADER_BYTES {
            return Err(HttpError::HeadersTooLarge(header_length));
        }
        if request.version != Some(1) {
            return Err(HttpError::UnsupportedVersion);
        }
        let method = match request.method.ok_or(HttpError::Malformed)? {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            method => return Err(HttpError::UnsupportedMethod(method.to_owned())),
        };
        let target = request.path.ok_or(HttpError::Malformed)?.to_owned();
        let mut content_length = None;
        for header in request.headers.iter() {
            if header.name.eq_ignore_ascii_case("transfer-encoding") {
                return Err(HttpError::TransferEncodingUnsupported);
            }
            if header.name.eq_ignore_ascii_case("content-length") {
                if content_length.is_some() {
                    return Err(HttpError::DuplicateContentLength);
                }
                let value = std::str::from_utf8(header.value)
                    .map_err(|_| HttpError::InvalidContentLength)?;
                content_length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| HttpError::InvalidContentLength)?,
                );
            }
        }
        let body_length = content_length.unwrap_or(0);
        if body_length > MAX_BODY_BYTES {
            return Err(HttpError::BodyTooLarge(body_length));
        }
        let total_length = header_length
            .checked_add(body_length)
            .ok_or(HttpError::BodyTooLarge(body_length))?;
        if input.len() < total_length {
            return Ok(ParseResult::NeedMore {
                total_length: Some(total_length),
            });
        }
        let endpoint = Endpoint::from_target(&target);
        Ok(ParseResult::Complete {
            request: Self {
                method,
                target,
                endpoint,
                body: input[header_length..total_length].to_vec(),
            },
            consumed: total_length,
        })
    }
}

impl Endpoint {
    fn from_target(target: &str) -> Self {
        match target.split('?').next().unwrap_or(target) {
            "/pair-setup" => Self::PairSetup,
            "/pair-verify" => Self::PairVerify,
            "/pairings" => Self::Pairings,
            "/accessories" => Self::Accessories,
            "/characteristics" => Self::Characteristics,
            "/resource" => Self::Resource,
            "/identify" => Self::Identify,
            _ => Self::Unknown,
        }
    }
}

/// Result of parsing the front of a plaintext HAP stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseResult {
    /// More bytes are needed. A known total includes a parsed Content-Length.
    NeedMore { total_length: Option<usize> },
    /// One complete request was parsed; trailing bytes remain caller-owned.
    Complete { request: Request, consumed: usize },
}

/// HAP response content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    PairingTlv8,
    HapJson,
    Jpeg,
}

impl ContentType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PairingTlv8 => "application/pairing+tlv8",
            Self::HapJson => "application/hap+json",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// HTTP status used by HAP responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    NoContent,
    MultiStatus,
    BadRequest,
    NotFound,
    UnprocessableEntity,
    ConnectionAuthorizationRequired,
}

impl Status {
    const fn code(self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::NoContent => 204,
            Self::MultiStatus => 207,
            Self::BadRequest => 400,
            Self::NotFound => 404,
            Self::UnprocessableEntity => 422,
            Self::ConnectionAuthorizationRequired => 470,
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::NoContent => "No Content",
            Self::MultiStatus => "Multi-Status",
            Self::BadRequest => "Bad Request",
            Self::NotFound => "Not Found",
            Self::UnprocessableEntity => "Unprocessable Entity",
            Self::ConnectionAuthorizationRequired => "Connection Authorization Required",
        }
    }
}

/// Complete plaintext HTTP response ready for HAP record encryption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: Status,
    pub content_type: Option<ContentType>,
    pub body: Vec<u8>,
}

impl Response {
    /// Creates a response carrying TLV8 or HAP JSON.
    pub const fn new(status: Status, content_type: ContentType, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some(content_type),
            body,
        }
    }

    /// Creates a response with no body or content type.
    pub const fn empty(status: Status) -> Self {
        Self {
            status,
            content_type: None,
            body: Vec::new(),
        }
    }

    /// Encodes an HTTP/1.1 response with explicit Content-Length framing.
    pub fn encode(&self) -> Vec<u8> {
        self.encode_with_connection_close(false)
    }

    /// Encodes an HTTP/1.1 response and optionally announces connection closure.
    pub fn encode_with_connection_close(&self, close: bool) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n",
            self.status.code(),
            self.status.reason(),
            self.body.len()
        )
        .into_bytes();
        if let Some(content_type) = self.content_type {
            response.extend_from_slice(b"Content-Type: ");
            response.extend_from_slice(content_type.as_str().as_bytes());
            response.extend_from_slice(b"\r\n");
        }
        if close {
            response.extend_from_slice(b"Connection: close\r\n");
        }
        response.extend_from_slice(b"\r\n");
        response.extend_from_slice(&self.body);
        response
    }
}

/// Encodes one HAP characteristic notification using EVENT/1.0 framing.
pub fn encode_event(body: &[u8]) -> Vec<u8> {
    let mut event = format!(
        "EVENT/1.0 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\n\r\n",
        body.len(),
        ContentType::HapJson.as_str(),
    )
    .into_bytes();
    event.extend_from_slice(body);
    event
}

/// HTTP parser or framing failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    Malformed,
    UnsupportedVersion,
    UnsupportedMethod(String),
    TooManyHeaders,
    HeadersTooLarge(usize),
    InvalidContentLength,
    DuplicateContentLength,
    BodyTooLarge(usize),
    TransferEncodingUnsupported,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("malformed HAP HTTP request"),
            Self::UnsupportedVersion => f.write_str("HAP requires HTTP/1.1"),
            Self::UnsupportedMethod(method) => write!(f, "unsupported HTTP method {method}"),
            Self::TooManyHeaders => write!(f, "more than {MAX_HEADERS} HTTP headers"),
            Self::HeadersTooLarge(size) => write!(f, "HTTP headers have {size} bytes"),
            Self::InvalidContentLength => f.write_str("invalid Content-Length"),
            Self::DuplicateContentLength => f.write_str("duplicate Content-Length"),
            Self::BodyTooLarge(size) => write!(f, "HTTP body has {size} bytes"),
            Self::TransferEncodingUnsupported => {
                f.write_str("Transfer-Encoding is not supported by HAP framing")
            }
        }
    }
}

impl StdError for HttpError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_request_and_preserves_pipeline_boundary() {
        let bytes = b"POST /pair-verify HTTP/1.1\r\nContent-Length: 3\r\n\r\nabcNEXT";
        let ParseResult::Complete { request, consumed } = Request::parse(bytes).unwrap() else {
            panic!("expected complete request");
        };

        assert_eq!(request.method, Method::Post);
        assert_eq!(request.endpoint, Endpoint::PairVerify);
        assert_eq!(request.body, b"abc");
        assert_eq!(&bytes[consumed..], b"NEXT");
    }

    #[test]
    fn recognizes_camera_resource_requests() {
        let bytes = b"POST /resource HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
        let ParseResult::Complete { request, .. } = Request::parse(bytes).unwrap() else {
            panic!("expected complete request");
        };

        assert_eq!(request.endpoint, Endpoint::Resource);
    }

    #[test]
    fn reports_known_partial_body_size() {
        let bytes = b"PUT /characteristics HTTP/1.1\r\nContent-Length: 5\r\n\r\nab";
        let ParseResult::NeedMore { total_length } = Request::parse(bytes).unwrap() else {
            panic!("expected partial request");
        };

        assert_eq!(total_length, Some(bytes.len() + 3));
    }

    #[test]
    fn rejects_ambiguous_or_chunked_lengths() {
        assert_eq!(
            Request::parse(
                b"POST /pair-setup HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\n"
            ),
            Err(HttpError::DuplicateContentLength)
        );
        assert_eq!(
            Request::parse(b"POST /pair-setup HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(HttpError::TransferEncodingUnsupported)
        );
    }

    #[test]
    fn encodes_pairing_response() {
        let encoded = Response::new(Status::Ok, ContentType::PairingTlv8, vec![6, 1, 2]).encode();

        assert_eq!(
            encoded,
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Type: application/pairing+tlv8\r\n\r\n\x06\x01\x02"
        );
    }

    #[test]
    fn announces_connection_close_when_requested() {
        let encoded = Response::empty(Status::NoContent).encode_with_connection_close(true);

        assert!(
            encoded
                .windows(b"Connection: close\r\n".len())
                .any(|value| value == b"Connection: close\r\n")
        );
    }

    #[test]
    fn encodes_hap_event_notification() {
        let body = br#"{"characteristics":[{"aid":1,"iid":13,"value":2}]}"#;
        let encoded = encode_event(body);

        assert!(encoded.starts_with(b"EVENT/1.0 200 OK\r\n"));
        assert!(
            encoded
                .windows(b"Content-Type: application/hap+json\r\n".len())
                .any(|value| value == b"Content-Type: application/hap+json\r\n")
        );
        assert!(encoded.ends_with(body));
    }
}
