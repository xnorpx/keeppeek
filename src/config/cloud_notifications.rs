//! Configuration for optional bounded Keeppeek Service mobile push signaling.
//!
//! Settings live under `[cloud_notifications]` in `config.toml` and default to off.
//! Signing keys remain `{secret:KEY}` references in configuration and are resolved only when
//! signing an outbound signal.

use std::fmt;

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// Maximum serialized length for the canonical HTTPS service origin.
pub const MAX_SERVICE_ORIGIN_BYTES: usize = 512;

/// Maximum length for a referenced secret key name.
const MAX_SECRET_KEY_BYTES: usize = 128;

/// Length of an opaque service-assigned NVR identifier in bytes.
pub const NVR_ID_BYTES: usize = 16;

/// Length of a raw Ed25519 private seed in bytes.
pub const ED25519_SEED_BYTES: usize = 32;

/// Configuration persisted under `[cloud_notifications]` in `config.toml`.
#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub image_previews_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvr_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<String>,
}

impl fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CloudNotificationSettings")
            .field("revision", &self.revision)
            .field("enabled", &self.enabled)
            .field("image_previews_enabled", &self.image_previews_enabled)
            .field("paired", &self.is_paired())
            .finish()
    }
}

/// Validated pairing fields extracted from `Settings` without exposing raw secret bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingReference {
    pub service_origin: String,
    pub nvr_id: [u8; NVR_ID_BYTES],
    pub nvr_id_canonical: String,
    pub signing_key_version: u32,
    pub signing_key_reference: String,
}

impl fmt::Debug for PairingReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingReference")
            .field("signing_key_version", &self.signing_key_version)
            .field("paired", &true)
            .finish()
    }
}

/// Resolved Ed25519 signing seed kept in memory only during request signing.
#[derive(Clone)]
pub struct SigningSeed([u8; ED25519_SEED_BYTES]);

impl SigningSeed {
    /// Parses a 32-byte Ed25519 seed from base64, URL-safe base64, or 64-char hex.
    pub fn parse(encoded: &str) -> anyhow::Result<Self> {
        let trimmed = encoded.trim();
        if trimmed.is_empty() {
            anyhow::bail!("cloud notification signing key is empty");
        }
        if let Some(bytes) = decode_fixed_32_hex(trimmed) {
            return Ok(Self(bytes));
        }
        for engine in [&STANDARD, &URL_SAFE_NO_PAD] {
            if let Ok(decoded) = engine.decode(trimmed)
                && decoded.len() == ED25519_SEED_BYTES
            {
                let mut seed = [0_u8; ED25519_SEED_BYTES];
                seed.copy_from_slice(&decoded);
                return Ok(Self(seed));
            }
        }
        anyhow::bail!("cloud notification signing key must decode to 32 bytes")
    }

    /// Returns the raw 32-byte Ed25519 seed.
    pub const fn as_bytes(&self) -> &[u8; ED25519_SEED_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SigningSeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SigningSeed([redacted])")
    }
}

impl Settings {
    /// Returns true when all pairing fields are present.
    pub const fn is_paired(&self) -> bool {
        self.service_origin.is_some()
            && self.nvr_id.is_some()
            && self.signing_key_version.is_some()
            && self.signing_key.is_some()
    }

    /// Validates all configuration bounds, pairing completeness, and preview invariants.
    pub fn validate(&self) -> anyhow::Result<()> {
        let pairing_present = self.service_origin.is_some()
            || self.nvr_id.is_some()
            || self.signing_key_version.is_some()
            || self.signing_key.is_some();
        if pairing_present && !self.is_paired() {
            anyhow::bail!(
                "cloud notification pairing requires origin, nvr_id, signing_key_version, and signing_key"
            );
        }
        if self.image_previews_enabled && (!self.enabled || !self.is_paired()) {
            anyhow::bail!(
                "cloud notification image previews require enabled cloud notifications and active pairing"
            );
        }
        if let Some(origin) = &self.service_origin {
            validate_service_origin(origin)?;
        }
        if let Some(nvr_id) = &self.nvr_id {
            parse_nvr_id(nvr_id)?;
        }
        if let Some(version) = self.signing_key_version {
            anyhow::ensure!(
                version >= 1,
                "cloud notification signing key version must be at least 1"
            );
        }
        if let Some(reference) = &self.signing_key {
            validate_secret_reference(reference)?;
        }
        Ok(())
    }

    /// Returns the validated pairing reference when paired.
    pub fn pairing_reference(&self) -> anyhow::Result<Option<PairingReference>> {
        self.validate()?;
        if !self.is_paired() {
            return Ok(None);
        }
        let service_origin = self
            .service_origin
            .clone()
            .expect("paired settings must have a service origin");
        let raw_nvr_id = self
            .nvr_id
            .as_deref()
            .expect("paired settings must have an nvr_id");
        let nvr_id = parse_nvr_id(raw_nvr_id)?;
        let signing_key_version = self
            .signing_key_version
            .expect("paired settings must have a signing key version");
        let signing_key_reference = self
            .signing_key
            .clone()
            .expect("paired settings must have a signing key reference");
        Ok(Some(PairingReference {
            service_origin,
            nvr_id,
            nvr_id_canonical: Uuid::from_bytes(nvr_id).hyphenated().to_string(),
            signing_key_version,
            signing_key_reference,
        }))
    }
}

/// Parses and validates the `[cloud_notifications]` TOML section without resolving secrets.
pub(super) fn parse_settings(value: toml::Value) -> anyhow::Result<Settings> {
    let settings: Settings = value
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid cloud_notifications settings"))?;
    settings.validate()?;
    Ok(settings)
}

/// Parses a 16-byte NVR identifier encoded as a hyphenated or simple 32-character UUID/hex string.
pub fn parse_nvr_id(value: &str) -> anyhow::Result<[u8; NVR_ID_BYTES]> {
    let trimmed = value.trim();
    let parsed = Uuid::parse_str(trimmed)
        .map_err(|_| anyhow::anyhow!("cloud notification nvr_id must be a valid 16-byte ID"))?;
    Ok(*parsed.as_bytes())
}

/// Validates that the Keeppeek Service destination is a canonical HTTPS origin without path or credentials.
pub fn validate_service_origin(origin: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !origin.is_empty() && origin.len() <= MAX_SERVICE_ORIGIN_BYTES,
        "cloud notification service origin exceeds allowed length"
    );
    let parsed = Url::parse(origin)
        .map_err(|_| anyhow::anyhow!("cloud notification requires a canonical HTTPS origin"))?;
    anyhow::ensure!(
        parsed.scheme() == "https"
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.origin().ascii_serialization() == origin,
        "cloud notification requires a canonical HTTPS origin"
    );
    Ok(())
}

fn validate_secret_reference(reference: &str) -> anyhow::Result<()> {
    let key = reference
        .strip_prefix("{secret:")
        .and_then(|value| value.strip_suffix('}'));
    anyhow::ensure!(
        key.is_some_and(
            |value| value.len() <= MAX_SECRET_KEY_BYTES && super::valid_secret_key(value)
        ),
        "cloud notification signing_key must use a private secret reference"
    );
    Ok(())
}

fn decode_fixed_32_hex(input: &str) -> Option<[u8; ED25519_SEED_BYTES]> {
    if input.len() != ED25519_SEED_BYTES * 2 || !input.is_ascii() {
        return None;
    }
    let bytes = input.as_bytes();
    let mut output = [0_u8; ED25519_SEED_BYTES];
    for (index, slot) in output.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2])?;
        let low = hex_nibble(bytes[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }
    Some(output)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Config, Secrets, config_from_table, load_config, load_configuration_table, update_settings,
        write_configuration_table,
    };
    use std::collections::BTreeMap;

    #[test]
    fn defaults_off_and_preserves_secret_reference_without_resolving() {
        let default = Config::default();
        assert!(!default.cloud_notifications.enabled);
        assert!(!default.cloud_notifications.image_previews_enabled);
        assert!(!default.cloud_notifications.is_paired());

        let source = toml::from_str(
            r#"
            [cloud_notifications]
            revision = 2
            enabled = true
            image_previews_enabled = false
            service_origin = "https://service.keeppeek.example"
            nvr_id = "01020304-0506-0708-090a-0b0c0d0e0f10"
            signing_key_version = 3
            signing_key = "{secret:CLOUD_SIGNING_KEY}"
            "#,
        )
        .unwrap();
        let canary = "secret-signing-canary-value-1234";
        let secrets = Secrets(BTreeMap::from([(
            "CLOUD_SIGNING_KEY".to_owned(),
            canary.to_owned(),
        )]));
        for secrets in [Secrets::default(), secrets] {
            let config = config_from_table(&source, &secrets).unwrap();
            let serialized = toml::to_string(&config).unwrap();
            assert!(config.cloud_notifications.enabled);
            assert!(!config.cloud_notifications.image_previews_enabled);
            assert!(config.cloud_notifications.is_paired());
            assert_eq!(
                config.cloud_notifications.signing_key.as_deref(),
                Some("{secret:CLOUD_SIGNING_KEY}")
            );
            assert!(serialized.contains("{secret:CLOUD_SIGNING_KEY}"));
            assert!(!serialized.contains(canary));
            let debug_output = format!("{:?}", config.cloud_notifications);
            assert!(!debug_output.contains("service.keeppeek.example"));
            assert!(!debug_output.contains("01020304"));
            assert!(!debug_output.contains(canary));
        }
    }

    #[test]
    fn rejects_invalid_preview_or_pairing_settings_without_echoing_private_values() {
        for entry in [
            "enabled = false\nimage_previews_enabled = true",
            "enabled = true\nimage_previews_enabled = true",
            "service_origin = \"http://service.keeppeek.example\"\nnvr_id = \"0102030405060708090a0b0c0d0e0f10\"\nsigning_key_version = 1\nsigning_key = \"{secret:K}\"",
            "service_origin = \"https://private-canary@service.keeppeek.example\"\nnvr_id = \"0102030405060708090a0b0c0d0e0f10\"\nsigning_key_version = 1\nsigning_key = \"{secret:K}\"",
            "service_origin = \"https://service.keeppeek.example/private-canary\"\nnvr_id = \"0102030405060708090a0b0c0d0e0f10\"\nsigning_key_version = 1\nsigning_key = \"{secret:K}\"",
            "service_origin = \"https://service.keeppeek.example\"\nnvr_id = \"0102030405060708090a0b0c0d0e0f10\"\nsigning_key_version = 0\nsigning_key = \"{secret:K}\"",
            "service_origin = \"https://service.keeppeek.example\"\nnvr_id = \"0102030405060708090a0b0c0d0e0f10\"\nsigning_key_version = 1\nsigning_key = \"private-canary\"",
            "service_origin = \"https://service.keeppeek.example\"\nnvr_id = \"0102030405060708090a0b0c0d0e0f10\"\nsigning_key_version = 1\nsigning_key = \"{secret:K|url}\"",
            "service_origin = \"https://service.keeppeek.example\"\nnvr_id = \"not-a-uuid\"\nsigning_key_version = 1\nsigning_key = \"{secret:K}\"",
            "service_origin = \"https://service.keeppeek.example\"",
        ] {
            let root = toml::from_str(&format!("[cloud_notifications]\n{entry}")).unwrap();
            let error = config_from_table(&root, &Secrets::default()).err().unwrap();
            assert!(
                !error.to_string().contains("private-canary"),
                "error leaked private value: {error}"
            );
        }
    }

    #[test]
    fn survives_unrelated_writes_and_validates_signing_seed_parsing() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-cloud-notif-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        let source = toml::from_str(
            r#"
            unrelated = "keep"

            [cloud_notifications]
            revision = 4
            enabled = true
            image_previews_enabled = true
            service_origin = "https://service.keeppeek.example:8443"
            nvr_id = "01020304-0506-0708-090a-0b0c0d0e0f10"
            signing_key_version = 2
            signing_key = "{secret:CLOUD_SIGNING_KEY}"
            "#,
        )
        .unwrap();
        write_configuration_table(&path, &source).unwrap();
        let mut config = load_config(&path).unwrap();
        config.port = 8089;
        update_settings(&path, &config).unwrap();
        let reloaded = load_configuration_table(&path).unwrap();
        assert_eq!(
            reloaded["cloud_notifications"],
            source["cloud_notifications"]
        );
        assert_eq!(reloaded["unrelated"], source["unrelated"]);

        let raw_seed = [0x5a_u8; 32];
        let b64 = STANDARD.encode(raw_seed);
        let hex = "5a".repeat(32);
        assert_eq!(SigningSeed::parse(&b64).unwrap().as_bytes(), &raw_seed);
        assert_eq!(SigningSeed::parse(&hex).unwrap().as_bytes(), &raw_seed);
        assert!(format!("{:?}", SigningSeed::parse(&b64).unwrap()).contains("redacted"));
        assert!(SigningSeed::parse("short").is_err());

        std::fs::remove_dir_all(directory).unwrap();
    }
}
