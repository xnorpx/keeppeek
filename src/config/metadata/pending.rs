pub mod control;
use super::*;
use crate::storage::StorageConfig;
use sha2::{Digest, Sha256};
use std::path::Path;

pub const PENDING: &str = "metadata_pending";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub source_digest: [u8; 32],
    pub target: MetadataBinding,
}

pub fn fingerprint(storage: &StorageToml) -> anyhow::Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(storage)?).into())
}

pub(in crate::config) fn preserve_storage(
    previous_text: &str,
    candidate: &toml::Table,
    updated: &StorageToml,
    secrets: &crate::config::Secrets,
) -> anyhow::Result<()> {
    let previous: toml::Table = toml::from_str(previous_text)?;
    if previous
        .get("storage")
        .and_then(|storage| storage.get(PENDING))
        .is_none()
    {
        return Ok(());
    }
    anyhow::ensure!(
        !candidate.contains_key(crate::config::STORAGE_MIGRATION_SECTION),
        "metadata handoff conflicts with legacy storage migration"
    );
    let original = crate::config::config_from_table(&previous, secrets)?;
    anyhow::ensure!(
        fingerprint(&original.storage)? == fingerprint(updated)?,
        "storage changes require cancelling the pending metadata handoff first"
    );
    Ok(())
}

pub fn apply(
    path: &Path,
    root: &mut toml::Table,
    secrets: &crate::config::Secrets,
) -> anyhow::Result<()> {
    let Some(raw) = root
        .get("storage")
        .and_then(|storage| storage.get(PENDING))
        .cloned()
    else {
        return Ok(());
    };
    anyhow::ensure!(
        !root.contains_key(crate::config::STORAGE_MIGRATION_SECTION),
        "metadata handoff conflicts with legacy storage migration"
    );
    let mut resolved = raw.clone();
    crate::config::resolve_toml_secret_references(&mut resolved, secrets)?;
    let pending: Pending = resolved.try_into()?;
    let current = crate::config::config_from_table(root, secrets)?;
    anyhow::ensure!(
        pending.source_digest == fingerprint(&current.storage)?,
        "storage configuration changed after metadata confirmation"
    );
    let candidate = candidate(root, &raw, &current.storage, &pending)?;
    let next = crate::config::config_from_table(&candidate, secrets)?;
    let source = StorageConfig::from_toml(&current.storage);
    let source_root = managed_source(&current.storage, &pending.target)?;
    let source_history = source
        .metadata_history_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| source.long_term_path.join(".exports/history.json"));
    let (catalog, history) = pending.target.paths(&next.storage)?;
    let destination =
        crate::storage::volumes::root::Root::open(catalog.parent().expect("metadata leaf"))?;
    anyhow::ensure!(
        *destination.identity() == pending.target.root_identity(),
        "metadata destination root changed"
    );
    crate::storage::catalog::authority::transfer_metadata_checked(
        &source.recording_catalog_path,
        &catalog,
        &source_history,
        &history,
        crate::storage::catalog::authority::TransferCheck {
            authority: &pending.target.authority(),
            destination: &destination,
            source: source_root.as_ref(),
            volume: next
                .storage
                .named_volumes
                .as_ref()
                .expect("validated metadata volume")
                .volumes
                .iter()
                .find(|volume| volume.id == pending.target.volume_id)
                .expect("validated metadata owner"),
            validate_history: crate::server::validate_export_history_snapshot,
        },
    )?;
    destination.revalidate()?;
    crate::config::write_private_file_atomically(
        path,
        toml::to_string_pretty(&candidate)?.as_bytes(),
    )?;
    *root = candidate;
    Ok(())
}

fn managed_source(
    storage: &StorageToml,
    target: &MetadataBinding,
) -> anyhow::Result<Option<crate::storage::volumes::root::Root>> {
    let Some(owner) = &storage.metadata else {
        return Ok(None);
    };
    anyhow::ensure!(
        owner.catalog_id == target.catalog_id
            && owner.generation.checked_add(1) == Some(target.generation),
        "metadata source binding changed"
    );
    let (catalog, _) = owner.paths(storage)?;
    let root = crate::storage::volumes::root::Root::open(catalog.parent().expect("metadata leaf"))?;
    anyhow::ensure!(
        *root.identity() == owner.root_identity(),
        "metadata source root changed"
    );
    Ok(Some(root))
}

fn candidate(
    root: &toml::Table,
    raw: &toml::Value,
    current: &StorageToml,
    pending: &Pending,
) -> anyhow::Result<toml::Table> {
    let mut candidate = root.clone();
    let storage = candidate
        .get_mut("storage")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("storage configuration is unavailable"))?;
    let configured = current
        .named_volumes
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("metadata volume is not configured"))?;
    let volumes = storage
        .get_mut("named_volumes")
        .and_then(|value| value.get_mut("volumes"))
        .and_then(toml::Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("metadata volumes are unavailable"))?;
    for (value, volume) in volumes.iter_mut().zip(&configured.volumes) {
        let state = if volume.id == pending.target.volume_id {
            Some("enabled")
        } else if current
            .metadata
            .as_ref()
            .is_some_and(|owner| owner.volume_id == volume.id)
        {
            Some("disabled")
        } else {
            None
        };
        if let Some(state) = state {
            value
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("invalid volume table"))?
                .insert("state".into(), state.into());
        }
    }
    storage.insert(
        "metadata".into(),
        raw.get("target")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("metadata target is missing"))?,
    );
    storage.remove("recording_catalog_path");
    storage.remove(PENDING);
    Ok(candidate)
}

#[cfg(test)]
mod tests;
