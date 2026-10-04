//! Server-owned paths for the current metadata authority.
pub mod pending;

use super::StorageToml;
use crate::storage::{
    catalog::authority::Authority,
    volumes::{VolumeId, VolumeRole, VolumeState},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataBinding {
    pub volume_id: VolumeId,
    pub catalog_file: String,
    pub history_file: String,
    pub catalog_id: String,
    pub generation: u64,
    pub filesystem: String,
    pub root_identity: String,
}

impl MetadataBinding {
    pub(crate) fn root_identity(&self) -> crate::storage::volumes::root::Identity {
        crate::storage::volumes::root::Identity {
            filesystem: self.filesystem.clone(),
            directory: self.root_identity.clone(),
        }
    }
    pub(crate) fn authority(&self) -> Authority {
        Authority {
            catalog_id: self.catalog_id.clone(),
            generation: self.generation,
        }
    }

    pub(crate) fn paths(&self, storage: &StorageToml) -> anyhow::Result<(PathBuf, PathBuf)> {
        let catalog = leaf_id(&self.catalog_file, "catalog-", ".db")?;
        let history = leaf_id(&self.history_file, "exports-", ".json")?;
        anyhow::ensure!(
            catalog == history,
            "metadata files belong to different handoffs"
        );
        uuid::Uuid::parse_str(&self.catalog_id)?;
        anyhow::ensure!(
            [&self.filesystem, &self.root_identity]
                .iter()
                .all(|value| !value.is_empty()
                    && value.len() <= 256
                    && !value.chars().any(char::is_control)),
            "invalid metadata root identity"
        );
        anyhow::ensure!(
            self.generation > 0 && self.generation <= i64::MAX as u64,
            "invalid metadata generation"
        );
        anyhow::ensure!(
            storage.recording_catalog_path.is_none(),
            "managed metadata forbids a legacy catalog override"
        );
        let configuration = storage
            .named_volumes
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("metadata volume is not configured"))?;
        configuration.validate()?;
        let volume = configuration
            .volumes
            .iter()
            .find(|volume| volume.id == self.volume_id)
            .ok_or_else(|| anyhow::anyhow!("metadata volume is not configured"))?;
        anyhow::ensure!(
            volume.roles.contains(&VolumeRole::Metadata) && volume.state == VolumeState::Enabled,
            "metadata owner must be an enabled metadata volume"
        );
        Ok((
            volume.root.join(&self.catalog_file),
            volume.root.join(&self.history_file),
        ))
    }
}

fn leaf_id<'a>(name: &'a str, prefix: &str, suffix: &str) -> anyhow::Result<&'a str> {
    let id = name
        .strip_prefix(prefix)
        .and_then(|name| name.strip_suffix(suffix))
        .ok_or_else(|| anyhow::anyhow!("invalid metadata filename"))?;
    let parsed = uuid::Uuid::parse_str(id)?;
    anyhow::ensure!(
        parsed.hyphenated().to_string() == id,
        "metadata filename requires a canonical UUID"
    );
    Ok(id)
}

pub(super) fn validate(storage: &StorageToml) -> anyhow::Result<()> {
    if let Some(binding) = &storage.metadata {
        binding.paths(storage)?;
    }
    Ok(())
}

pub(super) fn preserve_owner(previous: &StorageToml, next: &StorageToml) -> anyhow::Result<()> {
    if let Some(binding) = &previous.metadata {
        anyhow::ensure!(
            next.metadata.as_ref() == Some(binding),
            "metadata ownership requires a confirmed handoff"
        );
        anyhow::ensure!(
            binding.paths(previous)? == binding.paths(next)?,
            "metadata owner root cannot change through settings"
        );
    } else {
        anyhow::ensure!(
            next.metadata.is_none(),
            "metadata ownership requires a confirmed handoff"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
