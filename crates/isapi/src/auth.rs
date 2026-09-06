use std::fmt;

use digest_auth::{AuthContext, HttpMethod, WwwAuthenticateHeader};

use crate::error::Kind;
use crate::{Error, Method, Request};

const AUTH_HEADER_SIZE_BYTES_MAX: usize = 8192;
const AUTH_FIELD_SIZE_BYTES_MAX: usize = 1024;

mod callback;
pub use callback::CallbackAuth;

/// Camera credentials whose diagnostic representation never reveals either value.
pub struct Credentials {
    username: String,
    password: String,
}

impl Credentials {
    /// Takes ownership of credentials; [`Session::new`] validates their bounds.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Credentials([REDACTED])")
    }
}

/// A wire-ready Digest authorization value with a redacted diagnostic representation.
pub struct Authorization(String);

impl Authorization {
    /// Exposes the sensitive wire value only for transmission to the selected camera.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Authorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Authorization([REDACTED])")
    }
}

/// Per-camera Digest state driven only by supplied challenges, requests, and nonces.
pub struct Session {
    credentials: Credentials,
    challenge: Option<WwwAuthenticateHeader>,
}

impl Session {
    /// Validates credentials without authentication traffic or operating-system calls.
    ///
    /// # Errors
    /// Rejects empty usernames, unsafe header characters, and credentials over 1 KiB.
    pub fn new(credentials: Credentials) -> Result<Self, Error> {
        if credentials.username.is_empty()
            || !quoted_value_is_safe(&credentials.username)
            || credentials.password.len() > AUTH_FIELD_SIZE_BYTES_MAX
        {
            return Err(Error::new(Kind::InvalidInput));
        }
        Ok(Self {
            credentials,
            challenge: None,
        })
    }

    /// Installs a Digest challenge and reports whether the server declared it stale.
    ///
    /// # Errors
    /// Rejects Basic, malformed or oversized challenges, and unsafe quoted fields.
    /// A rejected challenge clears the previous authentication state.
    pub fn handle_challenge(&mut self, header: impl AsRef<str>) -> Result<bool, Error> {
        self.challenge = None;
        let header = header.as_ref();
        if header.len() > AUTH_HEADER_SIZE_BYTES_MAX
            || header.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(Error::new(Kind::Authentication));
        }
        let (scheme, parameters) = header
            .split_once(' ')
            .ok_or_else(|| Error::new(Kind::Authentication))?;
        if !scheme.eq_ignore_ascii_case("Digest") {
            return Err(Error::new(Kind::Authentication));
        }
        let challenge = digest_auth::parse(parameters)?;
        if challenge.nonce.is_empty()
            || !quoted_value_is_safe(&challenge.realm)
            || !quoted_value_is_safe(&challenge.nonce)
            || challenge
                .opaque
                .as_deref()
                .is_some_and(|value| !quoted_value_is_safe(value))
        {
            return Err(Error::new(Kind::Authentication));
        }
        let stale = challenge.stale;
        self.challenge = Some(challenge);
        Ok(stale)
    }

    /// Signs a request with caller-supplied entropy and advances the cached nonce count.
    ///
    /// Returns `None` until a challenge has been received. The caller must generate
    /// an unpredictable `client_nonce`; this method never generates randomness.
    ///
    /// # Errors
    /// Rejects empty or unsafe client nonces, exhausted counts, and unsupported algorithms.
    pub fn authorization(
        &mut self,
        request: &Request,
        client_nonce: impl AsRef<str>,
    ) -> Result<Option<Authorization>, Error> {
        let Some(challenge) = self.challenge.as_mut() else {
            return Ok(None);
        };
        let client_nonce = client_nonce.as_ref();
        if client_nonce.is_empty()
            || !quoted_value_is_safe(client_nonce)
            || challenge.nc == u32::MAX
        {
            return Err(Error::new(Kind::Authentication));
        }
        let method = match request.method() {
            Method::Get => HttpMethod::GET,
            Method::Put => HttpMethod::PUT,
            Method::Post => HttpMethod::POST,
            Method::Delete => HttpMethod::DELETE,
        };
        let mut context = AuthContext::new_with_method(
            self.credentials.username.as_str(),
            self.credentials.password.as_str(),
            request.resource(),
            Some(request.body()),
            method,
        );
        context.set_custom_cnonce(client_nonce);
        Ok(Some(Authorization(
            challenge.respond(&context)?.to_string(),
        )))
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Session")
            .field("has_challenge", &self.challenge.is_some())
            .finish_non_exhaustive()
    }
}

fn quoted_value_is_safe(value: &str) -> bool {
    value.len() <= AUTH_FIELD_SIZE_BYTES_MAX
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'"' | b'\\'))
}
