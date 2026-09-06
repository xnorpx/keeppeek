use super::nonzero;
use crate::error::Kind;
use crate::{Document, Error, Format, Method, Request};
use std::fmt;

/// A callback destination using the camera's documented MD5 Digest authentication.
#[derive(Clone)]
pub struct CallbackHost {
    id: u32,
    document: Document,
}

impl CallbackHost {
    /// Queries every configured callback host without changing the camera.
    ///
    /// # Errors
    /// Rejects malformed hosts, duplicate IDs and lists over 64 entries.
    pub fn list() -> Result<super::Query<Vec<Self>>, Error> {
        super::Query::new("/ISAPI/Event/notification/httpHosts", |document| {
            let hosts = document
                .root("HttpHostNotificationList")?
                .children("HttpHostNotification");
            if hosts.len() > 64 {
                return Err(Error::new(Kind::Limit));
            }
            let mut ids = std::collections::BTreeSet::new();
            hosts
                .into_iter()
                .map(|host| {
                    let id = super::number(host, "id")?
                        .filter(|id| *id > 0)
                        .ok_or_else(|| Error::new(Kind::Protocol))?;
                    if !ids.insert(id) {
                        return Err(Error::new(Kind::Protocol));
                    }
                    Ok(Self {
                        id,
                        document: host.document(),
                    })
                })
                .collect()
        })
    }

    /// Returns the configured destination URL as untrusted device data.
    ///
    /// # Errors
    /// Rejects missing or malformed fields.
    pub fn destination(&self) -> Result<String, Error> {
        super::required(self.document.root("HttpHostNotification")?, "url")
    }
    /// Configures a destination without contacting or changing a camera.
    ///
    /// # Errors
    /// Rejects zero IDs, non-HTTP URLs, userinfo, fragments and unbounded credentials.
    pub fn new(id: u32, destination: &str, username: &str, password: &str) -> Result<Self, Error> {
        nonzero(id)?;
        let url = url::Url::parse(destination).map_err(|_| Error::new(Kind::InvalidInput))?;
        if destination.len() > 2048
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query().is_some()
            || username.is_empty()
            || username.len() > 128
            || password.is_empty()
            || password.len() > 256
            || username
                .chars()
                .chain(password.chars())
                .any(char::is_control)
        {
            return Err(Error::new(Kind::InvalidInput));
        }
        let mut document = Document::empty_xml("HttpHostNotification")?;
        let (address_kind, address_field, address) =
            match url.host().ok_or_else(|| Error::new(Kind::InvalidInput))? {
                url::Host::Ipv4(address) => ("ipaddress", "ipAddress", address.to_string()),
                url::Host::Ipv6(address) => ("ipaddress", "ipv6Address", address.to_string()),
                url::Host::Domain(address) => ("hostname", "hostName", address.to_owned()),
            };
        for (name, value) in [
            ("id", id.to_string()),
            ("url", destination.to_owned()),
            ("protocolType", url.scheme().to_ascii_uppercase()),
            ("parameterFormatType", "XML".to_owned()),
            ("addressingFormatType", address_kind.to_owned()),
            (address_field, address),
            (
                "portNo",
                url.port_or_known_default()
                    .ok_or_else(|| Error::new(Kind::InvalidInput))?
                    .to_string(),
            ),
            ("userName", username.to_owned()),
            ("password", password.to_owned()),
            ("httpAuthenticationMethod", "MD5digest".to_owned()),
            ("uploadImagesDataType", "binary".to_owned()),
            ("eventMode", "all".to_owned()),
        ] {
            document.set("HttpHostNotification", &[name], value.into())?;
        }
        Ok(Self { id, document })
    }
    /// Returns the configured host ID.
    pub const fn id(&self) -> u32 {
        self.id
    }
    /// Creates a callback-host entry on a supporting device.
    ///
    /// # Errors
    /// Rejects oversized serialized configurations.
    pub fn create(&self) -> Result<Request, Error> {
        Request::with_body(
            Method::Post,
            "/ISAPI/Event/notification/httpHosts",
            Format::Xml,
            self.document.to_bytes()?,
        )
    }
    /// Replaces this callback-host entry explicitly.
    ///
    /// # Errors
    /// Rejects oversized serialized configurations.
    pub fn update(&self) -> Result<Request, Error> {
        Request::put(
            format!("/ISAPI/Event/notification/httpHosts/{}", self.id),
            self.document.to_bytes()?,
        )
    }
    /// Deletes a selected callback-host entry.
    ///
    /// # Errors
    /// Rejects host zero.
    pub fn delete(id: u32) -> Result<Request, Error> {
        Request::delete(format!(
            "/ISAPI/Event/notification/httpHosts/{}",
            nonzero(id)?
        ))
    }
    /// Asks a camera to test one configured callback destination.
    ///
    /// # Errors
    /// Rejects host zero.
    pub fn test(id: u32) -> Result<Request, Error> {
        Request::with_body(
            Method::Post,
            format!("/ISAPI/Event/notification/httpHosts/{}/test", nonzero(id)?),
            Format::Xml,
            [],
        )
    }
}

impl fmt::Debug for CallbackHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallbackHost")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
