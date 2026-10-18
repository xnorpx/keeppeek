//! Catalog authority for verified archive media after explicit local eviction.

use super::to_u64;
use std::{path::PathBuf, sync::Arc};

#[derive(Debug, Clone)]
pub struct Eviction {
    pub recording_id: String,
    pub operation: String,
    pub source_path: PathBuf,
    pub source_root: PathBuf,
    pub source_filesystem: String,
    pub source_root_identity: String,
    pub source_relative_key: String,
    pub source_identity: String,
    pub source_bytes: u64,
    pub archive: ArchiveRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRead {
    pub target_id: String,
    pub root: PathBuf,
    pub filesystem: String,
    pub root_identity: String,
    pub object_id: String,
    pub version: String,
    pub bytes: u64,
    pub digest: [u8; 32],
    pub chunk_hashes: Arc<[u8]>,
}

impl ArchiveRead {
    pub fn relative_key(&self) -> String {
        format!("{}.mp4", self.object_id)
    }

    pub fn path(&self) -> PathBuf {
        self.root.join(self.relative_key())
    }

    pub(crate) fn from_ready_export(job: &super::archive::Job) -> anyhow::Result<Self> {
        anyhow::ensure!(
            job.kind == "export" && job.state == "ready",
            "archive export is not ready"
        );
        let version = job
            .version
            .clone()
            .ok_or_else(|| anyhow::anyhow!("archive export version is missing"))?;
        super::super::archive::target::ObjectId::parse(&job.object_id)?;
        let expected_hashes = usize::try_from(job.bytes.div_ceil(4 * 1024 * 1024))?
            .checked_mul(32)
            .ok_or_else(|| anyhow::anyhow!("archive export chunk count overflow"))?;
        anyhow::ensure!(
            job.chunk_hashes.len() == expected_hashes,
            "archive export integrity index is incomplete"
        );
        Ok(Self {
            target_id: job.binding.id.clone(),
            root: job.binding.root.clone(),
            filesystem: job.binding.identity.filesystem.clone(),
            root_identity: job.binding.identity.directory.clone(),
            object_id: job.object_id.clone(),
            version,
            bytes: job.bytes,
            digest: job.digest,
            chunk_hashes: Arc::from(job.chunk_hashes.clone()),
        })
    }
}

pub(super) async fn read(
    connection: &turso::Connection,
    recording_id: &str,
) -> anyhow::Result<Option<ArchiveRead>> {
    let mut rows = connection
        .query(
            "SELECT a.local_evicted, a.state, a.target_id, b.root, b.filesystem,
                    b.root_identity, a.object_id, a.version, a.bytes, a.digest, a.chunk_hashes
             FROM archive_replicas a LEFT JOIN archive_bindings b ON b.id=a.target_id
             WHERE a.kind='recording' AND a.recording_id=?1",
            [recording_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let evicted: i64 = row.get(0)?;
    if evicted == 0 {
        return Ok(None);
    }
    anyhow::ensure!(
        evicted == 1 || evicted == 2,
        "invalid archive eviction state"
    );
    anyhow::ensure!(
        row.get::<String>(1)? == "ready",
        "evicted archive is not ready"
    );
    let root = row
        .get::<Option<String>>(3)?
        .ok_or_else(|| anyhow::anyhow!("archive binding is missing"))?;
    let filesystem = row
        .get::<Option<String>>(4)?
        .ok_or_else(|| anyhow::anyhow!("archive filesystem is missing"))?;
    let root_identity = row
        .get::<Option<String>>(5)?
        .ok_or_else(|| anyhow::anyhow!("archive root identity is missing"))?;
    let digest: [u8; 32] = row
        .get::<Vec<u8>>(9)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid archive digest"))?;
    let descriptor = ArchiveRead {
        target_id: row.get(2)?,
        root: PathBuf::from(root),
        filesystem,
        root_identity,
        object_id: row.get(6)?,
        version: row.get(7)?,
        bytes: to_u64(row.get(8)?, "archive bytes")?,
        digest,
        chunk_hashes: Arc::from(row.get::<Vec<u8>>(10)?),
    };
    super::super::archive::target::ObjectId::parse(&descriptor.object_id)?;
    let hashes = usize::try_from(descriptor.bytes.div_ceil(4 * 1024 * 1024))?
        .checked_mul(32)
        .ok_or_else(|| anyhow::anyhow!("archive chunk count overflow"))?;
    anyhow::ensure!(
        descriptor.chunk_hashes.len() == hashes,
        "invalid archive chunk hashes"
    );
    Ok(Some(descriptor))
}

pub(super) async fn preserves_index(
    connection: &turso::Connection,
    recording_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM archive_replicas WHERE kind='recording' AND recording_id=?1 AND (state='ready' OR local_evicted!=0)",
            [recording_id],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

pub(super) async fn ensure_index_deletable(
    connection: &turso::Connection,
    recording_id: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !preserves_index(connection, recording_id).await?,
        "archive replica preserves recording index"
    );
    Ok(())
}

pub(super) async fn begin_eviction(
    connection: &turso::Connection,
    readers: &super::readers::Registry,
    recording_id: &str,
    operation: &str,
) -> anyhow::Result<Eviction> {
    let mut actions=connection.query("SELECT 1 FROM archive_replicas WHERE recording_id=?1 AND kind='recording' AND action='evict' AND cancelled=0",[recording_id]).await?;
    anyhow::ensure!(
        actions.next().await?.is_some(),
        "archive eviction was cancelled or superseded"
    );
    drop(actions);
    anyhow::ensure!(
        !operation.is_empty() && operation.len() <= 256,
        "invalid archive eviction operation"
    );
    super::readers::ensure_recording_idle(connection, readers, recording_id, "").await?;
    let source = source_snapshot(connection, recording_id).await?;
    anyhow::ensure!(
        source.operation == operation || source.operation.is_empty(),
        "archive eviction operation changed"
    );
    anyhow::ensure!(
        source.evicted == 0 || (source.evicted == 2 && source.operation == operation),
        "archive eviction already committed"
    );
    if source.evicted == 0 {
        super::retention::runtime::ensure_admission_ready(connection).await?;
        super::retention::ensure_cleanup_allowed(connection, recording_id).await?;
        ensure_source_available(connection, recording_id, &source).await?;
        let changed = connection.execute(
            "UPDATE archive_replicas SET local_evicted=2,source_retirement_operation=?2
             WHERE kind='recording' AND recording_id=?1 AND state='ready' AND local_evicted=0 AND source_retirement_operation IS NULL",
            turso::params![recording_id, operation],
        ).await?;
        anyhow::ensure!(changed == 1, "archive eviction admission changed");
        super::bump_catalog_revision(connection).await?;
    }
    let archive = read(connection, recording_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("ready archive disappeared"))?;
    Ok(Eviction {
        recording_id: recording_id.to_owned(),
        operation: operation.to_owned(),
        source_path: source.path,
        source_root: source.root,
        source_filesystem: source.filesystem,
        source_root_identity: source.root_identity,
        source_relative_key: source.relative_key,
        source_identity: source.identity,
        source_bytes: source.bytes,
        archive,
    })
}

struct Source {
    path: PathBuf,
    root: PathBuf,
    filesystem: String,
    root_identity: String,
    relative_key: String,
    identity: String,
    source_operation: String,
    operation: String,
    bytes: u64,
    evicted: i64,
}

async fn source_snapshot(connection: &turso::Connection, id: &str) -> anyhow::Result<Source> {
    let mut rows = connection.query(
        "SELECT r.path,a.source_root,a.source_filesystem,a.source_root_identity,
                a.source_relative_key,a.source_identity,a.source_operation,
                COALESCE(a.source_retirement_operation,''),r.file_bytes,a.local_evicted
         FROM archive_replicas a JOIN recording_files r ON r.id=a.recording_id
         WHERE a.kind='recording' AND a.recording_id=?1 AND a.state='ready' AND r.finalized=1 AND r.cleanup_pending=0
           AND replace(r.path,char(92),'/')=replace(a.source_path,char(92),'/') COLLATE NOCASE",
        [id],
    ).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("ready archive source changed"))?;
    Ok(Source {
        path: PathBuf::from(row.get::<String>(0)?),
        root: PathBuf::from(row.get::<String>(1)?),
        filesystem: row.get(2)?,
        root_identity: row.get(3)?,
        relative_key: row.get(4)?,
        identity: row.get(5)?,
        source_operation: row.get(6)?,
        operation: row.get(7)?,
        bytes: to_u64(row.get(8)?, "source bytes")?,
        evicted: row.get(9)?,
    })
}

async fn ensure_source_available(
    connection: &turso::Connection,
    id: &str,
    source: &Source,
) -> anyhow::Result<()> {
    let mut rows = connection.query(
        "SELECT 1 FROM storage_volume_allocations s JOIN recording_files r ON r.id=s.object_id
         WHERE s.operation=?1 AND s.kind='recording' AND s.object_id=?2
           AND s.state='published' AND s.file_identity=?3 AND s.bytes=?4
           AND s.destination_path=replace(?5,char(92),'/') COLLATE NOCASE
           AND r.protected=0 AND r.retention_pending=0
           AND NOT EXISTS(SELECT 1 FROM storage_recording_retirements WHERE recording_id=?2 AND complete=0)
           AND NOT EXISTS(SELECT 1 FROM recording_maintenance_claims WHERE recording_id=?2 AND active=1)
           AND NOT EXISTS(SELECT 1 FROM storage_volume_moves WHERE source_operation=s.operation AND phase IN ('published','retiring','complete'))
           AND NOT EXISTS(SELECT 1 FROM storage_recording_recovery WHERE recording_id=?2 AND complete=0)",
        turso::params![source.source_operation.as_str(), id, source.identity.as_str(), super::to_i64(source.bytes, "source bytes")?, source.path.to_string_lossy()],
    ).await?;
    anyhow::ensure!(
        rows.next().await?.is_some(),
        "archive source is not available for eviction"
    );
    Ok(())
}

pub(super) async fn finish_eviction(
    connection: &turso::Connection,
    recording_id: &str,
    operation: &str,
) -> anyhow::Result<()> {
    let source = source_snapshot(connection, recording_id).await?;
    anyhow::ensure!(
        source.operation == operation,
        "archive eviction operation changed"
    );
    if source.evicted == 1 {
        return Ok(());
    }
    anyhow::ensure!(source.evicted == 2, "archive eviction was not started");
    let changed = connection
        .execute(
            "UPDATE storage_volume_allocations SET state='cancelled'
         WHERE operation=?1 AND kind='recording' AND object_id=?2 AND state='published'",
            turso::params![source.source_operation, recording_id],
        )
        .await?;
    anyhow::ensure!(changed == 1, "archive source allocation changed");
    let changed = connection
        .execute(
            "UPDATE archive_replicas SET local_evicted=1 WHERE kind='recording' AND recording_id=?1
         AND source_retirement_operation=?2 AND local_evicted=2 AND state='ready'",
            turso::params![recording_id, operation],
        )
        .await?;
    anyhow::ensure!(changed == 1, "archive eviction state changed");
    super::locations::bump_revision(connection).await?;
    super::bump_catalog_revision(connection).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_eviction_selects_verified_archive_and_preserves_index() -> anyhow::Result<()> {
        pollster::block_on(async {
            let database = turso::Builder::new_local(":memory:")
                .experimental_generated_columns(true)
                .build()
                .await?;
            let connection = database.connect()?;
            super::super::initialize_schema(&connection).await?;
            connection
                .execute_batch(
                    "INSERT INTO archive_bindings VALUES('cold','/archive','fs','root');
                 INSERT INTO archive_replicas(job_id,recording_id,kind,target_id,object_id,
                     source_path,source_identity,bytes,digest,chunk_hashes,source_root,
                     source_filesystem,source_root_identity,source_relative_key,source_operation,
                     version,state)
                 VALUES('job','rec','recording','cold','6c0a2dda-bd3f-4e33-8224-77c69170820c',
                     '/active/source.mp4','source',1,zeroblob(32),zeroblob(32),'/active',
                     'fs','root','source.mp4','operation','archive-version','ready');",
                )
                .await?;
            assert!(read(&connection, "rec").await?.is_none());
            assert!(preserves_index(&connection, "rec").await?);
            assert!(ensure_index_deletable(&connection, "rec").await.is_err());
            connection
                .execute(
                    "UPDATE archive_replicas SET local_evicted=2 WHERE recording_id='rec'",
                    (),
                )
                .await?;
            let archived = read(&connection, "rec").await?.expect("archive authority");
            assert_eq!(
                archived.path(),
                PathBuf::from("/archive/6c0a2dda-bd3f-4e33-8224-77c69170820c.mp4")
            );
            connection
                .execute(
                    "UPDATE archive_replicas SET state='failed' WHERE recording_id='rec'",
                    (),
                )
                .await?;
            assert!(read(&connection, "rec").await.is_err());
            assert!(preserves_index(&connection, "rec").await?);
            Ok(())
        })
    }
}
