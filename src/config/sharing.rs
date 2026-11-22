//! Deployment policy for isolated guest review. Grants and sessions are not settings.
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SharingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_origin: Option<String>,
    #[serde(default = "initial_revision")]
    pub revision: u64,
}

const fn initial_revision() -> u64 {
    1
}

impl Default for SharingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            public_origin: None,
            revision: initial_revision(),
        }
    }
}

impl SharingConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.revision > 0 && self.revision <= i64::MAX as u64,
            "sharing revision must fit a positive TOML integer"
        );
        if let Some(origin) = &self.public_origin {
            validate_origin(origin)?;
        }
        anyhow::ensure!(
            !self.enabled || self.public_origin.is_some(),
            "enabled sharing requires a public HTTPS origin"
        );
        Ok(())
    }
}

fn validate_origin(origin: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !origin.is_empty() && origin.len() <= 512 && !origin.chars().any(char::is_control),
        "sharing origin must contain 1 to 512 bytes"
    );
    let parsed = Url::parse(origin).map_err(|_| anyhow::anyhow!("invalid sharing origin"))?;
    anyhow::ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.origin().ascii_serialization() == *origin,
        "sharing origin must be a canonical HTTPS origin without a path or credentials"
    );
    Ok(())
}

/// Distinguishes rejected candidates from writes with an indeterminate commit result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError {
    Validation,
    Persistence,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Validation => "sharing settings could not be validated",
            Self::Persistence => "sharing settings persistence may be incomplete",
        })
    }
}

impl std::error::Error for UpdateError {}

/// The caller holds the configuration update lock through persistence and runtime activation.
pub fn update(
    path: &Path,
    expected_revision: u64,
    settings: SharingConfig,
) -> Result<Config, UpdateError> {
    update_with_writer(
        path,
        expected_revision,
        settings,
        write_private_file_atomically,
    )
}

/// Validates the complete candidate before a writer can change the configuration file.
pub fn update_with_writer(
    path: &Path,
    expected_revision: u64,
    settings: SharingConfig,
    writer: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
) -> Result<Config, UpdateError> {
    let (updated, serialized) =
        prepare(path, expected_revision, settings).map_err(|_| UpdateError::Validation)?;
    writer(path, serialized.as_bytes()).map_err(|_| UpdateError::Persistence)?;
    Ok(updated)
}

fn prepare(
    path: &Path,
    expected_revision: u64,
    mut settings: SharingConfig,
) -> anyhow::Result<(Config, String)> {
    let text = std::fs::read_to_string(path)?;
    let mut root: toml::Table = toml::from_str(&text)?;
    let secrets = load_secrets(path)?;
    let current = config_from_table(&root, &secrets)?;
    anyhow::ensure!(
        current.sharing.revision == expected_revision,
        "sharing revision conflicts"
    );
    settings.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("sharing revision is exhausted"))?;
    settings.validate()?;
    let mut next = toml::Value::try_from(&settings)?;
    if let Some(origin) = next.get_mut("public_origin") {
        *origin = preserve_secret_reference(
            root.get("sharing")
                .and_then(|section| section.get("public_origin")),
            origin.clone(),
            &secrets,
        )?;
    }
    root.insert("sharing".into(), next);
    let updated = validated_configuration_table_with_secrets(path, &root, &secrets)?;
    let serialized = toml::to_string_pretty(&root)?;
    Ok((updated, serialized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default_and_missing_origin_cannot_enable_sharing() {
        let defaults = Config::default();
        assert!(!defaults.sharing.enabled);
        assert!(defaults.sharing.public_origin.is_none());
        assert!(
            SharingConfig {
                enabled: true,
                ..SharingConfig::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn only_canonical_https_origins_without_credentials_or_paths_are_accepted() {
        for origin in [
            "https://review.example",
            "https://review.example:8443",
            "https://[::1]:8443",
        ] {
            assert!(validate_origin(origin).is_ok(), "{origin}");
        }
        for origin in [
            "http://review.example",
            "https://user:password@review.example",
            "https://review.example/path",
            "https://review.example?token=secret",
            "https://review.example#secret",
            "file:///tmp/review",
            "https://review.example/",
            "https://review.example:443",
            "https://REVIEW.EXAMPLE",
            "https://review.example\n",
            "null",
            "",
        ] {
            assert!(
                validate_origin(origin).is_err(),
                "unexpected origin acceptance"
            );
        }
        assert!(validate_origin(&format!("https://{}.example", "a".repeat(512))).is_err());
    }

    #[test]
    fn sharing_round_trip_preserves_private_references_and_other_owners() {
        let directory = directory();
        let path = directory.0.join("config.toml");
        write_private_file(
            &path,
            b"host = \"{secret:BIND_HOST}\"\ncustom_keep = \"untouched\"\n",
        )
        .unwrap();
        write_private_file(&secrets_path(&path), b"BIND_HOST = \"127.0.0.1\"\n").unwrap();
        let settings = SharingConfig {
            enabled: true,
            public_origin: Some("https://review.example".into()),
            revision: 99,
        };
        let updated = update(&path, 1, settings).unwrap();
        assert_eq!(updated.sharing.revision, 2);
        assert!(updated.sharing.enabled);
        let reloaded = load_config(&path).unwrap();
        assert_eq!(reloaded.sharing, updated.sharing);
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("{secret:BIND_HOST}"));
        assert!(!saved.contains("127.0.0.1"));
        assert_eq!(reloaded.source["custom_keep"].as_str(), Some("untouched"));
        assert!(!saved.contains("grant"));
        let before = std::fs::read(&path).unwrap();
        assert!(update(&path, 1, SharingConfig::default()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn invalid_sharing_change_leaves_configuration_and_secrets_unchanged() {
        let directory = directory();
        let path = directory.0.join("config.toml");
        write_private_file(&path, b"host = \"127.0.0.1\"\n").unwrap();
        write_private_file(&secrets_path(&path), b"PASSWORD = \"private-fixture\"\n").unwrap();
        let before = std::fs::read(&path).unwrap();
        let private_before = std::fs::read(secrets_path(&path)).unwrap();
        let settings = SharingConfig {
            enabled: true,
            public_origin: Some("http://review.example".into()),
            revision: 1,
        };
        assert!(update(&path, 1, settings).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(secrets_path(&path)).unwrap(), private_before);
    }

    #[test]
    fn saving_an_unchanged_origin_preserves_its_secret_reference() {
        let directory = directory();
        let path = directory.0.join("config.toml");
        write_private_file(
            &path,
            b"[sharing]\nenabled = true\npublic_origin = \"{secret:SHARING_TEST_ORIGIN}\"\n",
        )
        .unwrap();
        write_private_file(
            &secrets_path(&path),
            b"SHARING_TEST_ORIGIN = \"https://review.example\"\n",
        )
        .unwrap();
        let private_before = std::fs::read(secrets_path(&path)).unwrap();
        let mut settings = load_config(&path).unwrap().sharing;
        settings.enabled = false;
        let updated = update(&path, settings.revision, settings).unwrap();
        assert!(!updated.sharing.enabled);
        assert_eq!(updated.sharing.revision, 2);
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("{secret:SHARING_TEST_ORIGIN}"));
        assert!(!saved.contains("https://review.example"));
        assert_eq!(std::fs::read(secrets_path(&path)).unwrap(), private_before);
        assert_eq!(load_config(&path).unwrap().sharing, updated.sharing);
    }

    #[test]
    fn candidate_validation_and_revision_conflicts_never_start_the_writer() {
        let directory = directory();
        let path = directory.0.join("config.toml");
        write_private_file(&path, b"host = \"127.0.0.1\"\n").unwrap();
        write_private_file(&secrets_path(&path), b"PASSWORD = \"private-fixture\"\n").unwrap();
        let before = std::fs::read(&path).unwrap();
        let private_before = std::fs::read(secrets_path(&path)).unwrap();
        for (expected_revision, settings) in [
            (
                1,
                SharingConfig {
                    enabled: true,
                    public_origin: Some("http://review.example".into()),
                    revision: 1,
                },
            ),
            (2, SharingConfig::default()),
        ] {
            let called = std::cell::Cell::new(false);
            let result = update_with_writer(&path, expected_revision, settings, |_, _| {
                called.set(true);
                Ok(())
            });
            assert!(matches!(result, Err(UpdateError::Validation)));
            assert!(!called.get());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(std::fs::read(secrets_path(&path)).unwrap(), private_before);
        }
    }

    #[test]
    fn a_writer_error_after_commit_is_classified_without_disclosing_its_payload() {
        let directory = directory();
        let path = directory.0.join("config.toml");
        write_private_file(&path, b"host = \"127.0.0.1\"\n").unwrap();
        write_private_file(&secrets_path(&path), b"PASSWORD = \"private-fixture\"\n").unwrap();
        let private_before = std::fs::read(secrets_path(&path)).unwrap();
        let settings = SharingConfig {
            enabled: true,
            public_origin: Some("https://review.example".into()),
            revision: 1,
        };
        let result = update_with_writer(&path, 1, settings, |path, bytes| {
            write_private_file_atomically(path, bytes)?;
            Err(std::io::Error::other("synthetic-private-writer-payload"))
        });
        let error = result
            .err()
            .expect("injected writer must report its failure");
        assert_eq!(error, UpdateError::Persistence);
        assert!(
            !error
                .to_string()
                .contains("synthetic-private-writer-payload")
        );
        assert!(!format!("{error:?}").contains("synthetic-private-writer-payload"));
        let reloaded = load_config(&path).unwrap();
        assert!(reloaded.sharing.enabled);
        assert_eq!(reloaded.sharing.revision, 2);
        assert_eq!(std::fs::read(secrets_path(&path)).unwrap(), private_before);
    }

    struct Directory(PathBuf);

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn directory() -> Directory {
        Directory(
            std::env::temp_dir().join(format!("keeppeek-sharing-config-{}", uuid::Uuid::new_v4())),
        )
    }
    #[test]
    fn invalid_sharing_tables_fail_the_real_loader() {
        let directory = directory();
        let path = directory.0.join("config.toml");
        write_private_file(&secrets_path(&path), b"PASSWORD = \"private-fixture\"\n").unwrap();
        for table in [
            "enabled = true",
            "revision = 0",
            "enabled = false\nunknown = true",
            "enabled = false\npublic_origin = \"http://review.example\"",
        ] {
            write_private_file_atomically(&path, format!("[sharing]\n{table}\n").as_bytes())
                .unwrap();
            assert!(
                load_config(&path).is_err(),
                "invalid sharing table must fail closed"
            );
        }
    }
}
