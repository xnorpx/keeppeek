use super::{Object, Reply, bump_revision, fmt, to_i64, to_u64, validate_key};

/// Evidence captured from the pinned, synchronized destination before publication.
/// The owner must verify the digest and identity through the same opened handle.
#[derive(Clone, PartialEq, Eq)]
pub struct Publication {
    pub operation: String,
    pub bytes: u64,
    pub file_identity: String,
    pub digest: [u8; 32],
}

impl fmt::Debug for Publication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Publication")
            .field("operation", &self.operation)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

/// One authoritative location; reservations never appear as readable locations.
#[derive(Clone, PartialEq, Eq)]
pub struct Location {
    pub object: Object,
    pub volume: String,
    pub generation: u64,
    pub relative_key: String,
    pub revision: u64,
    pub bytes: u64,
    pub file_identity: String,
    pub digest: [u8; 32],
}

impl fmt::Debug for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Location")
            .field("object", &self.object)
            .field("volume", &self.volume)
            .field("revision", &self.revision)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

pub(super) async fn finalize(
    connection: &turso::Connection,
    publication: &Publication,
) -> anyhow::Result<Reply> {
    let mut rows = connection.query("SELECT a.object_id, a.state, r.path, r.finalized, b.root, a.relative_key FROM storage_volume_allocations a JOIN storage_volume_bindings b ON b.id = a.volume_id JOIN recording_files r ON r.id = a.object_id WHERE a.operation = ?1 AND a.kind = 'recording'", [publication.operation.as_str()]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording allocation does not exist"))?;
    let id = row.get::<String>(0)?;
    let state = row.get::<String>(1)?;
    let expected = std::path::PathBuf::from(row.get::<String>(4)?).join(row.get::<String>(5)?);
    anyhow::ensure!(
        std::path::Path::new(&row.get::<String>(2)?) == expected,
        "recording is not at its reserved location"
    );
    if state == "published" {
        anyhow::ensure!(
            row.get::<i64>(3)? == 1,
            "published recording is not finalized"
        );
        drop(rows);
        return publish(connection, publication).await;
    }
    anyhow::ensure!(
        state == "reserved" && row.get::<i64>(3)? == 0,
        "recording is not an active reserved recording"
    );
    drop(rows);
    connection
        .execute(
            "UPDATE recording_files SET finalized = 1,
            finalized_at_ms = CAST(unixepoch('subsec') * 1000 AS INTEGER),
            ended_at_ms = COALESCE((SELECT MAX(start_ms + duration_ms)
                FROM recording_fragments WHERE recording_id = ?1), ended_at_ms),
            file_bytes = ?2, file_identity = ?3 WHERE id = ?1",
            turso::params![
                id.clone(),
                to_i64(publication.bytes, "recording bytes")?,
                publication.file_identity.clone()
            ],
        )
        .await?;
    super::super::rebuild_recording_coverage(connection, &id).await?;
    publish(connection, publication).await
}

pub(super) async fn publish(
    connection: &turso::Connection,
    publication: &Publication,
) -> anyhow::Result<Reply> {
    super::images::retirement::ensure_writable(connection, &publication.operation).await?;
    super::export_cleanup::ensure_active_operation(connection, &publication.operation).await?;
    let mut moves = connection
        .query(
            "SELECT 1 FROM storage_volume_moves WHERE destination_operation = ?1 OR (source_operation = ?1 AND phase IN ('published','retiring','complete'))",
            [publication.operation.as_str()],
        )
        .await?;
    anyhow::ensure!(
        moves.next().await?.is_none(),
        "move publication requires its journal transition"
    );
    drop(moves);
    let mut rows = connection.query("SELECT kind, object_id, bytes, state, file_identity, digest FROM storage_volume_allocations WHERE operation = ?1", [publication.operation.as_str()]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("allocation does not exist"))?;
    let object = Object {
        kind: parse_kind(&row.get::<String>(0)?)?,
        id: row.get::<String>(1)?,
    };
    let bytes = to_u64(row.get::<i64>(2)?, "allocation bytes")?;
    match row.get::<String>(3)?.as_str() {
        "published" => {
            anyhow::ensure!(
                bytes == publication.bytes
                    && row.get::<String>(4)? == publication.file_identity
                    && row.get::<Vec<u8>>(5)? == publication.digest,
                "publication intent changed"
            );
            return lookup(connection, &object).await;
        }
        "reserved" => anyhow::ensure!(
            publication.bytes <= bytes,
            "publication exceeds reservation"
        ),
        _ => anyhow::bail!("allocation cannot be published"),
    }
    anyhow::ensure!(
        row.get::<Option<String>>(4)?
            .is_none_or(|identity| identity == publication.file_identity),
        "publication file identity changed"
    );
    validate_recording_location(connection, &object, &publication.operation).await?;
    connection.execute("UPDATE storage_volume_allocations SET state = 'published', bytes = ?2, file_identity = ?3, digest = ?4, location_revision = 1 WHERE operation = ?1 AND state = 'reserved'", turso::params![publication.operation.clone(), to_i64(publication.bytes, "publication bytes")?, publication.file_identity.clone(), publication.digest.to_vec()]).await?;
    bump_revision(connection).await?;
    lookup(connection, &object).await
}

async fn validate_recording_location(
    connection: &turso::Connection,
    object: &Object,
    operation: &str,
) -> anyhow::Result<()> {
    if object.kind != super::Kind::Recording {
        return Ok(());
    }
    let mut rows = connection.query("SELECT r.path, r.finalized, b.root, a.relative_key FROM storage_volume_allocations a JOIN storage_volume_bindings b ON b.id = a.volume_id JOIN recording_files r ON r.id = a.object_id WHERE a.operation = ?1 AND a.kind = 'recording'", [operation]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording identity does not exist"))?;
    let expected = std::path::PathBuf::from(row.get::<String>(2)?).join(row.get::<String>(3)?);
    anyhow::ensure!(
        row.get::<i64>(1)? == 1 && std::path::Path::new(&row.get::<String>(0)?) == expected,
        "recording is not finalized at its reserved location"
    );
    Ok(())
}

pub(super) async fn lookup(
    connection: &turso::Connection,
    object: &Object,
) -> anyhow::Result<Reply> {
    let mut rows = connection.query("SELECT volume_id, generation, relative_key, location_revision, bytes, file_identity, digest FROM storage_volume_allocations a WHERE kind = ?1 AND object_id = ?2 AND state = 'published' AND NOT EXISTS (SELECT 1 FROM storage_volume_moves m WHERE m.source_operation = a.operation AND m.phase IN ('published','retiring','complete'))", turso::params![object.kind.as_str(), object.id.clone()]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(Reply::Location(None));
    };
    let relative_key = row.get::<String>(2)?;
    let volume = row.get::<String>(0)?;
    if volume.starts_with("legacy-") {
        crate::storage::volumes::root::validate_legacy_key(&relative_key)?;
    } else {
        validate_key(&relative_key)?;
    }
    let digest: [u8; 32] = row
        .get::<Vec<u8>>(6)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid location digest"))?;
    Ok(Reply::Location(Some(Location {
        object: object.clone(),
        volume,
        generation: to_u64(row.get::<i64>(1)?, "volume generation")?,
        relative_key,
        revision: to_u64(row.get::<i64>(3)?, "location revision")?,
        bytes: to_u64(row.get::<i64>(4)?, "location bytes")?,
        file_identity: row.get::<String>(5)?,
        digest,
    })))
}

pub(super) fn parse_kind(value: &str) -> anyhow::Result<super::Kind> {
    match value {
        "recording" => Ok(super::Kind::Recording),
        "export" => Ok(super::Kind::Export),
        "thumbnail" => Ok(super::Kind::Thumbnail),
        _ => anyhow::bail!("invalid object kind"),
    }
}
