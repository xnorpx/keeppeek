use super::{
    CANDIDATES_MAX, PlacementRule, RULES_MAX, VOLUMES_MAX, Volume, VolumeConfiguration, VolumeRole,
};
use std::path::{Component, Path, PathBuf};

impl VolumeConfiguration {
    /// Checks bounded configuration without accessing or changing the filesystem.
    ///
    /// # Errors
    /// Rejects invalid limits, ambiguous rules, unknown references, and lexical root overlap.
    /// Filesystem aliases and mounted identities require a separate runtime check.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.volumes.len() <= VOLUMES_MAX,
            "at most 32 storage volumes are allowed"
        );
        anyhow::ensure!(
            self.placement.len() <= RULES_MAX,
            "at most 256 placement rules are allowed"
        );
        let mut roots = Vec::with_capacity(self.volumes.len());
        // ponytail: At most 32 roots need pairwise comparison; use an index if this bound grows.
        for (index, volume) in self.volumes.iter().enumerate() {
            validate_volume(volume)?;
            anyhow::ensure!(
                self.volumes[..index]
                    .iter()
                    .all(|other| other.id != volume.id),
                "duplicate volume ID"
            );
            let root = comparison_root(&volume.root)?;
            anyhow::ensure!(
                roots
                    .iter()
                    .all(|other: &PathBuf| !root.starts_with(other) && !other.starts_with(&root)),
                "storage volume roots must not overlap"
            );
            roots.push(root);
        }
        for (index, rule) in self.placement.iter().enumerate() {
            self.validate_rule(rule)?;
            anyhow::ensure!(
                self.placement[..index]
                    .iter()
                    .all(|other| other.role != rule.role
                        || other.source != rule.source
                        || other.group != rule.group),
                "duplicate placement selector for a storage role"
            );
        }
        Ok(())
    }

    fn validate_rule(&self, rule: &PlacementRule) -> anyhow::Result<()> {
        anyhow::ensure!(
            !rule.candidates.is_empty() && rule.candidates.len() <= CANDIDATES_MAX,
            "placement requires 1 to 8 candidates"
        );
        anyhow::ensure!(
            rule.source.is_none() || rule.group.is_none(),
            "placement selects either a source or a group"
        );
        for value in [rule.source.as_deref(), rule.group.as_deref()]
            .into_iter()
            .flatten()
        {
            validate_selector(value)?;
        }
        if rule.role == VolumeRole::Metadata {
            anyhow::ensure!(
                rule.source.is_none()
                    && rule.group.is_none()
                    && rule.candidates.len() == 1
                    && !rule.allow_fallback,
                "metadata placement requires one global destination without fallback"
            );
        }
        for (index, id) in rule.candidates.iter().enumerate() {
            anyhow::ensure!(
                !rule.candidates[..index].contains(id),
                "duplicate placement candidate"
            );
            let volume = self
                .volumes
                .iter()
                .find(|volume| volume.id == *id)
                .ok_or_else(|| anyhow::anyhow!("placement references an unknown volume"))?;
            anyhow::ensure!(
                volume.roles.contains(&rule.role),
                "placement candidate does not support the requested role"
            );
        }
        Ok(())
    }
}

fn validate_volume(volume: &Volume) -> anyhow::Result<()> {
    anyhow::ensure!(
        !volume.roles.is_empty() && volume.roles.len() <= 5,
        "a volume requires 1 to 5 distinct roles"
    );
    for (index, role) in volume.roles.iter().enumerate() {
        anyhow::ensure!(
            !volume.roles[..index].contains(role),
            "duplicate storage volume role"
        );
    }
    anyhow::ensure!(
        volume.capacity_bytes != Some(0),
        "a storage volume capacity must be nonzero or omitted"
    );
    for value in [
        volume.capacity_bytes.unwrap_or(0),
        volume.minimum_free_bytes,
        volume.warning_free_bytes,
        volume.critical_free_bytes,
    ] {
        anyhow::ensure!(
            i64::try_from(value).is_ok(),
            "volume byte threshold exceeds the configuration integer limit"
        );
    }
    anyhow::ensure!(
        volume.warning_free_bytes >= volume.critical_free_bytes.max(volume.minimum_free_bytes),
        "volume warning threshold must cover critical and minimum free space"
    );
    validate_selectors(&volume.sources)?;
    validate_selectors(&volume.groups)?;
    if volume.roles.contains(&VolumeRole::Metadata) {
        anyhow::ensure!(
            volume.sources.is_empty() && volume.groups.is_empty(),
            "metadata volume cannot restrict sources or groups"
        );
    }
    Ok(())
}

fn validate_selectors(values: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        values.len() <= RULES_MAX,
        "at most 256 source or group selectors are allowed"
    );
    for (index, value) in values.iter().enumerate() {
        validate_selector(value)?;
        anyhow::ensure!(
            !values[..index].contains(value),
            "duplicate source or group selector"
        );
    }
    Ok(())
}

fn validate_selector(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control),
        "source and group selectors require 1 to 256 bytes without control or surrounding whitespace"
    );
    Ok(())
}

pub(in crate::storage) fn comparison_root(root: &Path) -> anyhow::Result<PathBuf> {
    let text = root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("volume root must be valid UTF-8"))?;
    anyhow::ensure!(
        root.is_absolute() && text.len() <= 4096 && !text.chars().any(char::is_control),
        "volume root must be an absolute path of at most 4096 bytes without control characters"
    );
    anyhow::ensure!(
        !root
            .components()
            .any(|part| matches!(part, Component::ParentDir)),
        "volume root cannot contain parent traversal"
    );
    anyhow::ensure!(
        root.components().count() <= 64,
        "volume root has too many components"
    );
    #[cfg(windows)]
    {
        anyhow::ensure!(
            matches!(root.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_))),
            "volume roots require a local drive path"
        );
        for component in root.components() {
            if let Component::Normal(name) = component {
                let name = name
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid volume root component"))?;
                anyhow::ensure!(
                    !name.ends_with(['.', ' '])
                        && !name.contains([':', '*', '?', '<', '>', '|', '"'])
                        && !windows_device_name(name),
                    "volume roots cannot contain device names, reserved characters or ambiguous components"
                );
            }
        }
        Ok(PathBuf::from(text.to_ascii_lowercase()))
    }
    #[cfg(not(windows))]
    Ok(root.to_path_buf())
}

#[cfg(windows)]
fn windows_device_name(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let suffix = base
        .strip_prefix("COM")
        .or_else(|| base.strip_prefix("LPT"));
    suffix.is_some_and(|suffix| {
        matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    })
}
