//! Operator retention defaults and camera overrides stored in the application configuration.

use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{MAX_RULES, Policy, Predicate, Rule};

/// Independent lifetimes in fractional days. Omitted camera fields inherit defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lifetimes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuous_days: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_days: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub events: BTreeMap<String, f64>,
}

/// Optional global defaults and at most 127 overrides keyed by canonical source identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Lifetimes>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cameras: BTreeMap<String, Lifetimes>,
}

impl Settings {
    /// Validates all effective policies, including inherited event rules and explicit zeros.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.cameras.len() <= 127,
            "retention supports at most 127 camera overrides"
        );
        ensure!(
            self.default.is_some() || !self.cameras.is_empty(),
            "retention requires a policy"
        );
        if let Some(default) = &self.default {
            ensure!(
                default.has_rules(),
                "retention defaults require at least one rule"
            );
            self.policy_for_default()?;
        }
        for camera in self.cameras.keys() {
            ensure!(
                !camera.trim().is_empty() && camera.len() <= 256 && !camera.contains('\0'),
                "retention camera keys require 1 to 256 bytes without NUL"
            );
            self.policy_for(camera)?;
        }
        Ok(())
    }

    /// Returns no policy for an unmentioned camera when no defaults exist.
    pub fn policy_for(&self, camera: &str) -> Result<Option<Policy>> {
        match (self.default.as_ref(), self.cameras.get(camera)) {
            (None, None) => Ok(None),
            (default, camera) => {
                let mut effective = default.cloned().unwrap_or_default();
                if let Some(camera) = camera {
                    effective.continuous_days =
                        camera.continuous_days.or(effective.continuous_days);
                    effective.motion_days = camera.motion_days.or(effective.motion_days);
                    effective.events.extend(camera.events.clone());
                }
                ensure!(
                    effective.has_rules(),
                    "retention camera policy requires at least one rule"
                );
                Ok(Some(effective.policy()?))
            }
        }
    }

    fn policy_for_default(&self) -> Result<()> {
        if let Some(default) = &self.default {
            default.policy()?;
        }
        Ok(())
    }
}

impl Lifetimes {
    fn has_rules(&self) -> bool {
        self.continuous_days.is_some() || self.motion_days.is_some() || !self.events.is_empty()
    }

    fn policy(&self) -> Result<Policy> {
        let count = usize::from(self.continuous_days.is_some())
            + usize::from(self.motion_days.is_some())
            + self.events.len();
        ensure!(
            count <= MAX_RULES,
            "retention effective rule limit exceeded"
        );
        let mut rules = Vec::with_capacity(count);
        for (id, days, predicate) in [
            ("continuous", self.continuous_days, Predicate::Continuous),
            ("motion", self.motion_days, Predicate::Motion),
        ] {
            if let Some(days) = days {
                rules.push(Rule::new(id, milliseconds(days)?, predicate)?);
            }
        }
        for (event_type, days) in &self.events {
            super::validate_selector(event_type)?;
            // Stable IDs avoid collisions with class rules and remain below the selector ceiling.
            let digest: String = Sha256::digest(event_type.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let id = format!("event_{digest}");
            rules.push(Rule::new(
                id,
                milliseconds(*days)?,
                Predicate::Event {
                    event_type: event_type.as_str().into(),
                },
            )?);
        }
        Policy::new(rules)
    }
}

fn milliseconds(days: f64) -> Result<u64> {
    ensure!(
        days.is_finite() && days >= 0.0,
        "retention days must be finite and nonnegative"
    );
    let value = days * 86_400_000.0;
    ensure!(
        value.is_finite() && value < i64::MAX as f64,
        "retention duration exceeds the UTC range"
    );
    let rounded = value.round();
    let tolerance = (value.abs() * f64::EPSILON).max(0.000_001);
    ensure!(
        (value - rounded).abs() <= tolerance,
        "retention days must resolve to whole milliseconds"
    );
    Ok(rounded as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_settings_inherit_omitted_rules_and_preserve_explicit_zero() {
        let settings: Settings = toml::from_str(
            r#"
            [default]
            continuous_days = 1.5
            motion_days = 2.0
            [default.events]
            person = 3.0
            [cameras.front]
            continuous_days = 0.0
            [cameras.front.events]
            person = 0.0
            package = 0.5
        "#,
        )
        .unwrap();
        settings.validate().unwrap();
        let front = serde_json::to_value(settings.policy_for("front").unwrap().unwrap()).unwrap();
        let rules = front["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0]["duration_ms"], 0);
        assert_eq!(rules[1]["duration_ms"], 172_800_000);
        assert!(rules.iter().any(|rule| rule["predicate"]["event_type"] == "person" && rule["duration_ms"] == 0));
        assert!(
            rules
                .iter()
                .any(|rule| rule["predicate"]["event_type"] == "package"
                    && rule["duration_ms"] == 43_200_000)
        );
        let inherited =
            serde_json::to_value(settings.policy_for("back").unwrap().unwrap()).unwrap();
        assert_eq!(inherited["rules"][0]["duration_ms"], 129_600_000);
        let round_trip: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            round_trip.policy_for("front").unwrap(),
            settings.policy_for("front").unwrap()
        );
    }

    #[test]
    fn retention_settings_reject_invalid_durations_selectors_and_rule_overflow() {
        for source in [
            "[default]\ncontinuous_days = -1.0",
            "[default]\ncontinuous_days = nan",
            "[default]\ncontinuous_days = inf",
            "[default]\ncontinuous_days = 1000000000000.0",
            "[default]\ncontinuous_days = 0.000000001",
            "[default.events]\n'person/invalid' = 1.0",
            "[default]\ncontinous_days = 1.0",
            "[default]",
        ] {
            assert!(
                toml::from_str::<Settings>(source)
                    .and_then(|settings| settings.validate().map_err(serde::de::Error::custom))
                    .is_err(),
                "{source}"
            );
        }
        let mut source = String::from("[default.events]\n");
        for index in 0..17 {
            source.push_str(&format!("event{index} = 1.0\n"));
        }
        assert!(
            toml::from_str::<Settings>(&source)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn camera_only_retention_does_not_activate_unmentioned_cameras() {
        let settings: Settings = toml::from_str("[cameras.front]\nmotion_days = 0.0").unwrap();
        settings.validate().unwrap();
        assert!(settings.policy_for("front").unwrap().is_some());
        assert!(settings.policy_for("back").unwrap().is_none());
    }
}
