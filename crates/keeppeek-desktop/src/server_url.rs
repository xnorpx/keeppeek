//! Validated canonical HTTP(S) origin types for KeepPeek servers.
//!
//! `ServerOrigin` guarantees that every server target uses `http` or `https`,
//! omits credentials, path, query, and fragment components, rejects wildcard
//! bind addresses, and fits within a 512-byte bound.

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use url::{Host, Url};

/// Maximum serialized length of a KeepPeek server origin in UTF-8 bytes.
pub const MAX_SERVER_ORIGIN_BYTES: usize = 512;

/// Default local KeepPeek server origin when no server URL is configured.
pub const DEFAULT_SERVER_ORIGIN: &str = "http://127.0.0.1:8081";

/// Validated canonical HTTP(S) origin of a KeepPeek server.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServerOrigin(String);

/// Error returned when a server URL fails origin validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerOriginError {
    /// The supplied string is empty or exceeds `MAX_SERVER_ORIGIN_BYTES`.
    InvalidLength,
    /// The supplied string contains ASCII control characters.
    ControlCharacter,
    /// The URL parser rejected the supplied string.
    MalformedUrl(String),
    /// The scheme is neither `http` nor `https`.
    UnsupportedScheme(String),
    /// The URL includes a username or password.
    CredentialsNotAllowed,
    /// The URL includes a path, query, or fragment component.
    ExtraUrlComponents,
    /// The host is missing or uses an unspecified bind address.
    InvalidHost(String),
    /// The port is zero.
    InvalidPort,
    /// The supplied string is not in canonical ASCII origin form.
    NonCanonical { expected: String },
}

impl fmt::Display for ServerOriginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => write!(
                formatter,
                "Server URL must be between 1 and {MAX_SERVER_ORIGIN_BYTES} bytes."
            ),
            Self::ControlCharacter => {
                write!(formatter, "Server URL must not contain control characters.")
            }
            Self::MalformedUrl(detail) => write!(formatter, "Server URL is invalid: {detail}."),
            Self::UnsupportedScheme(scheme) => write!(
                formatter,
                "Server URL scheme `{scheme}` is not supported. Use http:// or https://."
            ),
            Self::CredentialsNotAllowed => {
                write!(
                    formatter,
                    "Server URL must not contain a username or password."
                )
            }
            Self::ExtraUrlComponents => write!(
                formatter,
                "Server URL must contain only a scheme, host, and optional port."
            ),
            Self::InvalidHost(reason) => write!(formatter, "Server URL host is invalid: {reason}."),
            Self::InvalidPort => write!(formatter, "Server URL port must be between 1 and 65535."),
            Self::NonCanonical { expected } => write!(
                formatter,
                "Server URL must use its canonical origin: {expected}."
            ),
        }
    }
}

impl std::error::Error for ServerOriginError {}

impl ServerOrigin {
    /// Parses and normalizes user-supplied input into a canonical `ServerOrigin`.
    ///
    /// Leading and trailing whitespace and a single trailing `/` are removed
    /// before canonicalization. Credentials, non-root paths, queries,
    /// fragments, unsupported schemes, and wildcard bind addresses are rejected.
    ///
    /// # Errors
    ///
    /// Returns a [`ServerOriginError`] when the input is not a valid HTTP(S)
    /// server origin.
    pub fn parse_user_input(raw: &str) -> Result<Self, ServerOriginError> {
        if raw.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
            return Err(ServerOriginError::ControlCharacter);
        }
        let trimmed = raw.trim();
        let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
        let parsed = parse_and_check_parts(trimmed)?;
        let canonical = canonicalize_origin(&parsed)?;
        if canonical.len() > MAX_SERVER_ORIGIN_BYTES {
            return Err(ServerOriginError::InvalidLength);
        }
        Ok(Self(canonical))
    }

    /// Parses an already canonical server origin stored in `config.toml`.
    ///
    /// # Errors
    ///
    /// Returns a [`ServerOriginError`] when `raw` is invalid or differs from
    /// its canonical ASCII serialization.
    pub fn parse_canonical(raw: &str) -> Result<Self, ServerOriginError> {
        let parsed = parse_and_check_parts(raw)?;
        let canonical = canonicalize_origin(&parsed)?;
        if canonical != raw {
            return Err(ServerOriginError::NonCanonical {
                expected: canonical,
            });
        }
        Ok(Self(canonical))
    }

    /// Returns the default local server origin (`http://127.0.0.1:8081`).
    #[must_use]
    pub fn default_local() -> Self {
        Self(DEFAULT_SERVER_ORIGIN.to_owned())
    }

    /// Derives a reachable local client origin from a server bind host and port.
    ///
    /// Wildcard listener addresses (`0.0.0.0`, `::`, or empty strings) map to
    /// `127.0.0.1`.
    ///
    /// # Errors
    ///
    /// Returns a [`ServerOriginError`] when the port is zero or the host is
    /// not a valid URL authority.
    pub fn from_server_bind(host: &str, port: u16) -> Result<Self, ServerOriginError> {
        if port == 0 {
            return Err(ServerOriginError::InvalidPort);
        }
        let trimmed = host.trim().trim_matches(['[', ']']);
        let client_host = if trimmed.is_empty() || trimmed == "0.0.0.0" || trimmed == "::" {
            "127.0.0.1"
        } else {
            trimmed
        };
        let candidate = if client_host.contains(':') {
            format!("http://[{client_host}]:{port}")
        } else {
            format!("http://{client_host}:{port}")
        };
        Self::parse_user_input(&candidate)
    }

    /// Returns the canonical ASCII origin string without a trailing slash.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the root application entry URL (`<origin>/`).
    ///
    /// # Panics
    ///
    /// Panics if the validated internal origin cannot be parsed as a URL.
    #[must_use]
    pub fn app_entry_url(&self) -> Url {
        let joined = format!("{}/", self.0);
        Url::parse(&joined).expect("validated ServerOrigin must produce a valid app entry URL")
    }

    /// Returns the KeepPeek session probe URL (`<origin>/auth/session`).
    ///
    /// # Panics
    ///
    /// Panics if the validated internal origin cannot be parsed as a URL.
    #[must_use]
    pub fn session_probe_url(&self) -> Url {
        let joined = format!("{}/auth/session", self.0);
        Url::parse(&joined).expect("validated ServerOrigin must produce a valid session probe URL")
    }

    /// Returns true when the origin uses `https`.
    #[must_use]
    pub fn is_https(&self) -> bool {
        self.0.starts_with("https://")
    }

    /// Returns true when the origin targets loopback (`localhost`, `127.0.0.0/8`, or `::1`).
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        let Ok(url) = Url::parse(&self.0) else {
            return false;
        };
        match url.host() {
            Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        }
    }
}

impl fmt::Display for ServerOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn parse_and_check_parts(raw: &str) -> Result<Url, ServerOriginError> {
    if raw.is_empty() || raw.len() > MAX_SERVER_ORIGIN_BYTES {
        return Err(ServerOriginError::InvalidLength);
    }
    if raw.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(ServerOriginError::ControlCharacter);
    }
    let parsed =
        Url::parse(raw).map_err(|error| ServerOriginError::MalformedUrl(error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ServerOriginError::UnsupportedScheme(
            parsed.scheme().to_owned(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ServerOriginError::CredentialsNotAllowed);
    }
    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ServerOriginError::ExtraUrlComponents);
    }
    if parsed.port() == Some(0) {
        return Err(ServerOriginError::InvalidPort);
    }
    validate_host(&parsed)?;
    Ok(parsed)
}

fn validate_host(parsed: &Url) -> Result<(), ServerOriginError> {
    match parsed.host() {
        Some(Host::Ipv4(address)) if IpAddr::V4(address).is_unspecified() => {
            Err(ServerOriginError::InvalidHost(
                "use 127.0.0.1 or localhost instead of 0.0.0.0".to_owned(),
            ))
        }
        Some(Host::Ipv6(address)) if IpAddr::V6(address).is_unspecified() => Err(
            ServerOriginError::InvalidHost("use [::1] or localhost instead of [::]".to_owned()),
        ),
        Some(Host::Domain("")) => Err(ServerOriginError::InvalidHost(
            "host must not be empty".to_owned(),
        )),
        Some(_) => Ok(()),
        None => Err(ServerOriginError::InvalidHost(
            "host is required".to_owned(),
        )),
    }
}

fn canonicalize_origin(parsed: &Url) -> Result<String, ServerOriginError> {
    let canonical = parsed.origin().ascii_serialization();
    if canonical == "null" {
        return Err(ServerOriginError::InvalidHost(
            "origin cannot be serialized".to_owned(),
        ));
    }
    Ok(canonical)
}
