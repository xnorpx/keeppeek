//! Repairs captured recordings before the catalog exposes any command handles.

use super::LegacyPaths;
use crate::storage::{
    catalog::{self, CatalogKeyframe, locations::recording_recovery},
    long_term::inspection::container,
    volumes::root::{OwnedFile, Root},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// The authority lease excludes other writers for this entire startup pass.
pub(in crate::storage::catalog) async fn repair(
    connection: &turso::Connection,
    paths: &LegacyPaths,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS storage_legacy_repair_cursor (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1), after_id TEXT NOT NULL);
        INSERT OR IGNORE INTO storage_legacy_repair_cursor VALUES(1,'');",
        )
        .await?;
    let mut rows = connection
        .query(
            "SELECT after_id FROM storage_legacy_repair_cursor WHERE singleton=1",
            (),
        )
        .await?;
    let after: String = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing startup repair cursor"))?
        .get(0)?;
    drop(rows);
    let mut ids = candidates(connection, &after).await?;
    if ids.is_empty() {
        ids = candidates(connection, "").await?;
    }
    // ponytail: one rotating page per startup avoids a second worker and bounds recovery delays.
    for id in ids {
        if Instant::now() >= deadline {
            break;
        }
        if let Err(error) = repair_one(connection, paths, &id, deadline).await {
            tracing::warn!(recording_id = id, %error, "preserving unresolved legacy recording at startup");
            anyhow::ensure!(
                connection.is_autocommit()?,
                "legacy repair transaction remains open"
            );
        }
        connection
            .execute(
                "UPDATE storage_legacy_repair_cursor SET after_id=?1 WHERE singleton=1",
                [id],
            )
            .await?;
    }
    Ok(())
}

async fn candidates(connection: &turso::Connection, after: &str) -> anyhow::Result<Vec<String>> {
    let mut rows = connection.query(
        "SELECT r.id FROM recording_files r WHERE r.id>?1 AND r.cleanup_pending=0
        AND (r.finalized=0 OR EXISTS(SELECT 1 FROM recording_fragments f LEFT JOIN recording_keyframes k
            ON k.recording_id=f.recording_id AND k.fragment_sequence=f.sequence
            WHERE f.recording_id=r.id AND k.recording_id IS NULL)) ORDER BY r.id LIMIT 64", [after]).await?;
    let mut ids = Vec::with_capacity(64);
    while let Some(row) = rows.next().await? {
        ids.push(row.get(0)?);
    }
    Ok(ids)
}

async fn repair_one(
    connection: &turso::Connection,
    paths: &LegacyPaths,
    id: &str,
    deadline: Instant,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT path,finalized,file_identity,file_bytes,started_at_ms,init_offset,init_len
        FROM recording_files WHERE id=?1",
            [id],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing legacy owner"))?;
    let original = PathBuf::from(row.get::<String>(0)?);
    let finalized = row.get::<i64>(1)? != 0;
    let path = if finalized {
        original.clone()
    } else {
        anyhow::ensure!(!original.try_exists()?, "legacy recording remains active");
        catalog::finalized_sibling(&original)
            .ok_or_else(|| anyhow::anyhow!("missing finalized sibling"))?
    };
    check_conflicts(connection, id, &original, &path).await?;
    let mut file = open(paths, &path)?;
    let before = file.file_mut().metadata()?;
    check_owner(&row, &file, before.len())?;
    check_inventory(connection, id, &mut file, deadline).await?;
    let index = container::inspect(file.file_mut(), before.len(), deadline)?;
    anyhow::ensure!(
        i64::try_from(index.initialization.offset)? == row.get::<i64>(5)?
            && i64::try_from(index.initialization.size)? == row.get::<i64>(6)?,
        "legacy initialization changed"
    );
    let keyframes = matching_keyframes(connection, id, row.get(4)?, &index).await?;
    drop(rows);
    let inspected = Inspected {
        path,
        file,
        metadata: before,
        finalized,
        deadline,
    };
    commit(connection, id, inspected, keyframes).await
}

fn check_owner(row: &turso::Row, file: &OwnedFile, bytes: u64) -> anyhow::Result<()> {
    let identity = file.catalog_identity()?;
    anyhow::ensure!(
        row.get::<Option<String>>(2)?
            .is_none_or(|known| known == identity),
        "legacy recording identity changed"
    );
    let known_bytes: i64 = row.get(3)?;
    anyhow::ensure!(
        known_bytes == 0 || u64::try_from(known_bytes)? == bytes,
        "legacy recording length changed"
    );
    Ok(())
}

fn open(paths: &LegacyPaths, path: &Path) -> anyhow::Result<OwnedFile> {
    let root = [&paths.archive_root, &paths.active_root]
        .into_iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| anyhow::anyhow!("legacy recording is outside captured roots"))?;
    let key = path
        .strip_prefix(root)?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("legacy recording path is not UTF-8"))?
        .replace('\\', "/");
    Root::open(root)?.inspect_legacy(&key)
}

async fn check_conflicts(
    connection: &turso::Connection,
    id: &str,
    original: &Path,
    path: &Path,
) -> anyhow::Result<()> {
    let mut rows = connection.query(
        "SELECT EXISTS(SELECT 1 FROM storage_volume_allocations WHERE kind='recording' AND state!='cancelled'
            AND (object_id=?1 OR destination_path COLLATE NOCASE IN (?2,?3)))
        OR EXISTS(SELECT 1 FROM recording_maintenance_claims WHERE active=1
            AND (recording_id=?1 OR replace(path,char(92),'/') COLLATE NOCASE IN (?2,?3)))",
        turso::params![id, original.to_string_lossy().replace('\\', "/"), path.to_string_lossy().replace('\\', "/")],
    ).await?;
    anyhow::ensure!(
        rows.next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing conflict result"))?
            .get::<i64>(0)?
            == 0,
        "legacy recording has a durable owner conflict"
    );
    Ok(())
}

async fn check_inventory(
    connection: &turso::Connection,
    id: &str,
    file: &mut OwnedFile,
    deadline: Instant,
) -> anyhow::Result<()> {
    let mut rows = connection.query("SELECT catalog_identity,bytes,digest FROM storage_legacy_recordings WHERE recording_id=?1 AND digest IS NOT NULL", [id]).await?;
    if let Some(row) = rows.next().await? {
        let (bytes, _, digest) = file.inspect_evidence_until(deadline)?;
        anyhow::ensure!(
            row.get::<String>(0)? == file.catalog_identity()?
                && u64::try_from(row.get::<i64>(1)?)? == bytes
                && row.get::<Vec<u8>>(2)? == digest,
            "verified legacy recording changed"
        );
    }
    Ok(())
}

async fn matching_keyframes(
    connection: &turso::Connection,
    id: &str,
    start_ms: i64,
    index: &container::Index,
) -> anyhow::Result<Vec<CatalogKeyframe>> {
    let fragments = recording_recovery::fragments(connection, id).await?;
    anyhow::ensure!(
        !fragments.is_empty() && fragments.len() == index.fragments.len(),
        "legacy fragment count changed"
    );
    let mut keys = Vec::with_capacity(fragments.len());
    for (known, actual) in fragments.iter().zip(&index.fragments) {
        let sample = &actual.first_sample;
        let expected_start = start_ms
            .checked_add(i64::try_from(actual.start_ms)?)
            .ok_or_else(|| anyhow::anyhow!("legacy fragment time overflow"))?;
        anyhow::ensure!(
            known.sequence == u64::from(sample.sequence_number)
                && known.offset == actual.range.offset
                && known.bytes == actual.range.size
                && known.start_ms == expected_start
                && known.duration_ms == actual.duration_ms
                && known.random_access == sample.is_sync,
            "legacy fragment metadata changed"
        );
        anyhow::ensure!(
            known
                .key_offset
                .is_none_or(|offset| offset == sample.location.offset)
                && known
                    .key_bytes
                    .is_none_or(|bytes| bytes == u64::from(sample.location.size)),
            "legacy keyframe metadata changed"
        );
        if known.key_offset.is_none() && sample.is_sync {
            keys.push(CatalogKeyframe {
                recording_id: id.to_owned(),
                fragment_sequence: known.sequence,
                byte_offset: sample.location.offset,
                byte_len: u64::from(sample.location.size),
            });
        }
    }
    Ok(keys)
}

struct Inspected {
    path: PathBuf,
    file: OwnedFile,
    metadata: std::fs::Metadata,
    finalized: bool,
    deadline: Instant,
}

impl Inspected {
    fn revalidate(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            Instant::now() < self.deadline,
            "legacy startup repair timed out"
        );
        self.file.revalidate()?;
        let after = self.file.file_mut().metadata()?;
        anyhow::ensure!(
            self.metadata.len() == after.len() && self.metadata.modified()? == after.modified()?,
            "legacy recording changed during inspection"
        );
        Ok(())
    }
}

async fn commit(
    connection: &turso::Connection,
    id: &str,
    mut inspected: Inspected,
    keyframes: Vec<CatalogKeyframe>,
) -> anyhow::Result<()> {
    inspected.revalidate()?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        if !inspected.finalized {
            connection.execute("UPDATE recording_files SET path=?2,finalized=1,file_identity=?3,file_bytes=?4,
                finalized_at_ms=COALESCE(finalized_at_ms,CAST(unixepoch('subsec')*1000 AS INTEGER)),
                ended_at_ms=(SELECT MAX(start_ms+duration_ms) FROM recording_fragments WHERE recording_id=?1) WHERE id=?1",
                turso::params![id, inspected.path.to_string_lossy().into_owned(), inspected.file.catalog_identity()?, i64::try_from(inspected.metadata.len())?]).await?;
            catalog::rebuild_recording_coverage(connection, id).await?;
        }
        catalog::insert_backfilled_keyframes_in_transaction(connection, id, keyframes).await?;
        inspected.revalidate()?;
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(())
    }.await;
    if result.is_err() && !connection.is_autocommit()? {
        connection.execute_batch("ROLLBACK").await?;
    }
    result
}
