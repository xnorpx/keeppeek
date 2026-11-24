//! Deployment settings for the optional HTTPS bug reporting service.
//!
//! Store the credential reference in configuration. Resolve it only when the
//! recorder needs to contact the service.

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub(crate) origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) credential: Option<String>,
    #[serde(default = "default_retention_days")]
    pub(crate) retention_days: u8,
}

const fn default_retention_days() -> u8 {
    7
}

pub(super) fn parse_settings(value: toml::Value) -> anyhow::Result<Settings> {
    let settings: Settings = value
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid reporting settings"))?;
    anyhow::ensure!(settings.origin.len() <= 512, "reporting origin is too long");
    let url = Url::parse(&settings.origin)
        .map_err(|_| anyhow::anyhow!("reporting requires a canonical HTTPS origin"))?;
    anyhow::ensure!(
        url.scheme() == "https" && url.origin().ascii_serialization() == settings.origin,
        "reporting requires a canonical HTTPS origin"
    );
    anyhow::ensure!(
        (1..=30).contains(&settings.retention_days),
        "reporting retention must be between 1 and 30 days"
    );
    if let Some(reference) = &settings.credential {
        let key = reference
            .strip_prefix("{secret:")
            .and_then(|value| value.strip_suffix('}'));
        anyhow::ensure!(
            key.is_some_and(|value| value.len() <= 128 && super::valid_secret_key(value)),
            "reporting requires a private credential reference"
        );
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::super::{
        Config, Secrets, config_from_table, load_config, load_configuration_table, update_settings,
        write_configuration_table,
    };
    use std::collections::BTreeMap;

    #[test]
    fn reporting_defaults_to_local_and_does_not_resolve_credentials_during_configuration() {
        assert!(Config::default().reporting.is_none());
        let source = toml::from_str(
            r#"
            [reporting]
            origin = "https://reports.example"
            credential = "{secret:REPORT_BEARER}"
            "#,
        )
        .unwrap();
        let secrets = Secrets(BTreeMap::from([(
            "REPORT_BEARER".to_owned(),
            "test-credential-canary".to_owned(),
        )]));
        for secrets in [Secrets::default(), secrets] {
            let config = config_from_table(&source, &secrets).unwrap();
            let serialized = toml::to_string(&config).unwrap();
            let reporting = config.reporting.unwrap();
            assert_eq!(reporting.retention_days, 7);
            assert_eq!(
                reporting.credential.as_deref(),
                Some("{secret:REPORT_BEARER}")
            );
            assert!(serialized.contains("{secret:REPORT_BEARER}"));
            assert!(!serialized.contains("test-credential-canary"));
            assert_eq!(config.source, source);
        }
    }

    #[test]
    fn invalid_reporting_configuration_rejects_private_values_without_echoing_them() {
        for entry in [
            "origin = \"http://reports.example\"",
            "origin = \"https://private-canary@reports.example\"",
            "origin = \"https://reports.example/private-canary\"",
            "origin = \"https://reports.example?private-canary\"",
            "origin = \"https://reports.example\"\ncredential = \"private-canary\"",
            "origin = \"https://reports.example\"\ncredential = \"{secret:EXAMPLE|url}\"",
            "origin = \"https://reports.example\"\nretention_days = 31",
            "origin = \"https://reports.example\"\nretention_days = 0",
        ] {
            let root = toml::from_str(&format!("[reporting]\n{entry}")).unwrap();
            let error = config_from_table(&root, &Secrets::default()).err().unwrap();
            assert!(!error.to_string().contains("private-canary"));
        }
    }

    #[test]
    fn reporting_settings_survive_unrelated_writes_and_validated_import() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-reporting-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let original = directory.join("config.toml");
        let imported = directory.join("imported.toml");
        let source = toml::from_str(
            r#"
            unrelated = "preserve"

            [reporting]
            origin = "https://reports.example:8443"
            credential = "{secret:REPORT_BEARER}"
            retention_days = 30

            "#,
        )
        .unwrap();
        write_configuration_table(&original, &source).unwrap();
        let mut configuration = load_config(&original).unwrap();
        configuration.port = 8084;
        update_settings(&original, &configuration).unwrap();
        let updated = load_configuration_table(&original).unwrap();
        assert_eq!(updated["reporting"], source["reporting"]);
        assert_eq!(updated["unrelated"], source["unrelated"]);
        write_configuration_table(&imported, &updated).unwrap();
        let loaded = load_config(&imported).unwrap().reporting.unwrap();
        assert_eq!(loaded.origin, "https://reports.example:8443");
        assert_eq!(loaded.retention_days, 30);
        assert_eq!(loaded.credential.as_deref(), Some("{secret:REPORT_BEARER}"));

        let mut invalid = updated;
        invalid["reporting"]
            .as_table_mut()
            .unwrap()
            .insert("retention_days".into(), 31.into());
        let serialized = std::fs::read(&imported).unwrap();
        assert!(write_configuration_table(&imported, &invalid).is_err());
        assert_eq!(std::fs::read(&imported).unwrap(), serialized);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
