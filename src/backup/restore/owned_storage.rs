//! Preserves named ownership when restoring configuration and secrets.

use crate::config;
use std::path::Path;

pub(super) fn preserve(source: &mut toml::Table, target: &toml::Table) -> anyhow::Result<()> {
    let Some(storage) = target.get("storage").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    if !storage.contains_key("named_volumes") {
        return Ok(());
    }
    anyhow::ensure!(
        !storage.contains_key(config::metadata::pending::PENDING),
        "finish or cancel the pending metadata handoff before restoring configuration"
    );
    let next = source
        .entry("storage")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("restored storage configuration is not a table"))?;
    next.remove(config::metadata::pending::PENDING);
    // ponytail: preserve complete ownership tables instead of rebuilding individual fields.
    for field in ["named_volumes", "metadata"] {
        next.remove(field);
        if let Some(value) = storage.get(field) {
            next.insert(field.into(), value.clone());
        }
    }
    Ok(())
}

pub(super) fn verify(target_path: &Path, candidate: &config::Config) -> anyhow::Result<()> {
    let target = config::load_config(target_path)?;
    if target.storage.named_volumes.is_none() {
        return Ok(());
    }
    anyhow::ensure!(
        candidate.storage.named_volumes == target.storage.named_volumes
            && candidate.storage.metadata == target.storage.metadata,
        "restore secrets must preserve the target named storage owners and metadata authority"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
