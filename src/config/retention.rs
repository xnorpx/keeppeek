//! Validated retention settings in the existing application configuration.
//!
//! A resolved policy selects deadlines. It never grants permission to remove media.

pub mod evidence;

use crate::storage::{
    metadata::EventSource,
    retention::{EvidenceKind, RetentionMode, RetentionPolicy, RetentionRule, RuleClass, RuleId},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
};

const RULES_MAX: usize = 16;
const MAPPINGS_MAX: usize = 16;
const CAMERA_OVERRIDES_MAX: usize = 4096;

/// Global rollout gate, complete default rules, and sparse camera overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(try_from = "RawSettings")]
pub struct Settings {
    enabled: bool,
    rules: Vec<Rule>,
    event_mappings: Vec<EventMapping>,
    cameras: BTreeMap<String, CameraOverride>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawSettings {
    enabled: bool,
    rules: Vec<Rule>,
    event_mappings: Vec<EventMapping>,
    cameras: BTreeMap<String, CameraOverride>,
}

impl TryFrom<RawSettings> for Settings {
    type Error = anyhow::Error;

    fn try_from(raw: RawSettings) -> Result<Self, Self::Error> {
        let settings = Self {
            enabled: raw.enabled,
            rules: raw.rules,
            event_mappings: raw.event_mappings,
            cameras: raw.cameras,
        };
        settings.validate_structure()?;
        Ok(settings)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    id: RuleId,
    class: RuleClass,
    duration_ms: u64,
    mode: RetentionMode,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CameraOverride {
    enabled: Option<bool>,
    rules: BTreeMap<RuleId, RuleOverride>,
    event_mappings: Option<Vec<EventMapping>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RuleOverride {
    duration_ms: Option<u64>,
    mode: Option<RetentionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventMapping {
    source: EventSource,
    kind: String,
    evidence: EvidenceKind,
}

impl Settings {
    pub(super) fn validate(&self, configured: &HashSet<IpAddr>) -> anyhow::Result<()> {
        for key in self.cameras.keys() {
            let ip: IpAddr = key.parse()?;
            anyhow::ensure!(
                configured.contains(&ip),
                "retention camera must be configured"
            );
        }
        Ok(())
    }

    fn validate_structure(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.cameras.len() <= CAMERA_OVERRIDES_MAX,
            "too many retention camera overrides"
        );
        anyhow::ensure!(self.rules.len() <= RULES_MAX, "too many retention rules");
        let mut ids = HashSet::new();
        for rule in &self.rules {
            anyhow::ensure!(ids.insert(&rule.id), "duplicate retention rule ID");
            RetentionRule::new(rule.id.clone(), rule.class, rule.duration_ms, rule.mode)?;
        }
        validate_mappings(&self.event_mappings)?;
        for (key, camera) in &self.cameras {
            let ip: IpAddr = key
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid retention camera key"))?;
            anyhow::ensure!(
                ip.to_string() == *key,
                "retention camera key must be canonical"
            );
            anyhow::ensure!(
                camera.rules.len() <= RULES_MAX,
                "too many retention overrides"
            );
            for (id, update) in &camera.rules {
                anyhow::ensure!(
                    ids.contains(id),
                    "retention override refers to an unknown rule"
                );
                if let Some(duration) = update.duration_ms {
                    i64::try_from(duration)?;
                }
            }
            if let Some(mappings) = &camera.event_mappings {
                validate_mappings(mappings)?;
            }
        }
        Ok(())
    }

    /// Builds the configured policy without authorizing expiration or filesystem work.
    /// Global disable is a hard rollout bound; explicit camera zero durations override inheritance.
    ///
    /// # Errors
    /// Rejects rule counts or durations outside the resolver's bounds.
    pub fn policy_for(&self, camera: IpAddr) -> anyhow::Result<Option<RetentionPolicy>> {
        let override_ = self.cameras.get(&camera.to_string());
        if !self.enabled || override_.is_some_and(|value| value.enabled == Some(false)) {
            return Ok(None);
        }
        let rules = self
            .rules
            .iter()
            .map(|rule| {
                let update = override_.and_then(|value| value.rules.get(&rule.id));
                RetentionRule::new(
                    rule.id.clone(),
                    rule.class,
                    update
                        .and_then(|value| value.duration_ms)
                        .unwrap_or(rule.duration_ms),
                    update.and_then(|value| value.mode).unwrap_or(rule.mode),
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Some(RetentionPolicy::new(rules)?))
    }

    /// Matches an explicit event origin and exact kind; no detector payload is inferred.
    /// A match does not establish a closed interval or complete producer evidence.
    pub fn classify(
        &self,
        camera: IpAddr,
        source: EventSource,
        kind: &str,
    ) -> Option<EvidenceKind> {
        let mappings = self
            .cameras
            .get(&camera.to_string())
            .and_then(|value| value.event_mappings.as_deref())
            .unwrap_or(&self.event_mappings);
        mappings
            .iter()
            .find(|mapping| mapping.source == source && mapping.kind == kind)
            .map(|mapping| mapping.evidence)
    }
}

fn validate_mappings(mappings: &[EventMapping]) -> anyhow::Result<()> {
    anyhow::ensure!(
        mappings.len() <= MAPPINGS_MAX,
        "too many retention event mappings"
    );
    let mut seen = HashSet::new();
    for mapping in mappings {
        anyhow::ensure!(
            !mapping.kind.is_empty() && mapping.kind.len() <= 128,
            "retention event kind must contain 1..128 UTF-8 bytes"
        );
        anyhow::ensure!(
            seen.insert((mapping.source.as_str(), mapping.kind.as_str())),
            "duplicate retention event mapping"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_retention_candidate_preserves_configuration_bytes() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-retention-candidate-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        let original = "[cameras.front]\nip='192.0.2.8'\n";
        std::fs::write(&path, original).unwrap();
        let candidate: toml::Table = toml::from_str(&format!(
            "{original}[recording_retention.cameras.'192.0.2.9']\nenabled=false"
        ))
        .unwrap();
        assert!(crate::config::write_configuration_table(&path, &candidate).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
