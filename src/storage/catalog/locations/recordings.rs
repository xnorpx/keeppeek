//! Journals volume-scoped recording retirement before filesystem removal.

use super::{Kind, Location, Object, Publication, Reply, bump_revision, to_u64};

impl super::RecordingCatalogHandle {
    pub(crate) fn legacy_recording_bytes(&self) -> anyhow::Result<u64> {
        match self.volume_location(super::Request::LegacyRecordingBytes)? {
            Reply::Bytes(bytes) => Ok(bytes),
            _ => unreachable!("legacy byte count reply"),
        }
    }
}

pub(super) async fn legacy_bytes(connection: &turso::Connection) -> anyhow::Result<u64> {
    let mut rows = connection.query("SELECT COALESCE(SUM(file_bytes),0) FROM recording_files r
        WHERE NOT EXISTS(SELECT 1 FROM storage_volume_allocations a WHERE a.kind='recording' AND a.state!='cancelled'
            AND (a.object_id=r.id OR a.destination_path=replace(r.path,char(92),'/') COLLATE NOCASE))", ()).await?;
    to_u64(
        rows.next().await?.expect("sum row").get(0)?,
        "legacy recording bytes",
    )
}

#[derive(Debug, Clone, Copy)]
pub enum Reason {
    Capacity,
    DiskPressure,
}

impl Reason {
    const fn deletion(self) -> super::super::CatalogDeletionReason {
        match self {
            Self::Capacity => super::super::CatalogDeletionReason::ArchiveLimit,
            Self::DiskPressure => super::super::CatalogDeletionReason::DiskPressure,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    Begin { volume: String, reason: Reason },
    Load(String),
    Complete(Publication),
    Acknowledge(String),
}

impl Action {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Begin { volume, .. } => super::identifier(volume),
            Self::Load(id) | Self::Acknowledge(id) => super::identifier(id),
            Self::Complete(evidence) => super::validate_publication(evidence),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub operation: String,
    pub location: Location,
    pub complete: bool,
    pub acknowledged: bool,
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS storage_recording_retirements (
        operation TEXT PRIMARY KEY REFERENCES storage_volume_allocations(operation),
        recording_id TEXT NOT NULL UNIQUE,
        volume_id TEXT NOT NULL,
        reason TEXT NOT NULL CHECK(reason IN ('archive_limit','disk_pressure')),
        complete INTEGER NOT NULL DEFAULT 0 CHECK(complete IN (0,1)),
        acknowledged INTEGER NOT NULL DEFAULT 0 CHECK(acknowledged IN (0,1))
    );
    CREATE INDEX IF NOT EXISTS storage_recording_retirement_pending ON storage_recording_retirements(volume_id,acknowledged);
    CREATE TRIGGER IF NOT EXISTS storage_recording_retirement_update_fence
    BEFORE UPDATE ON recording_files WHEN EXISTS(SELECT 1 FROM storage_recording_retirements WHERE recording_id=OLD.id AND complete=0)
    BEGIN SELECT RAISE(ABORT,'recording retirement owns this object'); END;").await?;
    Ok(())
}

pub(super) async fn dispatch(
    connection: &turso::Connection,
    action: Action,
) -> anyhow::Result<Reply> {
    match action {
        Action::Begin { volume, reason } => {
            return Ok(Reply::RecordingRetirement(
                begin(connection, &volume, reason).await?.map(Box::new),
            ));
        }
        Action::Load(id) => {
            return Ok(Reply::RecordingRetirement(
                load(connection, &id).await?.map(Box::new),
            ));
        }
        Action::Complete(evidence) => complete(connection, &evidence).await?,
        Action::Acknowledge(id) => {
            let job = load(connection, &id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("recording retirement is missing"))?;
            anyhow::ensure!(job.complete, "recording retirement is not complete");
            connection
                .execute(
                    "UPDATE storage_recording_retirements SET acknowledged=1 WHERE operation=?1",
                    [id],
                )
                .await?;
        }
    }
    Ok(Reply::Bound)
}

async fn begin(
    connection: &turso::Connection,
    volume: &str,
    reason: Reason,
) -> anyhow::Result<Option<Job>> {
    let mut pending = connection.query("SELECT operation FROM storage_recording_retirements WHERE volume_id=?1 AND acknowledged=0 LIMIT 1", [volume]).await?;
    if let Some(row) = pending.next().await? {
        let id: String = row.get(0)?;
        drop(pending);
        return load(connection, &id).await;
    }
    drop(pending);
    let mut rows = connection.query("SELECT a.operation,r.id FROM storage_volume_allocations a
        JOIN recording_files r ON r.id=a.object_id JOIN storage_volume_bindings b ON b.id=a.volume_id
        WHERE a.volume_id=?1 AND a.kind='recording' AND a.state='published' AND b.writable=1
        AND r.finalized=1 AND r.protected=0 AND r.cleanup_pending=0
        AND NOT EXISTS(SELECT 1 FROM storage_recording_retirements WHERE recording_id=r.id)
        AND NOT EXISTS(SELECT 1 FROM recording_maintenance_claims c WHERE c.active=1 AND (c.recording_id=r.id OR replace(c.path,char(92),'/')=a.destination_path COLLATE NOCASE))
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.kind='recording' AND m.object_id=r.id AND (m.phase NOT IN ('complete','cancelled') OR m.receipt_acknowledged=0))
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.source_operation=a.operation AND m.phase IN ('published','retiring','complete'))
        ORDER BY r.started_at_ms,r.id LIMIT 1", [volume]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let operation: String = row.get(0)?;
    let recording: String = row.get(1)?;
    drop(rows);
    connection.execute("INSERT INTO storage_recording_retirements(operation,recording_id,volume_id,reason) VALUES (?1,?2,?3,?4)", turso::params![operation.clone(), recording, volume, reason.deletion().as_str()]).await?;
    connection
        .execute(
            "UPDATE storage_volume_archives SET done=1 WHERE operation=?1",
            [operation.as_str()],
        )
        .await?;
    bump_revision(connection).await?;
    load(connection, &operation).await
}

async fn load(connection: &turso::Connection, operation: &str) -> anyhow::Result<Option<Job>> {
    let mut rows = connection.query("SELECT r.recording_id,a.volume_id,a.generation,a.relative_key,a.location_revision,a.bytes,a.file_identity,a.digest,r.complete,r.acknowledged FROM storage_recording_retirements r JOIN storage_volume_allocations a ON a.operation=r.operation WHERE r.operation=?1", [operation]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(Job {
        operation: operation.into(),
        location: Location {
            object: Object {
                kind: Kind::Recording,
                id: row.get(0)?,
            },
            volume: row.get(1)?,
            generation: to_u64(row.get(2)?, "volume generation")?,
            relative_key: row.get(3)?,
            revision: to_u64(row.get(4)?, "location revision")?,
            bytes: to_u64(row.get(5)?, "recording bytes")?,
            file_identity: row.get(6)?,
            digest: row
                .get::<Vec<u8>>(7)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid recording digest"))?,
        },
        complete: row.get::<i64>(8)? != 0,
        acknowledged: row.get::<i64>(9)? != 0,
    }))
}

async fn complete(connection: &turso::Connection, evidence: &Publication) -> anyhow::Result<()> {
    let job = load(connection, &evidence.operation)
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording retirement is missing"))?;
    anyhow::ensure!(
        job.location.bytes == evidence.bytes
            && job.location.file_identity == evidence.file_identity
            && job.location.digest == evidence.digest,
        "recording retirement evidence changed"
    );
    if job.complete {
        return Ok(());
    }
    let mut rows = connection
        .query(
            "SELECT reason FROM storage_recording_retirements WHERE operation=?1",
            [evidence.operation.as_str()],
        )
        .await?;
    let reason = rows
        .next()
        .await?
        .expect("retirement exists")
        .get::<String>(0)?;
    let reason = super::super::CatalogDeletionReason::parse(&reason)
        .ok_or_else(|| anyhow::anyhow!("invalid retirement reason"))?;
    drop(rows);
    super::super::record_deletion(connection, &job.location.object.id, reason).await?;
    connection
        .execute(
            "UPDATE storage_volume_allocations SET state='cancelled' WHERE operation=?1",
            [evidence.operation.as_str()],
        )
        .await?;
    connection
        .execute(
            "DELETE FROM recording_files WHERE id=?1",
            [job.location.object.id],
        )
        .await?;
    connection
        .execute(
            "UPDATE storage_recording_retirements SET complete=1 WHERE operation=?1",
            [evidence.operation.as_str()],
        )
        .await?;
    bump_revision(connection).await
}

pub(super) async fn ensure_active(
    connection: &turso::Connection,
    object: &Object,
) -> anyhow::Result<()> {
    if object.kind != Kind::Recording {
        return Ok(());
    }
    let mut rows = connection
        .query(
            "SELECT 1 FROM storage_recording_retirements WHERE recording_id=?1",
            [object.id.as_str()],
        )
        .await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "recording retirement owns this object"
    );
    Ok(())
}
