use std::fmt;

use url::{Host, Url};

const URL_BYTES_MAX: usize = 4096;

/// A camera-bound endpoint whose diagnostics omit private paths and queries.
#[derive(Clone, Eq, PartialEq)]
pub struct Endpoint(Url);

/// An invalid or foreign camera endpoint, without the rejected URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointError;

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid or unapproved ONVIF endpoint")
    }
}

impl std::error::Error for EndpointError {}

impl Endpoint {
    /// Validates an explicitly configured HTTP(S) endpoint without connecting.
    ///
    /// # Errors
    /// Rejects credentials, query strings, fragments, wildcards and multicast hosts.
    pub fn new(configured: impl AsRef<str>) -> Result<Self, EndpointError> {
        let text = configured.as_ref();
        validate_text(text)?;
        let url = Url::parse(text).map_err(|_| EndpointError)?;
        validate_url(&url)?;
        if url.query().is_some() || wildcard(&url) || multicast(&url) {
            return Err(EndpointError);
        }
        Ok(Self(url))
    }

    /// Resolves a returned endpoint against the approved camera host.
    ///
    /// A wildcard host is replaced with the configured host and port. Other hosts
    /// are rejected without DNS lookup. HTTPS cannot become HTTP. Same-camera
    /// service ports are permitted; they never authorize another camera host.
    ///
    /// # Errors
    /// Rejects foreign hosts, invalid URLs, embedded credentials and TLS downgrades.
    pub fn resolve(&self, advertised: impl AsRef<str>) -> Result<Self, EndpointError> {
        let text = advertised.as_ref();
        validate_text(text)?;
        if text.is_empty() {
            return Err(EndpointError);
        }
        let mut url = self.0.join(text).map_err(|_| EndpointError)?;
        validate_url(&url)?;
        if self.0.scheme() == "https" && url.scheme() != "https" {
            return Err(EndpointError);
        }
        if wildcard(&url) {
            url.set_host(self.0.host_str()).map_err(|_| EndpointError)?;
            url.set_port(self.0.port_or_known_default())
                .map_err(|()| EndpointError)?;
        }
        if url.host() != self.0.host() {
            return Err(EndpointError);
        }
        Ok(Self(url))
    }

    /// Returns the private request URL. Do not log or publish this value.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

fn validate_text(text: &str) -> Result<(), EndpointError> {
    if text.len() > URL_BYTES_MAX
        || text.contains('\\')
        || text.chars().any(char::is_whitespace)
        || text.chars().any(char::is_control)
    {
        return Err(EndpointError);
    }
    Ok(())
}

fn validate_url(url: &Url) -> Result<(), EndpointError> {
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(EndpointError);
    }
    Ok(())
}

fn wildcard(url: &Url) -> bool {
    matches!(url.host(), Some(Host::Ipv4(ip)) if ip.is_unspecified())
        || matches!(url.host(), Some(Host::Ipv6(ip)) if ip.is_unspecified())
}

fn multicast(url: &Url) -> bool {
    matches!(url.host(), Some(Host::Ipv4(ip)) if ip.is_multicast() || ip.is_broadcast())
        || matches!(url.host(), Some(Host::Ipv6(ip)) if ip.is_multicast())
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Endpoint").finish_non_exhaustive()
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ONVIF camera endpoint")
    }
}
