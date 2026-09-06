use super::{number, required};
use crate::error::Kind;
use crate::{Document, Error};

/// Device-level operation status, independent of the HTTP status code.
#[derive(Clone, Eq, PartialEq)]
pub struct ResponseStatus {
    code: u32,
    subcode: String,
    id: Option<String>,
    error_code: Option<u32>,
}

impl ResponseStatus {
    /// Parses XML or JSON ResponseStatus, including wrapped JSON responses.
    ///
    /// # Errors
    /// Rejects missing, duplicate or invalid status fields and unsupported encodings.
    pub fn parse(content_type: &str, bytes: &[u8]) -> Result<Self, Error> {
        Self::from_document(&Document::parse(content_type, bytes)?)?
            .ok_or_else(|| Error::new(Kind::Protocol))
    }

    pub(super) fn from_document(document: &Document) -> Result<Option<Self>, Error> {
        let is_status = document
            .xml_root()
            .is_some_and(|root| root.name().rsplit(':').next() == Some("ResponseStatus"))
            || document.json_value().is_some_and(|value| {
                value.get("ResponseStatus").is_some() || value.get("statusCode").is_some()
            });
        if !is_status {
            return Ok(None);
        }
        let root = document.root("ResponseStatus")?;
        Ok(Some(Self {
            code: number(root, "statusCode")?.ok_or_else(|| Error::new(Kind::Protocol))?,
            subcode: required(root, "subStatusCode")?,
            id: root.field("id")?,
            error_code: number(root, "errorCode")?,
        }))
    }
    /// Returns the original device status code.
    pub const fn code(&self) -> u32 {
        self.code
    }
    /// Returns the camera's machine-readable substatus; do not log it as trusted text.
    pub fn subcode(&self) -> &str {
        &self.subcode
    }
    /// Returns the created resource ID when the camera provides one.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    /// Returns an optional numeric vendor error code.
    pub const fn error_code(&self) -> Option<u32> {
        self.error_code
    }
    /// Treats zero, one and seven as accepted; seven also requires a reboot.
    pub const fn success(&self) -> bool {
        matches!(self.code, 0 | 1 | 7)
    }
    /// Reports a successful configuration that requires a device reboot.
    pub const fn reboot_required(&self) -> bool {
        self.code == 7
    }
    /// Converts a device rejection into a payload-safe error.
    ///
    /// # Errors
    /// Returns the original device code when the operation was rejected.
    pub const fn ensure_success(&self) -> Result<(), Error> {
        if self.success() {
            Ok(())
        } else {
            Err(Error::new(Kind::DeviceStatus(self.code)))
        }
    }
}

impl std::fmt::Debug for ResponseStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResponseStatus")
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}
