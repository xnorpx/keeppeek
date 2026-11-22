use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::fmt;

/// A bearer value returned only when issued; Debug never exposes its contents.
pub struct Secret(String);

impl Secret {
    pub(super) fn issue() -> Self {
        Self(URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()))
    }

    pub(super) fn hash(&self) -> [u8; 32] {
        digest(&self.0)
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

pub(super) fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

pub(super) fn valid(value: &str) -> bool {
    value.len() == 43
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|decoded| decoded.len() == 32)
}
