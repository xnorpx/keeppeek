//! Freezes effective legacy paths at explicit adoption or first activation.
//! Disabled configuration drafts must not register a snapshot.

use crate::storage::volumes::validation::comparison_root;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SNAPSHOT_BYTES_MAX: usize = 32_768;

pub mod adoption;
pub mod inventory;
pub mod roots;
pub(in crate::storage::catalog) mod startup;

/// Original effective paths, independent of later changes to placement defaults.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyPaths {
    pub active_root: PathBuf,
    pub archive_root: PathBuf,
    pub export_root: PathBuf,
    pub thumbnail_root: PathBuf,
    pub catalog_path: PathBuf,
    pub export_history_path: PathBuf,
}

impl std::fmt::Debug for LegacyPaths {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LegacyPaths([REDACTED])")
    }
}

impl LegacyPaths {
    pub(crate) fn effective(storage: &crate::storage::StorageConfig) -> anyhow::Result<Self> {
        use std::path::absolute;
        Ok(Self {
            active_root: absolute(&storage.medium_term_path)?,
            archive_root: absolute(&storage.long_term_path)?,
            thumbnail_root: absolute(&storage.event_thumbnail_path)?,
            export_root: absolute(storage.long_term_path.join(".exports"))?,
            catalog_path: absolute(&storage.recording_catalog_path)?,
            export_history_path: absolute(storage.long_term_path.join(".exports/history.json"))?,
        })
    }

    /// Keeps media roots fixed while named placement changes independently.
    pub(crate) fn ensure_same_media_roots(&self, effective: &Self) -> anyhow::Result<()> {
        for (captured, current) in [
            (&self.active_root, &effective.active_root),
            (&self.archive_root, &effective.archive_root),
            (&self.export_root, &effective.export_root),
            (&self.thumbnail_root, &effective.thumbnail_root),
        ] {
            anyhow::ensure!(
                comparison_root(captured)? == comparison_root(current)?,
                "captured legacy media roots changed; restore the original paths and use named placement or confirmed migration"
            );
        }
        Ok(())
    }

    /// Validates bounded absolute paths without probing or creating any directory.
    ///
    /// # Errors
    /// Rejects unsafe lexical paths, missing metadata filenames, or oversized snapshots.
    pub fn validate(&self) -> anyhow::Result<()> {
        for path in [
            &self.active_root,
            &self.archive_root,
            &self.export_root,
            &self.thumbnail_root,
            &self.catalog_path,
            &self.export_history_path,
        ] {
            comparison_root(path)?;
        }
        anyhow::ensure!(
            self.catalog_path.file_name().is_some()
                && self.export_history_path.file_name().is_some(),
            "legacy metadata paths require filenames"
        );
        anyhow::ensure!(
            serde_json::to_vec(self)?.len() <= SNAPSHOT_BYTES_MAX,
            "legacy path snapshot is too large"
        );
        Ok(())
    }
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS storage_legacy_paths (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1),
        snapshot TEXT NOT NULL CHECK(length(CAST(snapshot AS BLOB)) <= 32768)
    );
    CREATE TRIGGER IF NOT EXISTS storage_legacy_paths_update_fence
    BEFORE UPDATE ON storage_legacy_paths
    BEGIN SELECT RAISE(ABORT,'legacy path snapshot is immutable'); END;
    CREATE TRIGGER IF NOT EXISTS storage_legacy_paths_delete_fence
    BEFORE DELETE ON storage_legacy_paths
    BEGIN SELECT RAISE(ABORT,'legacy path snapshot is immutable'); END;",
        )
        .await?;
    Ok(())
}

/// First registration belongs to explicit adoption/activation, never draft loading.
/// A later request returns the original snapshot, even when defaults have changed.
pub(in crate::storage::catalog) async fn register(
    connection: &turso::Connection,
    requested: &LegacyPaths,
) -> anyhow::Result<LegacyPaths> {
    requested.validate()?;
    // ponytail: a singleton and insert-if-absent preserve the original defaults without a new journal.
    connection
        .execute(
            "INSERT OR IGNORE INTO storage_legacy_paths(singleton,snapshot) VALUES(1,?1)",
            [serde_json::to_string(requested)?],
        )
        .await?;
    load(connection)
        .await?
        .ok_or_else(|| anyhow::anyhow!("legacy path snapshot was not persisted"))
}

pub(crate) async fn load(connection: &turso::Connection) -> anyhow::Result<Option<LegacyPaths>> {
    let mut rows = connection
        .query(
            "SELECT snapshot FROM storage_legacy_paths WHERE singleton=1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let serialized: String = row.get(0)?;
    anyhow::ensure!(
        serialized.len() <= SNAPSHOT_BYTES_MAX,
        "legacy path snapshot is too large"
    );
    let snapshot: LegacyPaths = serde_json::from_str(&serialized)?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod inventory_tests;

#[cfg(test)]
mod inventory_boundary_tests;

#[cfg(test)]
mod startup_tests;

#[cfg(test)]
mod startup_repair_tests;

#[cfg(test)]
mod startup_boundary_tests;

#[cfg(test)]
mod roots_tests;

#[cfg(test)]
mod adoption_tests;
