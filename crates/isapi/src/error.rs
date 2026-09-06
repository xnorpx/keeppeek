use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    InvalidInput,
    Authentication,
    Limit,
    Protocol,
    DeviceStatus(u32),
    #[cfg(feature = "ureq")]
    Cancelled,
    #[cfg(feature = "ureq")]
    HttpStatus(u16),
    #[cfg(feature = "ureq")]
    Io(std::io::ErrorKind),
}

/// A protocol failure that does not expose credentials or peer-controlled payloads.
#[derive(Debug)]
pub struct Error {
    kind: Kind,
    backtrace: Backtrace,
}

impl Error {
    /// Returns the device-level ResponseStatus code, independently of HTTP success.
    pub const fn device_status(&self) -> Option<u32> {
        match self.kind {
            Kind::DeviceStatus(code) => Some(code),
            _ => None,
        }
    }
    pub(crate) const fn new(kind: Kind) -> Self {
        Self {
            kind,
            backtrace: Backtrace::disabled(),
        }
    }

    /// Reports invalid request paths, endpoint configuration, credentials, or command parameters.
    pub const fn is_invalid_input(&self) -> bool {
        matches!(self.kind, Kind::InvalidInput)
    }

    /// Reports an invalid or unsupported Digest authentication exchange.
    pub const fn is_authentication(&self) -> bool {
        matches!(self.kind, Kind::Authentication)
    }

    /// Reports a configured size or work limit being exceeded.
    pub const fn is_limit(&self) -> bool {
        matches!(self.kind, Kind::Limit)
    }

    /// Reports malformed protocol data or a truncated message.
    pub const fn is_protocol(&self) -> bool {
        matches!(self.kind, Kind::Protocol)
    }

    /// Returns a rejected HTTP status, when the failure came from a camera response.
    #[cfg(feature = "ureq")]
    pub const fn http_status(&self) -> Option<u16> {
        match self.kind {
            Kind::HttpStatus(status) => Some(status),
            _ => None,
        }
    }

    /// Reports an expired network deadline.
    #[cfg(feature = "ureq")]
    pub const fn is_timeout(&self) -> bool {
        matches!(self.kind, Kind::Io(std::io::ErrorKind::TimedOut))
    }

    /// Reports that the transport's caller requested cancellation.
    #[cfg(feature = "ureq")]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.kind, Kind::Cancelled)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind {
            Kind::InvalidInput => "Invalid ISAPI request or configuration",
            Kind::Authentication => "ISAPI Digest authentication failed",
            Kind::Limit => "ISAPI resource limit exceeded",
            Kind::Protocol => "Malformed or truncated ISAPI protocol data",
            Kind::DeviceStatus(_) => "Camera reported an ISAPI operation failure",
            #[cfg(feature = "ureq")]
            Kind::HttpStatus(_) => "Camera rejected the ISAPI request",
            #[cfg(feature = "ureq")]
            Kind::Io(std::io::ErrorKind::TimedOut) => "ISAPI network deadline expired",
            #[cfg(feature = "ureq")]
            Kind::Cancelled => "ISAPI operation cancelled",
            #[cfg(feature = "ureq")]
            Kind::Io(_) => "ISAPI network operation failed",
        })?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(formatter, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<digest_auth::Error> for Error {
    fn from(_: digest_auth::Error) -> Self {
        Self::new(Kind::Authentication)
    }
}

impl From<xml::reader::Error> for Error {
    fn from(_: xml::reader::Error) -> Self {
        Self::new(Kind::Protocol)
    }
}

impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::new(Kind::Protocol)
    }
}

#[cfg(feature = "ureq")]
impl From<ureq::Error> for Error {
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::Timeout(_) => Self::new(Kind::Io(std::io::ErrorKind::TimedOut)),
            ureq::Error::Io(error) => Self::from(error),
            ureq::Error::Other(error) => error.downcast::<Self>().map_or_else(
                |_| Self::new(Kind::Io(std::io::ErrorKind::Other)),
                |error| *error,
            ),
            _ => Self::new(Kind::Io(std::io::ErrorKind::Other)),
        }
    }
}

#[cfg(feature = "ureq")]
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        let kind = error.kind();
        if let Some(cause) = error
            .into_inner()
            .and_then(|cause| cause.downcast::<ureq::Error>().ok())
        {
            return Self::from(*cause);
        }
        Self::new(Kind::Io(kind))
    }
}
