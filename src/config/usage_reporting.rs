//! Consent configuration for opt-in usage reporting stored in `config.toml`.
//!
//! All categories default to disabled so fresh installations and upgrades make zero
//! telemetry requests and generate no installation UUID until explicit Administrator opt-in.

use super::*;
use crate::usage_reporting::model::CURRENT_DISCLOSURE_REVISION;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UsageReportingConfig {
    #[serde(default)]
    pub basic: bool,
    #[serde(default)]
    pub camera_compatibility: bool,
    #[serde(default)]
    pub feature_adoption: bool,
    #[serde(default)]
    pub consented_disclosure_revision: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_origin: Option<String>,
}

impl UsageReportingConfig {
    pub const fn any_enabled(&self) -> bool {
        self.basic || self.camera_compatibility || self.feature_adoption
    }

    pub const fn renewal_required(&self, current_disclosure_revision: u32) -> bool {
        self.basic
            && self.consented_disclosure_revision > 0
            && self.consented_disclosure_revision < current_disclosure_revision
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        self.validate_against_disclosure(CURRENT_DISCLOSURE_REVISION)
    }

    pub fn validate_against_disclosure(
        &self,
        current_disclosure_revision: u32,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.basic || (!self.camera_compatibility && !self.feature_adoption),
            "basic usage reporting must be enabled before camera compatibility or feature adoption"
        );
        if self.basic {
            anyhow::ensure!(
                self.consented_disclosure_revision >= 1
                    && self.consented_disclosure_revision <= current_disclosure_revision,
                "enabled usage reporting requires a valid consented disclosure revision"
            );
        } else {
            anyhow::ensure!(
                self.consented_disclosure_revision <= current_disclosure_revision,
                "usage reporting disclosure revision exceeds supported revision"
            );
        }
        if let Some(origin) = &self.receiver_origin {
            validate_receiver_origin(origin)?;
        }
        Ok(())
    }
}

pub fn validate_receiver_origin(origin: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !origin.is_empty() && origin.len() <= 512 && !origin.chars().any(char::is_control),
        "usage reporting receiver origin must contain 1 to 512 bytes"
    );
    let parsed =
        Url::parse(origin).map_err(|_| anyhow::anyhow!("invalid usage reporting origin"))?;
    anyhow::ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.origin().ascii_serialization() == *origin,
        "usage reporting receiver origin must be a canonical HTTPS origin without a path or credentials"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError {
    RevisionConflict,
    Validation,
    Persistence,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RevisionConflict => "configuration revision conflicts",
            Self::Validation => "usage reporting settings could not be validated",
            Self::Persistence => "usage reporting settings persistence failed",
        })
    }
}

impl std::error::Error for UpdateError {}

/// Updates `[usage_reporting]` atomically in `config.toml` after verifying revision and validity.
pub fn update(
    path: &Path,
    expected_configuration_revision: &str,
    settings: UsageReportingConfig,
) -> Result<(Config, String), UpdateError> {
    update_with_writer(
        path,
        expected_configuration_revision,
        settings,
        write_private_file_atomically,
    )
}

pub fn table_revision(root: &toml::Table) -> String {
    use sha2::Digest as _;
    let serialized = toml::to_string(root).unwrap_or_default();
    sha2::Sha256::digest(serialized.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn update_with_writer(
    path: &Path,
    expected_configuration_revision: &str,
    settings: UsageReportingConfig,
    writer: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
) -> Result<(Config, String), UpdateError> {
    let text = std::fs::read_to_string(path).map_err(|_| UpdateError::Persistence)?;
    let mut root: toml::Table = toml::from_str(&text).map_err(|_| UpdateError::Validation)?;
    let current_revision = table_revision(&root);
    if expected_configuration_revision.is_empty()
        || current_revision != expected_configuration_revision
    {
        return Err(UpdateError::RevisionConflict);
    }
    let secrets = load_secrets(path).map_err(|_| UpdateError::Persistence)?;
    settings.validate().map_err(|_| UpdateError::Validation)?;
    let value = toml::Value::try_from(&settings).map_err(|_| UpdateError::Validation)?;
    root.insert("usage_reporting".into(), value);
    let updated = validated_configuration_table_with_secrets(path, &root, &secrets)
        .map_err(|_| UpdateError::Validation)?;
    let next_revision = table_revision(&root);
    let serialized = toml::to_string_pretty(&root).map_err(|_| UpdateError::Validation)?;
    writer(path, serialized.as_bytes()).map_err(|_| UpdateError::Persistence)?;
    Ok((updated, next_revision))
}
