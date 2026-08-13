use std::fmt;

/// Errors produced by the Baichuan protocol implementation.
#[derive(Debug)]
pub enum BcError {
    /// Not enough data yet (caller should feed more bytes).
    Incomplete,
    /// Bad magic bytes in header.
    BadMagic([u8; 4]),
    /// Header field out of range.
    InvalidHeader(&'static str),
    /// Encryption error (wrong key, corrupt data).
    Encryption(&'static str),
    /// XML parse error.
    XmlParse(&'static str),
    /// Protocol error (unexpected message, wrong state).
    Protocol(&'static str),
    /// Authentication failed with the given status code.
    AuthFailed(u32),
    /// Command issued for the wrong role (client vs camera).
    WrongRole,
    /// Caller-provided output buffer is too small.
    BufferTooSmall { needed: usize, available: usize },
    /// Internal buffer overflow (message exceeds configured max).
    MessageTooLarge { size: usize, max: usize },
    /// Malformed Baichuan UDP datagram.
    InvalidUdpPacket(&'static str),
    /// Baichuan UDP discovery payload failed its checksum.
    UdpChecksumMismatch { expected: u32, actual: u32 },
}

impl fmt::Display for BcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete => write!(f, "incomplete data"),
            Self::BadMagic(m) => {
                write!(
                    f,
                    "bad magic: {:02x} {:02x} {:02x} {:02x}",
                    m[0], m[1], m[2], m[3]
                )
            }
            Self::InvalidHeader(msg) => write!(f, "invalid header: {msg}"),
            Self::Encryption(msg) => write!(f, "encryption error: {msg}"),
            Self::XmlParse(msg) => write!(f, "XML parse error: {msg}"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Self::AuthFailed(code) => write!(f, "authentication failed: status {code}"),
            Self::WrongRole => write!(f, "command not valid for this session role"),
            Self::BufferTooSmall { needed, available } => {
                write!(f, "buffer too small: need {needed} bytes, have {available}")
            }
            Self::MessageTooLarge { size, max } => {
                write!(f, "message too large: {size} bytes exceeds max {max}")
            }
            Self::InvalidUdpPacket(message) => write!(f, "invalid UDP packet: {message}"),
            Self::UdpChecksumMismatch { expected, actual } => write!(
                f,
                "UDP checksum mismatch: expected {expected:#010x}, calculated {actual:#010x}"
            ),
        }
    }
}

impl std::error::Error for BcError {}
