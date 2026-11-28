use crate::soap::client::Credentials;
use std::fmt::{Debug, Formatter};
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
    reuse_headers: bool,
    /// The last 401 WWW-Authenticate challenge that the next request answers.
    challenge: Option<String>,
    /// Count of 401 responses to credentialed requests since the last success.
    rejections: u8,
}

impl Digest {
    pub fn new(uri: &Url, creds: &Option<Credentials>, reuse_headers: bool) -> Self {
        Self {
            creds: creds.clone(),
            uri: uri.clone(),
            reuse_headers,
            challenge: None,
            rejections: 0,
        }
    }
}

impl Digest {
    /// Call this when the authentication was successful.
    pub fn set_success(&mut self) {
        self.rejections = 0;

        if !self.reuse_headers {
            self.challenge = None;
        }
    }

    /// Call this when a non-success, non-401 response consumed the challenge.
    pub fn discard_challenge(&mut self) {
        self.challenge = None;
    }

    /// Call this when received 401 Unauthorized.
    pub fn set_401(&mut self, challenge: String) {
        if self.challenge.is_some() {
            self.rejections = self.rejections.saturating_add(1);
        }

        self.challenge = Some(challenge);
    }

    pub const fn is_failed(&self) -> bool {
        // A 401 to an unauthenticated request supplies the initial challenge.
        // One rejected credentialed request can be a stale nonce retry; a second
        // rejected answer since the last success indicates failed credentials.
        self.challenge.is_some() && self.rejections >= 2
    }

    pub fn authorization_header(&self) -> Result<Option<String>, Error> {
        match &self.challenge {
            None => Ok(None),
            Some(challenge) => {
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
        .map_err(|_| Error::Digest("invalid WWW-Authenticate challenge".to_string()))?
        .respond(&context)
        .map_err(|_| Error::Digest("unable to compute Digest response".to_string()))?
        .to_string())
}

impl Debug for Digest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Digest")
            .field("creds", &self.creds)
            .field("challenge", &self.challenge.is_some())
            .field("rejections", &self.rejections)
            .finish()
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
