//! Freezes effective legacy paths at explicit adoption or first activation.
//! Disabled configuration drafts must not register a snapshot.

use crate::storage::volumes::validation::comparison_root;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SNAPSHOT_BYTES_MAX: usize = 32_768;

pub mod inventory;
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
pub(super) async fn register(
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
