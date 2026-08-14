use crate::soap::client::Credentials;
use nonzero_ext::nonzero;
use std::{
    fmt::{Debug, Formatter},
    num::NonZeroU8,
};
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid state")]
    InvalidState,
    #[error("No credentials")]
    NoCredentials,
    #[error("Digest {0}")]
    Digest(String),
}

pub struct Digest {
    creds: Option<Credentials>,
    uri: Url,
    state: State,
    reuse_headers: bool,
}

enum State {
    Default,
    Got401 { challenge: String, count: NonZeroU8 },
}

impl Digest {
    pub fn new(uri: &Url, creds: &Option<Credentials>, reuse_headers: bool) -> Self {
        Self {
            creds: creds.clone(),
            uri: uri.clone(),
            state: State::Default,
            reuse_headers,
        }
    }
}

impl Digest {
    /// Call this when the authentication was successful.
    pub fn set_success(&mut self) {
        if !self.reuse_headers {
            // Since we don't need to preserve the headers, reset all the state to default.
            *self = Self::new(&self.uri, &self.creds, self.reuse_headers);
            return;
        }

        if let State::Got401 { count, .. } = &mut self.state {
            // We always store at least one request, so it's never zero.
            *count = nonzero!(1_u8);
        }
    }

    /// Call this when received 401 Unauthorized.
    pub fn set_401(&mut self, challenge: String) {
        self.state = match self.state {
            State::Default => State::Got401 {
                challenge,
                count: nonzero!(1_u8),
            },
            State::Got401 { count, .. } => State::Got401 {
                challenge,
                count: count.saturating_add(1),
            },
        }
    }

    pub const fn is_failed(&self) -> bool {
        match &self.state {
            State::Default => false,
            // Possible scenarios:
            // - We've got 401 with a challenge for the first time, we calculate the answer, then
            //   we get 200 OK. So, a single 401 is never a failure.
            // - After successful auth the count is 1 because we always store at least one request,
            //   and the caller decided to reuse the same challenge for multiple requests. But at
            //   some point, we'll get a 401 with a new challenge and `stale=true`.
            //   So, we'll get a second 401, and this is also not a failure because after
            //   calculating the answer to the challenge, we'll get a 200 OK, and will reset the
            //   counter in `set_success()`.
            // - Three 401's in a row is certainly a failure.
            State::Got401 { count, .. } => count.get() >= 3,
        }
    }

    pub fn authorization_header(&self) -> Result<Option<String>, Error> {
        match &self.state {
            State::Default => Ok(None),
            State::Got401 { challenge, .. } => {
                let creds = self.creds.as_ref().ok_or(Error::NoCredentials)?;
                Ok(Some(digest_auth(challenge, creds, &self.uri)?))
            }
        }
    }
}

fn digest_auth(www_authenticate: &str, creds: &Credentials, url: &Url) -> Result<String, Error> {
    let mut context = digest_auth::AuthContext::new(&creds.username, &creds.password, url.path());

    context.method = digest_auth::HttpMethod::POST;

    Ok(digest_auth::parse(www_authenticate)
        .map_err(|e| Error::Digest(e.to_string()))?
        .respond(&context)
        .map_err(|e| Error::Digest(e.to_string()))?
        .to_string())
}

impl Debug for Digest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Digest")
            .field("creds", &self.creds)
            .field("state", &self.state)
            .finish()
    }
}

impl Debug for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => write!(f, "FirstRequest")?,
            Self::Got401 { count, .. } => write!(f, "Got401({count})")?,
        };

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_authorization_header_from_stored_challenge() {
        let credentials = Some(Credentials {
            username: "admin".to_string(),
            password: "secret".to_string(),
        });
        let uri = Url::parse("http://camera.example/onvif/device_service").unwrap();
        let mut digest = Digest::new(&uri, &credentials, false);

        assert_eq!(digest.authorization_header().unwrap(), None);

        digest.set_401("Digest realm=\"camera\", nonce=\"nonce\", qop=\"auth\"".to_string());
        let authorization = digest.authorization_header().unwrap().unwrap();

        assert!(authorization.starts_with("Digest "));
        assert!(authorization.contains("username=\"admin\""));

        digest.set_success();
        assert_eq!(digest.authorization_header().unwrap(), None);
    }
}
