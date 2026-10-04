//! Stages metadata changes in the existing configuration without moving live files.

use super::*;
use crate::config::{Config, load_configuration_table, load_secrets, write_configuration_table};

pub struct View {
    pub root: toml::Table,
    pub config: Config,
    pub pending: Option<Pending>,
}

impl View {
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        let root = load_configuration_table(path)?;
        let secrets = load_secrets(path)?;
        let config = crate::config::config_from_table(&root, &secrets)?;
        let mut raw = root
            .get("storage")
            .and_then(|value| value.get(PENDING))
            .cloned();
        if let Some(value) = &mut raw {
            crate::config::resolve_toml_secret_references(value, &secrets)?;
        }
        let pending = raw.map(toml::Value::try_into).transpose()?;
        Ok(Self {
            root,
            config,
            pending,
        })
    }

    pub(crate) fn stage(&self, path: &Path, target: &MetadataBinding) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.pending.is_none(),
            "a metadata handoff is already pending"
        );
        anyhow::ensure!(
            !self
                .root
                .contains_key(crate::config::STORAGE_MIGRATION_SECTION),
            "metadata handoff conflicts with legacy storage migration"
        );
        let pending = Pending {
            source_digest: fingerprint(&self.config.storage)?,
            target: target.clone(),
        };
        let mut raw = toml::Value::try_from(&pending)?;
        raw["target"]["volume_id"] = self.raw_volume_id(&target.volume_id)?.clone();
        let candidate = candidate(&self.root, &raw, &self.config.storage, &pending)?;
        crate::config::config_from_table(&candidate, &load_secrets(path)?)?;
        let mut next = self.root.clone();
        next.get_mut("storage")
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| anyhow::anyhow!("storage configuration is unavailable"))?
            .insert(PENDING.into(), raw);
        self.write(path, &next)
    }

    pub(crate) fn cancel(&self, path: &Path) -> anyhow::Result<()> {
        let mut next = self.root.clone();
        if let Some(storage) = next.get_mut("storage").and_then(toml::Value::as_table_mut) {
            storage.remove(PENDING);
        }
        self.write(path, &next)
    }

    fn raw_volume_id(
        &self,
        id: &crate::storage::volumes::VolumeId,
    ) -> anyhow::Result<&toml::Value> {
        let index = self
            .config
            .storage
            .named_volumes
            .as_ref()
            .and_then(|configuration| {
                configuration
                    .volumes
                    .iter()
                    .position(|volume| &volume.id == id)
            })
            .ok_or_else(|| anyhow::anyhow!("metadata volume is not configured"))?;
        self.root
            .get("storage")
            .and_then(|value| value.get("named_volumes"))
            .and_then(|value| value.get("volumes"))
            .and_then(toml::Value::as_array)
            .and_then(|volumes| volumes.get(index))
            .and_then(|volume| volume.get("id"))
            .ok_or_else(|| anyhow::anyhow!("metadata volume identifier is unavailable"))
    }

    fn write(&self, path: &Path, next: &toml::Table) -> anyhow::Result<()> {
        anyhow::ensure!(
            load_configuration_table(path)? == self.root,
            "configuration changed during metadata confirmation"
        );
        write_configuration_table(path, next)
    }
}
