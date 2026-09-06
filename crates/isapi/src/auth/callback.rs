use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use digest_auth::{Algorithm, AlgorithmType, AuthContext, AuthorizationHeader, HttpMethod, Qop};
use subtle::ConstantTimeEq;

use super::{Credentials, Session, quoted_value_is_safe};
use crate::Error;
use crate::error::Kind;

const REALM: &str = "keeppeek-isapi";
const NONCE_TTL: Duration = Duration::from_secs(60);
const NONCES_MAX: usize = 8;

struct Nonce {
    issued: Duration,
    count: u32,
}

/// A bounded MD5 Digest verifier for one configured callback sender.
///
/// The caller supplies unpredictable nonces and monotonic time. Only qop=auth
/// is accepted so authentication can finish before reading an upload body.
/// Digest does not encrypt the callback; use verified HTTPS or an isolated LAN.
pub struct CallbackAuth {
    credentials: Credentials,
    nonces: BTreeMap<String, Nonce>,
    now: Duration,
}

impl CallbackAuth {
    /// Validates private receiver credentials without reading sockets, clocks or entropy.
    ///
    /// # Errors
    /// Returns the credential errors from [`Session::new`].
    pub fn new(credentials: Credentials) -> Result<Self, Error> {
        let session = Session::new(credentials)?;
        if session.credentials.password.is_empty() {
            return Err(Error::new(Kind::InvalidInput));
        }
        Ok(Self {
            credentials: session.credentials,
            nonces: BTreeMap::new(),
            now: Duration::ZERO,
        })
    }

    /// Issues a one-minute challenge using at least 128 bits of caller-generated nonce text.
    ///
    /// # Errors
    /// Rejects non-hexadecimal, repeated, oversized or short nonces and backwards time.
    pub fn challenge(&mut self, nonce: &str, now: Duration) -> Result<String, Error> {
        self.advance(now)?;
        if !(32..=128).contains(&nonce.len())
            || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.nonces.contains_key(nonce)
        {
            return Err(Error::new(Kind::InvalidInput));
        }
        if self.nonces.len() >= NONCES_MAX {
            let oldest = self
                .nonces
                .iter()
                .min_by_key(|(_, nonce)| nonce.issued)
                .map(|(key, _)| key.clone())
                .expect("nonce limit is nonzero");
            self.nonces.remove(&oldest);
        }
        self.nonces.insert(
            nonce.to_owned(),
            Nonce {
                issued: now,
                count: 0,
            },
        );
        Ok(format!(
            "Digest realm=\"{REALM}\", nonce=\"{nonce}\", algorithm=MD5, qop=\"auth\", stale=true"
        ))
    }

    /// Authenticates an exact POST target and rejects reused or decreasing nonce counts.
    ///
    /// # Errors
    /// Rejects invalid credentials, unsupported schemes/algorithms, expired nonces,
    /// mismatched request targets, missing qop/cnonce and backwards caller time.
    pub fn verify(
        &mut self,
        value: &str,
        method: &str,
        uri: &str,
        now: Duration,
    ) -> Result<(), Error> {
        self.advance(now)?;
        if method != "POST"
            || uri.len() > 4096
            || value.len() > 8192
            || value.bytes().any(|byte| byte.is_ascii_control())
            || !value.starts_with("Digest ")
        {
            return Err(Error::new(Kind::Authentication));
        }
        let mut parsed = AuthorizationHeader::parse(value)?;
        if parsed.realm != REALM
            || parsed.username != self.credentials.username
            || parsed.uri != uri
            || parsed.qop != Some(Qop::AUTH)
            || parsed.algorithm != Algorithm::new(AlgorithmType::MD5, false)
            || parsed.userhash
            || parsed.opaque.is_some()
            || parsed
                .cnonce
                .as_deref()
                .is_none_or(|value| value.is_empty() || !quoted_value_is_safe(value))
        {
            return Err(Error::new(Kind::Authentication));
        }
        let nonce = self
            .nonces
            .get(&parsed.nonce)
            .ok_or_else(|| Error::new(Kind::Authentication))?;
        if parsed.nc == 0
            || parsed.nc <= nonce.count
            || parsed.response.len() != 32
            || !parsed.response.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::new(Kind::Authentication));
        }
        let supplied = parsed.response.to_ascii_lowercase();
        let context = AuthContext::new_with_method(
            self.credentials.username.as_str(),
            self.credentials.password.as_str(),
            uri,
            None::<&[u8]>,
            HttpMethod::POST,
        );
        parsed.digest(&context);
        if !bool::from(supplied.as_bytes().ct_eq(parsed.response.as_bytes())) {
            return Err(Error::new(Kind::Authentication));
        }
        self.nonces
            .get_mut(&parsed.nonce)
            .expect("verified nonce remains registered")
            .count = parsed.nc;
        Ok(())
    }

    fn advance(&mut self, now: Duration) -> Result<(), Error> {
        if now < self.now {
            return Err(Error::new(Kind::InvalidInput));
        }
        self.now = now;
        self.nonces
            .retain(|_, nonce| now.saturating_sub(nonce.issued) < NONCE_TTL);
        Ok(())
    }
}

impl fmt::Debug for CallbackAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallbackAuth")
            .field("nonce_count", &self.nonces.len())
            .finish_non_exhaustive()
    }
}
