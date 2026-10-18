//! Bounded local archive settings stored in the application configuration.
//!
//! A configured root is an operator-selected destination, not a verified filesystem binding.
//! The archive catalog owns the binding after an explicit probe. Configuration validation does
//! not create the root, inspect its contents, or authorize source eviction.

use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::storage::volumes::VolumeId;

const SOURCES_MAX: usize = 127;
const SOURCE_BYTES_MAX: usize = 256;
// These ceilings bound automatic selection and work retained after a destination outage.
const AGE_SECS_MAX: u64 = 10 * 365 * 24 * 60 * 60;
const PENDING_COUNT_MAX: u32 = 4096;
const TIMEOUT_SECS_MAX: u64 = 3600;
const ATTEMPTS_MAX: u32 = 10;
// Four-MiB chunk hashes for 64 GiB require at most 512 KiB of integrity metadata.
const OBJECT_BYTES_MAX: u64 = 64 * 1024 * 1024 * 1024;

/// Operator intent for one local archive destination and its bounded transfer policy.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub target_id: String,
    pub root: PathBuf,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub minimum_age_secs: u64,
    #[serde(default)]
    pub evict_after_archive: bool,
    #[serde(default = "default_maximum_object_bytes")]
    pub maximum_object_bytes: u64,
    #[serde(default = "default_maximum_pending_count")]
    pub maximum_pending_count: u32,
    #[serde(default = "default_maximum_pending_bytes")]
    pub maximum_pending_bytes: u64,
    #[serde(default = "default_operation_timeout_secs")]
    pub operation_timeout_secs: u64,
    #[serde(default = "default_maximum_attempts")]
    pub maximum_attempts: u32,
}

impl Settings {
    /// Validates resolved settings without accessing the archive destination.
    ///
    /// # Errors
    /// Rejects invalid identity, nonabsolute roots, duplicate selectors, and unsafe work limits.
    pub fn validate(&self) -> Result<()> {
        VolumeId::parse(&self.target_id)?;
        super::super::volumes::validation::comparison_root(&self.root)?;
        self.validate_sources()?;
        ensure!(
            self.minimum_age_secs <= AGE_SECS_MAX,
            "archive minimum age exceeds ten years"
        );
        ensure!(
            self.maximum_object_bytes > 0
                && self.maximum_object_bytes <= OBJECT_BYTES_MAX
                && self.maximum_object_bytes <= self.maximum_pending_bytes
                && i64::try_from(self.maximum_pending_bytes).is_ok(),
            "archive object limit must be 1..64 GiB, not exceed the pending limit, and fit signed-64-bit pending bytes"
        );
        ensure!(
            (1..=PENDING_COUNT_MAX).contains(&self.maximum_pending_count),
            "archive pending count must be between 1 and 4096"
        );
        ensure!(
            (1..=TIMEOUT_SECS_MAX).contains(&self.operation_timeout_secs),
            "archive operation timeout must be between 1 and 3600 seconds"
        );
        ensure!(
            (1..=ATTEMPTS_MAX).contains(&self.maximum_attempts),
            "archive attempts must be between 1 and 10"
        );
        Ok(())
    }

    pub(crate) fn validate_storage(&self, storage: &crate::config::StorageToml) -> Result<()> {
        self.validate()?;
        if let Some(configuration) = &storage.named_volumes {
            for volume in &configuration.volumes {
                self.require_separate_root(&volume.root)?;
            }
        }
        let effective = crate::storage::StorageConfig::from_toml(storage);
        for root in [
            &effective.medium_term_path,
            &effective.long_term_path,
            &effective.event_thumbnail_path,
        ] {
            self.require_separate_root(root)?;
        }
        self.require_separate_root(
            effective
                .recording_catalog_path
                .parent()
                .unwrap_or(Path::new(".")),
        )?;
        for root in [
            &storage.medium_term_path,
            &storage.long_term_path,
            &storage.event_thumbnail_path,
        ]
        .into_iter()
        .flatten()
        {
            self.require_separate_root(Path::new(root))?;
        }
        if let Some(catalog) = &storage.recording_catalog_path {
            self.require_separate_root(Path::new(catalog).parent().unwrap_or(Path::new(".")))?;
        }
        Ok(())
    }

    fn validate_sources(&self) -> Result<()> {
        ensure!(
            self.source_ids.len() <= SOURCES_MAX,
            "archive supports at most 127 source selectors"
        );
        let mut seen = BTreeSet::new();
        for source in &self.source_ids {
            ensure!(
                !source.is_empty()
                    && source.len() <= SOURCE_BYTES_MAX
                    && source.trim() == source
                    && !source.chars().any(char::is_control)
                    && seen.insert(source),
                "archive source selectors must be distinct nonblank values of at most 256 bytes"
            );
        }
        Ok(())
    }

    fn require_separate_root(&self, other: &Path) -> Result<()> {
        let archive = super::super::volumes::validation::comparison_root(&self.root)?;
        let other =
            super::super::volumes::validation::comparison_root(&std::path::absolute(other)?)?;
        ensure!(
            !archive.starts_with(&other) && !other.starts_with(&archive),
            "archive root must not overlap existing media, thumbnail, metadata or named-volume roots"
        );
        Ok(())
    }
}

impl fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Settings")
            .field("target_id", &"[REDACTED]")
            .field("root", &"[REDACTED]")
            .field("enabled", &self.enabled)
            .field("automatic", &self.automatic)
            .field("source_count", &self.source_ids.len())
            .field("evict_after_archive", &self.evict_after_archive)
            .finish_non_exhaustive()
    }
}

// Four-GiB objects and a 64-GiB backlog bound one local destination without buffering whole files.
const fn default_maximum_object_bytes() -> u64 {
    4 * 1024 * 1024 * 1024
}
const fn default_maximum_pending_bytes() -> u64 {
    64 * 1024 * 1024 * 1024
}
// One bounded page of pending work prevents automatic selection from retaining a fleet history.
const fn default_maximum_pending_count() -> u32 {
    256
}
// Local operations have a finite failure window; retry consumes the same durable job identity.
const fn default_operation_timeout_secs() -> u64 {
    120
}
const fn default_maximum_attempts() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Settings {
        toml::Table::from_iter([
            ("target_id".into(), "private-archive-target".into()),
            (
                "root".into(),
                std::env::temp_dir()
                    .join("private-archive-root")
                    .to_string_lossy()
                    .as_ref()
                    .into(),
            ),
        ])
        .try_into()
        .unwrap()
    }

    #[test]
    fn archive_defaults_require_explicit_transfer_and_eviction_opt_in() {
        let settings = fixture();
        settings.validate().unwrap();
        assert!(!settings.enabled);
        assert!(!settings.automatic);
        assert!(!settings.evict_after_archive);
        assert!(settings.maximum_object_bytes <= settings.maximum_pending_bytes);
    }

    #[test]
    fn archive_settings_reject_unbounded_work_and_accept_object_ceiling() {
        let original = fixture();
        let mut maximum = original.clone();
        maximum.maximum_object_bytes = 64 * 1024 * 1024 * 1024;
        maximum.validate().unwrap();
        for (object, pending, count, timeout, attempts) in [
            (0, original.maximum_pending_bytes, 1, 1, 1),
            (
                64 * 1024 * 1024 * 1024 + 1,
                original.maximum_pending_bytes + 1,
                1,
                1,
                1,
            ),
            (2, 1, 1, 1, 1),
            (1, u64::MAX, 1, 1, 1),
            (1, 1, 0, 1, 1),
            (1, 1, 4097, 1, 1),
            (1, 1, 1, 0, 1),
            (1, 1, 1, 3601, 1),
            (1, 1, 1, 1, 0),
            (1, 1, 1, 1, 11),
        ] {
            let settings = Settings {
                maximum_object_bytes: object,
                maximum_pending_bytes: pending,
                maximum_pending_count: count,
                operation_timeout_secs: timeout,
                maximum_attempts: attempts,
                ..original.clone()
            };
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn archive_settings_reject_ambiguous_sources_and_unknown_provider_fields() {
        let mut settings = fixture();
        for sources in [
            vec!["camera".into(), "camera".into()],
            vec![" camera".into()],
            vec!["camera\n".into()],
            (0..128).map(|i| format!("camera-{i}")).collect(),
        ] {
            settings.source_ids = sources;
            assert!(settings.validate().is_err());
        }
        let mut raw: toml::Table = toml::Value::try_from(fixture())
            .unwrap()
            .try_into()
            .unwrap();
        raw.insert("provider".into(), "s3".into());
        assert!(raw.try_into::<Settings>().is_err());
    }

    #[test]
    fn archive_settings_debug_does_not_disclose_paths_or_sources() {
        let mut settings = fixture();
        settings.source_ids = vec!["private-source-identity".into()];
        let debug = format!("{settings:?}");
        assert!(!debug.contains("private-archive-root"));
        assert!(!debug.contains("private-archive-target"));
        assert!(!debug.contains("private-source-identity"));
    }
}
