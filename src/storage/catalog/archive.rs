//! Serializes archive intents and independently verified replica manifests.

use super::{Command, RecordingCatalogHandle, readers, to_i64, to_u64};
use crate::storage::{archive::target::Version, volumes::root::Identity};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Instant,
};

mod decode;
mod dispatch;
use dispatch::dispatch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub id: String,
    pub root: PathBuf,
    pub identity: Identity,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub recording_id: String,
    pub kind: String,
    pub binding: Binding,
    pub object_id: String,
    pub source_path: String,
    pub source_root: PathBuf,
    pub source_root_identity: Identity,
    pub source_relative_key: String,
    pub source_identity: String,
    pub source_operation: String,
    pub bytes: u64,
    pub digest: [u8; 32],
    pub chunk_hashes: Vec<u8>,
    pub stage: Option<Version>,
    pub version: Option<String>,
    pub state: String,
    pub attempts: u32,
    pub cancelled: bool,
    pub local_evicted: u8,
    pub error: Option<String>,
    pub action: String,
    pub action_attempts: u32,
    pub source_retirement_operation: Option<String>,
    pub restore_operation: Option<String>,
    pub restore_stage: Option<Version>,
    pub probe_acknowledged: bool,
    pub cleanup_version: Option<Version>,
    pub cleanup_digest: Option<[u8; 32]>,
    pub cleanup_key: Option<String>,
    pub cleanup_complete: bool,
    pub cleanup_acknowledged: bool,
    pub export_job_id: Option<String>,
    pub export_metadata: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Admission {
    pub job_id: String,
    pub recording_id: String,
    pub kind: String,
    pub target: String,
    pub object_limit: u64,
    pub count_limit: u32,
    pub bytes_limit: u64,
    pub export_job_id: Option<String>,
    pub export_metadata: Option<String>,
}

#[derive(Debug)]
pub enum Request {
    Bind(Binding),
    Enqueue(Admission),
    Get(String),
    FindExport(String),
    List {
        after: String,
        limit: u16,
    },
    Pending {
        after: String,
        limit: u16,
    },
    Candidates {
        after: String,
        before_ms: i64,
        limit: u16,
    },
    Start {
        id: String,
        maximum_attempts: u32,
    },
    Stage {
        id: String,
        version: Version,
    },
    Ready {
        id: String,
        version: Version,
        digest: [u8; 32],
        chunk_hashes: Vec<u8>,
    },
    Fail(String),
    Retry(String),
    Cancel(String),
    ProbeDeleted(String),
    ProbeAcknowledged(String),
    ReserveTransfer {
        id: String,
        capacity: super::locations::Capacity,
    },
    CleanupEvidence {
        id: String,
        version: Version,
        digest: [u8; 32],
        key: String,
    },
    CleanupComplete(String),
    CleanupAcknowledged(String),
    CleanupFailed(String),
    Action {
        id: String,
        action: String,
    },
    StartAction {
        id: String,
        maximum_attempts: u32,
    },
    ActionFailed {
        id: String,
    },
    ActionComplete {
        id: String,
    },
    BeginRestore {
        id: String,
        capacity: super::locations::Capacity,
    },
    RestoreStage {
        id: String,
        version: Version,
    },
    RestoreReady {
        id: String,
        version: Version,
        digest: [u8; 32],
    },
    BeginEviction {
        recording_id: String,
        operation: String,
    },
    FinishEviction {
        recording_id: String,
        operation: String,
    },
}

#[derive(Debug)]
pub enum Reply {
    Done,
    Job(Option<Box<Job>>),
    Jobs(Vec<Job>),
    Candidates(Vec<(String, Option<String>)>),
    Eviction(Box<super::archive_playback::Eviction>),
}

impl RecordingCatalogHandle {
    pub(crate) fn archive(&self, request: Request) -> anyhow::Result<Reply> {
        let (reply, response) = mpsc::sync_channel(1);
        let deadline = Instant::now() + super::BUSY_TIMEOUT;
        self.tx
            .try_send(Command::Archive {
                request,
                deadline,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("archive catalog is busy or unavailable"))?;
        response
            .recv_timeout(super::BUSY_TIMEOUT)
            .map_err(|_| anyhow::anyhow!("archive catalog reply timed out; reconcile by job ID"))?
    }

    pub(crate) fn archive_job(&self, id: &str) -> anyhow::Result<Job> {
        let Reply::Job(Some(job)) = self.archive(Request::Get(id.into()))? else {
            anyhow::bail!("archive job is unavailable");
        };
        Ok(*job)
    }
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(include_str!("archive/schema.sql"))
        .await?;
    Ok(())
}

pub(super) async fn execute(
    connection: &turso::Connection,
    registry: &Arc<readers::Registry>,
    request: Request,
    deadline: Instant,
) -> anyhow::Result<Reply> {
    anyhow::ensure!(
        Instant::now() < deadline,
        "archive catalog deadline expired"
    );
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = dispatch(connection, registry, request).await;
    match result {
        Ok(reply) => {
            if Instant::now() >= deadline {
                connection
                    .execute_batch("ROLLBACK")
                    .await
                    .map_err(|_| super::locations::FatalTransaction)?;
                anyhow::bail!("archive catalog deadline expired");
            }
            if let Err(error) = connection.execute_batch("COMMIT").await {
                connection
                    .execute_batch("ROLLBACK")
                    .await
                    .map_err(|_| super::locations::FatalTransaction)?;
                return Err(error.into());
            }
            Ok(reply)
        }
        Err(error) => {
            connection
                .execute_batch("ROLLBACK")
                .await
                .map_err(|_| super::locations::FatalTransaction)?;
            Err(error)
        }
    }
}

async fn bind(connection: &turso::Connection, binding: &Binding) -> anyhow::Result<Reply> {
    super::locations::identifier(&binding.id)?;
    let root = binding
        .root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid archive root encoding"))?;
    let mut rows = connection
        .query(
            "SELECT root,filesystem,root_identity FROM archive_bindings WHERE id=?1",
            [binding.id.as_str()],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        anyhow::ensure!(
            row.get::<String>(0)? == root
                && row.get::<String>(1)? == binding.identity.filesystem
                && row.get::<String>(2)? == binding.identity.directory,
            "archive target identity changed; use a new target ID"
        );
    } else {
        drop(rows);
        let mut count = connection
            .query("SELECT count(*) FROM archive_bindings", ())
            .await?;
        anyhow::ensure!(
            count.next().await?.expect("count row").get::<i64>(0)? < 64,
            "archive target binding limit reached"
        );
        drop(count);
        connection.execute("INSERT INTO archive_bindings(id,root,filesystem,root_identity) VALUES(?1,?2,?3,?4)",
            turso::params![binding.id.clone(),root,binding.identity.filesystem.clone(),binding.identity.directory.clone()]).await?;
    }
    Ok(Reply::Done)
}

async fn enqueue(connection: &turso::Connection, value: &Admission) -> anyhow::Result<Reply> {
    uuid::Uuid::parse_str(&value.job_id)?;
    anyhow::ensure!(
        matches!(value.kind.as_str(), "recording" | "export" | "probe"),
        "invalid archive media kind"
    );
    if let Some(existing) = load(connection, &value.job_id).await? {
        anyhow::ensure!(
            existing.recording_id == value.recording_id
                && existing.kind == value.kind
                && existing.binding.id == value.target
                && existing.export_job_id == value.export_job_id
                && existing.export_metadata == value.export_metadata,
            "archive job ID belongs to another intent"
        );
        return Ok(Reply::Job(Some(Box::new(existing))));
    }
    if value.kind == "probe" {
        connection.execute("DELETE FROM archive_replicas WHERE kind='probe' AND recording_id=?1
            AND ((state='deleted' AND probe_acknowledged=1) OR (state='cancelled' AND cleanup_acknowledged=1))",
            [value.recording_id.as_str()]).await?;
        let mut rows = connection
            .query(
                "SELECT job_id FROM archive_replicas WHERE kind='probe' AND recording_id=?1",
                [value.recording_id.as_str()],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            let id = row.get::<String>(0)?;
            drop(rows);
            return Ok(Reply::Job(load(connection, &id).await?.map(Box::new)));
        }
    }
    connection.execute("DELETE FROM archive_replicas WHERE kind=?1 AND recording_id=?2 AND state='cancelled' AND cleanup_acknowledged=1",
        (value.kind.as_str(),value.recording_id.as_str())).await?;
    let source = admission_source(connection, value).await?;
    validate_export_descriptor(value)?;
    check_capacity(connection, value, source.bytes).await?;
    connection.execute("INSERT INTO archive_replicas(job_id,recording_id,kind,target_id,object_id,source_path,source_identity,bytes,digest,
        source_root,source_filesystem,source_root_identity,source_relative_key,source_operation,state,export_job_id,export_metadata)
        VALUES(?1,?2,?3,?4,?1,?5,?6,?7,?8,?9,?10,?11,?12,?13,'queued',?14,?15)",
        turso::params![value.job_id.clone(),value.recording_id.clone(),value.kind.clone(),value.target.clone(),source.path,
            source.identity,to_i64(source.bytes,"archive bytes")?,source.digest.to_vec(),source.root,source.filesystem,
            source.root_identity,source.key,source.operation,value.export_job_id.clone(),value.export_metadata.clone()]).await?;
    Ok(Reply::Job(
        load(connection, &value.job_id).await?.map(Box::new),
    ))
}

fn validate_export_descriptor(value: &Admission) -> anyhow::Result<()> {
    if value.kind == "export" {
        anyhow::ensure!(
            value
                .export_job_id
                .as_ref()
                .is_some_and(|id| !id.is_empty() && id.len() <= 256)
                && value
                    .export_metadata
                    .as_ref()
                    .is_some_and(|metadata| !metadata.is_empty() && metadata.len() <= 65536),
            "export archive requires a bounded durable download descriptor"
        );
    } else {
        anyhow::ensure!(
            value.export_job_id.is_none() && value.export_metadata.is_none(),
            "unexpected archive export descriptor"
        );
    }
    Ok(())
}

struct Source {
    path: String,
    identity: String,
    bytes: u64,
    digest: [u8; 32],
    root: String,
    filesystem: String,
    root_identity: String,
    key: String,
    operation: String,
}

async fn admission_source(
    connection: &turso::Connection,
    value: &Admission,
) -> anyhow::Result<Source> {
    if value.kind == "probe" {
        use sha2::{Digest, Sha256};
        return Ok(Source {
            path: String::new(),
            identity: String::new(),
            bytes: crate::storage::archive::PROBE_PAYLOAD.len() as u64,
            digest: Sha256::digest(crate::storage::archive::PROBE_PAYLOAD).into(),
            root: String::new(),
            filesystem: String::new(),
            root_identity: String::new(),
            key: String::new(),
            operation: String::new(),
        });
    }
    if value.kind == "recording" {
        ensure_source_active(connection, &value.recording_id).await?;
    }
    let mut rows = connection.query("SELECT a.destination_path,a.file_identity,a.bytes,a.digest,b.root,b.filesystem,b.root_identity,a.relative_key,a.operation
        FROM storage_volume_allocations a JOIN storage_volume_bindings b ON b.id=a.volume_id AND b.generation=a.generation
        WHERE a.kind=?1 AND a.object_id=?2 AND a.state='published'
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.object_id=a.object_id AND m.kind=a.kind
            AND (m.phase NOT IN('complete','cancelled') OR m.receipt_acknowledged=0))",
        (value.kind.as_str(),value.recording_id.as_str())).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("archive source requires a finalized owned local object"))?;
    Ok(Source {
        path: row.get(0)?,
        identity: row.get(1)?,
        bytes: to_u64(row.get(2)?, "source bytes")?,
        digest: row
            .get::<Vec<u8>>(3)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid source digest"))?,
        root: row.get(4)?,
        filesystem: row.get(5)?,
        root_identity: row.get(6)?,
        key: row.get(7)?,
        operation: row.get(8)?,
    })
}

async fn check_capacity(
    connection: &turso::Connection,
    value: &Admission,
    bytes: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        bytes > 0 && bytes <= value.object_limit,
        "archive object exceeds configured byte limit"
    );
    let mut rows = connection.query("SELECT count(*),COALESCE(sum(bytes),0) FROM archive_replicas WHERE state IN('queued','copying','failed','cancelled') AND NOT(state='cancelled' AND cleanup_acknowledged=1)", ()).await?;
    let row = rows.next().await?.expect("aggregate row");
    anyhow::ensure!(
        to_u64(row.get(0)?, "pending count")? < u64::from(value.count_limit),
        "archive queue count limit reached"
    );
    anyhow::ensure!(
        to_u64(row.get(1)?, "pending bytes")?
            .checked_add(bytes)
            .is_some_and(|total| total <= value.bytes_limit),
        "archive queue byte limit reached"
    );
    Ok(())
}

async fn ensure_source_active(connection: &turso::Connection, id: &str) -> anyhow::Result<()> {
    let mut rows=connection.query("SELECT 1 FROM recording_files r WHERE r.id=?1 AND r.finalized=1 AND r.cleanup_pending=0
        AND NOT EXISTS(SELECT 1 FROM recording_maintenance_claims c WHERE c.recording_id=r.id AND c.active=1)
        AND NOT EXISTS(SELECT 1 FROM storage_recording_retirements q WHERE q.recording_id=r.id AND q.complete=0)
        AND NOT EXISTS(SELECT 1 FROM storage_recording_recovery q WHERE q.recording_id=r.id AND q.complete=0)", [id]).await?;
    anyhow::ensure!(
        rows.next().await?.is_some(),
        "recording is active, retiring or unavailable"
    );
    Ok(())
}

async fn start(
    connection: &turso::Connection,
    id: &str,
    maximum_attempts: u32,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        matches!(job.state.as_str(), "queued" | "copying" | "failed")
            && !job.cancelled
            && job.attempts < maximum_attempts,
        "archive job cannot start or has exhausted its attempts"
    );
    if job.kind == "recording" {
        ensure_source_active(connection, &job.recording_id).await?;
    }
    connection.execute("UPDATE archive_replicas SET state='copying',attempts=attempts+1,error=NULL WHERE job_id=?1",[id]).await?;
    Ok(Reply::Job(load(connection, id).await?.map(Box::new)))
}

async fn stage(
    connection: &turso::Connection,
    id: &str,
    version: &Version,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    version.validate()?;
    anyhow::ensure!(
        matches!(job.state.as_str(), "copying" | "cancelled")
            && version.object.to_string() == job.object_id
            && version.bytes <= job.bytes,
        "archive staging intent changed"
    );
    anyhow::ensure!(
        job.stage
            .as_ref()
            .is_none_or(|old| old.token == version.token),
        "archive staged identity changed"
    );
    connection.execute("UPDATE archive_replicas SET stage_identity=?2,stage_bytes=?3,target_materialized_bytes=MIN(target_reserved_bytes,?3),
        cleanup_complete=0,cleanup_acknowledged=0 WHERE job_id=?1",
        turso::params![id,version.token.clone(),to_i64(version.bytes,"staged bytes")?]).await?;
    super::locations::bump_revision(connection).await?;
    Ok(Reply::Done)
}

async fn ready(
    connection: &turso::Connection,
    id: &str,
    version: &Version,
    digest: [u8; 32],
    chunk_hashes: &[u8],
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    version.validate()?;
    anyhow::ensure!(
        job.state == "copying"
            && !job.cancelled
            && version.object.to_string() == job.object_id
            && version.bytes == job.bytes
            && digest == job.digest,
        "archive verification does not match source intent"
    );
    anyhow::ensure!(
        job.stage
            .as_ref()
            .is_some_and(|stage| stage.token == version.token && stage.bytes == job.bytes),
        "archive publication lacks durable staging evidence"
    );
    anyhow::ensure!(
        u64::try_from(chunk_hashes.len())?
            == job.bytes.div_ceil(crate::storage::archive::CHUNK_BYTES) * 32,
        "archive chunk verification evidence is incomplete"
    );
    if job.kind == "recording" {
        ensure_source_active(connection, &job.recording_id).await?;
    }
    if job.kind != "probe" {
        validate_source_location(connection, &job, "published").await?;
    }
    connection.execute("UPDATE archive_replicas SET state='ready',version=?2,chunk_hashes=?3,error=NULL,target_reserved_bytes=0,target_materialized_bytes=0 WHERE job_id=?1",turso::params![id,version.token.clone(),chunk_hashes.to_vec()]).await?;
    super::locations::bump_revision(connection).await?;
    Ok(Reply::Job(load(connection, id).await?.map(Box::new)))
}

async fn update_state(
    connection: &turso::Connection,
    id: &str,
    state: &str,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        matches!(job.state.as_str(), "queued" | "copying" | "failed"),
        "archive job already completed"
    );
    connection.execute("UPDATE archive_replicas SET state=?2,error='Archive operation failed; check target availability and retry' WHERE job_id=?1",(id,state)).await?;
    Ok(Reply::Done)
}

async fn retry(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    if job.cancelled {
        if !job.cleanup_acknowledged {
            connection
                .execute(
                    "UPDATE archive_replicas SET error=NULL WHERE job_id=?1",
                    [id],
                )
                .await?;
        } else {
            anyhow::ensure!(job.state == "cancelled", "cancelled archive state changed");
            if job.kind == "recording" {
                ensure_source_active(connection, &job.recording_id).await?;
            }
            if job.kind != "probe" {
                validate_source_location(connection, &job, "published").await?;
            }
            connection.execute("UPDATE archive_replicas SET state='queued',cancelled=0,attempts=0,error=NULL,
                stage_identity=NULL,stage_bytes=0,version=NULL,cleanup_identity=NULL,cleanup_bytes=NULL,cleanup_digest=NULL,
                cleanup_key=NULL,cleanup_complete=0,cleanup_acknowledged=0 WHERE job_id=?1",[id]).await?;
        }
        return Ok(Reply::Job(load(connection, id).await?.map(Box::new)));
    }
    if job.state == "ready" && !job.action.is_empty() {
        connection
            .execute(
                "UPDATE archive_replicas SET action_attempts=0,error=NULL WHERE job_id=?1",
                [id],
            )
            .await?;
        return Ok(Reply::Job(load(connection, id).await?.map(Box::new)));
    }
    anyhow::ensure!(
        job.state == "failed" && !job.cancelled,
        "only failed archive jobs can retry"
    );
    connection
        .execute(
            "UPDATE archive_replicas SET state='queued',attempts=0,error=NULL WHERE job_id=?1",
            [id],
        )
        .await?;
    Ok(Reply::Done)
}

async fn cancel(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    if job.state == "ready" {
        anyhow::ensure!(
            !job.action.is_empty(),
            "completed archive copies require explicit deletion"
        );
        if job.action == "evict" {
            anyhow::ensure!(
                job.local_evicted == 0,
                "primary retirement has started and must finish recovery before another action"
            );
            connection.execute("UPDATE archive_replicas SET action='',action_attempts=0,error=NULL WHERE job_id=?1",[id]).await?;
        } else {
            anyhow::ensure!(job.local_evicted == 1, "restore has already completed");
            connection.execute("UPDATE archive_replicas SET cancelled=1,cleanup_complete=0,cleanup_acknowledged=0,error=NULL WHERE job_id=?1",[id]).await?;
        }
        return Ok(Reply::Job(load(connection, id).await?.map(Box::new)));
    }
    anyhow::ensure!(
        matches!(job.state.as_str(), "queued" | "copying" | "failed"),
        "completed archive copies require explicit deletion"
    );
    connection.execute("UPDATE archive_replicas SET cancelled=1,state='cancelled',cleanup_complete=0,cleanup_acknowledged=0,error=NULL WHERE job_id=?1",[id]).await?;
    Ok(Reply::Done)
}

async fn cleanup_evidence(
    connection: &turso::Connection,
    id: &str,
    version: &Version,
    digest: [u8; 32],
    key: &str,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        job.cancelled && !job.cleanup_acknowledged,
        "archive cleanup is not pending"
    );
    let (object, stage) = if job.action == "restore" {
        (
            restore_object(&job.source_relative_key)?.to_string(),
            job.restore_stage.as_ref(),
        )
    } else {
        (job.object_id.clone(), job.stage.as_ref())
    };
    anyhow::ensure!(
        version.object.to_string() == object
            && version.bytes <= job.bytes
            && stage.is_some_and(|stage| stage.token == version.token),
        "archive cleanup identity changed"
    );
    anyhow::ensure!(
        key == format!("{object}.tmp") || key == format!("{object}.mp4"),
        "invalid archive cleanup key"
    );
    if let Some(previous) = &job.cleanup_version {
        anyhow::ensure!(
            previous == version
                && job.cleanup_digest == Some(digest)
                && job.cleanup_key.as_deref() == Some(key),
            "archive cleanup evidence changed"
        );
    }
    connection.execute("UPDATE archive_replicas SET cleanup_identity=?2,cleanup_bytes=?3,cleanup_digest=?4,cleanup_key=?5 WHERE job_id=?1",
        turso::params![id,version.token.clone(),to_i64(version.bytes,"cleanup bytes")?,digest.to_vec(),key]).await?;
    Ok(Reply::Done)
}

async fn reserve_transfer(
    connection: &turso::Connection,
    id: &str,
    capacity: &super::locations::Capacity,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        job.state == "copying" && !job.cancelled,
        "archive transfer is no longer pending"
    );
    anyhow::ensure!(
        capacity.ledger_revision == super::locations::revision(connection).await?
            && capacity.observed_at.elapsed() <= std::time::Duration::from_secs(5),
        "archive capacity observation expired"
    );
    anyhow::ensure!(
        capacity.filesystem == job.binding.identity.filesystem
            && capacity.root_identity == job.binding.identity.directory,
        "archive capacity root changed"
    );
    let mut rows=connection.query("SELECT
        (SELECT COALESCE(sum(reserved_bytes),0) FROM storage_volume_bindings WHERE filesystem=?1),
        (SELECT COALESCE(sum(a.target_reserved_bytes-a.target_materialized_bytes),0) FROM archive_replicas a JOIN archive_bindings b ON b.id=a.target_id WHERE b.filesystem=?1 AND a.job_id!=?2 AND a.target_reserved_bytes>a.target_materialized_bytes),
        (SELECT COALESCE(max(minimum_free_bytes),0) FROM storage_volume_bindings WHERE filesystem=?1 AND writable=1)",
        (capacity.filesystem.as_str(),id)).await?;
    let row = rows.next().await?.expect("capacity aggregate");
    let materialized = job
        .stage
        .as_ref()
        .map_or(0, |stage| stage.bytes.min(job.bytes));
    let minimum_free = to_u64(row.get(2)?, "recording free-space reserve")?;
    let reserved = to_u64(row.get(0)?, "volume reservations")?
        .checked_add(to_u64(row.get(1)?, "archive reservations")?)
        .and_then(|value| value.checked_add(job.bytes - materialized))
        .and_then(|value| value.checked_add(minimum_free))
        .ok_or_else(|| anyhow::anyhow!("archive reservation overflow"))?;
    anyhow::ensure!(
        capacity.available_bytes >= reserved,
        "archive target has insufficient unreserved capacity or recording headroom"
    );
    connection.execute("UPDATE archive_replicas SET target_reserved_bytes=bytes,target_materialized_bytes=?2 WHERE job_id=?1",(id,to_i64(materialized,"archive materialized bytes")?)).await?;
    super::locations::bump_revision(connection).await?;
    Ok(Reply::Done)
}

async fn candidates(
    connection: &turso::Connection,
    after: &str,
    before: i64,
    limit: u16,
) -> anyhow::Result<Reply> {
    anyhow::ensure!(
        (1..=64).contains(&limit),
        "invalid archive candidate page size"
    );
    let mut rows=connection.query("SELECT r.id,r.source_id FROM recording_files r
        WHERE r.id>?1 AND r.finalized=1 AND r.cleanup_pending=0 AND r.ended_at_ms<=?2
        AND NOT EXISTS(SELECT 1 FROM archive_replicas a WHERE a.recording_id=r.id AND a.kind='recording')
        AND EXISTS(SELECT 1 FROM storage_volume_allocations a WHERE a.kind='recording' AND a.object_id=r.id AND a.state='published')
        ORDER BY r.id LIMIT ?3",turso::params![after,before,i64::from(limit)]).await?;
    let mut values = Vec::with_capacity(usize::from(limit));
    while let Some(row) = rows.next().await? {
        values.push((row.get(0)?, row.get(1)?));
    }
    Ok(Reply::Candidates(values))
}

async fn page(connection: &turso::Connection, after: &str, limit: u16) -> anyhow::Result<Reply> {
    anyhow::ensure!((1..=64).contains(&limit), "invalid archive job page size");
    let mut rows = connection
        .query(
            "SELECT job_id FROM archive_replicas WHERE job_id>?1 ORDER BY job_id LIMIT ?2",
            (after, i64::from(limit)),
        )
        .await?;
    let mut ids = Vec::with_capacity(usize::from(limit));
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    drop(rows);
    let mut values = Vec::with_capacity(ids.len());
    for id in ids {
        values.push(
            load_internal(connection, &id, false)
                .await?
                .ok_or_else(|| anyhow::anyhow!("archive job disappeared"))?,
        );
    }
    Ok(Reply::Jobs(values))
}

async fn pending(connection: &turso::Connection, after: &str, limit: u16) -> anyhow::Result<Reply> {
    anyhow::ensure!(
        (1..=64).contains(&limit),
        "invalid archive pending page size"
    );
    let mut rows=connection.query("SELECT job_id FROM archive_replicas WHERE job_id>?1 AND ((state IN('queued','copying','failed') AND cancelled=0) OR (kind='probe' AND (state='ready' OR (state='deleted' AND probe_acknowledged=0))) OR (state='ready' AND action!='') OR (state='cancelled' AND cleanup_acknowledged=0)) ORDER BY job_id LIMIT ?2",(after,i64::from(limit))).await?;
    let mut ids = Vec::with_capacity(usize::from(limit));
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    drop(rows);
    let mut jobs = Vec::with_capacity(ids.len());
    for id in ids {
        jobs.push(required(connection, &id).await?);
    }
    Ok(Reply::Jobs(jobs))
}

async fn required(connection: &turso::Connection, id: &str) -> anyhow::Result<Job> {
    load(connection, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("archive job does not exist"))
}

pub(super) async fn load(connection: &turso::Connection, id: &str) -> anyhow::Result<Option<Job>> {
    load_internal(connection, id, true).await
}

async fn load_internal(
    connection: &turso::Connection,
    id: &str,
    integrity: bool,
) -> anyhow::Result<Option<Job>> {
    let mut rows=connection.query("SELECT a.job_id,a.recording_id,a.kind,a.target_id,a.object_id,a.source_path,a.source_identity,a.bytes,a.digest,
        a.source_root,a.source_filesystem,a.source_root_identity,a.source_relative_key,a.source_operation,a.stage_identity,a.stage_bytes,
        a.version,a.state,a.attempts,a.cancelled,a.local_evicted,a.error,b.root,b.filesystem,b.root_identity,CASE WHEN ?2=1 THEN a.chunk_hashes ELSE X'' END,
        a.action,a.action_attempts,a.source_retirement_operation,a.restore_operation,a.restore_identity,a.restore_bytes,a.probe_acknowledged,
        a.cleanup_identity,a.cleanup_bytes,a.cleanup_digest,a.cleanup_key,a.cleanup_complete,a.cleanup_acknowledged,a.export_job_id,a.export_metadata
        FROM archive_replicas a JOIN archive_bindings b ON b.id=a.target_id WHERE a.job_id=?1",(id,i64::from(integrity))).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(decode::job(&row)?))
}

async fn action(connection: &turso::Connection, id: &str, action: &str) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        job.kind == "recording" && job.state == "ready" && matches!(action, "evict" | "restore"),
        "archive action is unavailable"
    );
    anyhow::ensure!(
        job.action.is_empty() || job.action == action,
        "another archive action is pending"
    );
    anyhow::ensure!(
        if action == "evict" {
            job.local_evicted != 1
        } else {
            job.local_evicted == 1
        },
        "archive action does not match local availability"
    );
    connection
        .execute(
            "UPDATE archive_replicas SET action=?2,error=NULL WHERE job_id=?1",
            (id, action),
        )
        .await?;
    Ok(Reply::Job(load(connection, id).await?.map(Box::new)))
}

async fn start_action(
    connection: &turso::Connection,
    id: &str,
    maximum_attempts: u32,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        job.state == "ready" && !job.action.is_empty() && job.action_attempts < maximum_attempts,
        "archive action attempts exhausted"
    );
    connection
        .execute(
            "UPDATE archive_replicas SET action_attempts=action_attempts+1 WHERE job_id=?1",
            [id],
        )
        .await?;
    Ok(Reply::Job(load(connection, id).await?.map(Box::new)))
}

fn restore_object(key: &str) -> anyhow::Result<crate::storage::archive::target::ObjectId> {
    crate::storage::archive::target::ObjectId::parse(
        key.strip_suffix(".mp4")
            .ok_or_else(|| anyhow::anyhow!("restore destination is not a media leaf"))?,
    )
}

#[cfg(test)]
#[path = "archive/tests.rs"]
mod tests;

async fn begin_restore(
    connection: &turso::Connection,
    id: &str,
    capacity: &super::locations::Capacity,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        job.state == "ready" && job.action == "restore" && job.local_evicted == 1,
        "restore intent is unavailable"
    );
    restore_object(&job.source_relative_key)?;
    anyhow::ensure!(
        capacity.filesystem == job.source_root_identity.filesystem
            && capacity.root_identity == job.source_root_identity.directory,
        "restore root identity changed"
    );
    super::locations::reserve_archive_restore(connection, &job.source_operation, capacity).await?;
    validate_source_location(connection, &job, "reserved").await?;
    if job.restore_operation.is_none() {
        connection
            .execute(
                "UPDATE archive_replicas SET restore_operation=?2 WHERE job_id=?1",
                (id, uuid::Uuid::new_v4().to_string()),
            )
            .await?;
    }
    Ok(Reply::Job(load(connection, id).await?.map(Box::new)))
}

async fn restore_stage(
    connection: &turso::Connection,
    id: &str,
    version: &Version,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        job.action == "restore"
            && job.local_evicted == 1
            && job.restore_operation.is_some()
            && restore_object(&job.source_relative_key)? == version.object
            && version.bytes <= job.bytes,
        "restore staging intent changed"
    );
    anyhow::ensure!(
        job.restore_stage
            .as_ref()
            .is_none_or(|old| old.token == version.token),
        "restore staged identity changed"
    );
    connection
        .execute(
            "UPDATE archive_replicas SET restore_identity=?2,restore_bytes=?3 WHERE job_id=?1",
            turso::params![
                id,
                version.token.clone(),
                to_i64(version.bytes, "restore bytes")?
            ],
        )
        .await?;
    let changed=connection.execute("UPDATE storage_volume_allocations SET materialized_bytes=?2 WHERE operation=?1 AND state='reserved'",(job.source_operation.as_str(),to_i64(version.bytes,"restore materialized bytes")?)).await?;
    anyhow::ensure!(changed == 1, "restore reservation changed");
    super::locations::bump_revision(connection).await?;
    Ok(Reply::Done)
}

async fn restore_ready(
    connection: &turso::Connection,
    registry: &readers::Registry,
    id: &str,
    version: &Version,
    digest: [u8; 32],
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        job.action == "restore"
            && !job.cancelled
            && job.local_evicted == 1
            && job.digest == digest
            && version.bytes == job.bytes
            && job.restore_stage.as_ref() == Some(version),
        "restore verification evidence changed"
    );
    readers::ensure_recording_idle(connection, registry, &job.recording_id, "").await?;
    ensure_source_active(connection, &job.recording_id).await?;
    validate_source_location(connection, &job, "reserved").await?;
    let changed=connection.execute("UPDATE recording_files SET file_identity=?2 WHERE id=?1 AND finalized=1 AND cleanup_pending=0
        AND replace(path,char(92),'/')=replace(?3,char(92),'/') COLLATE NOCASE AND file_bytes=?4",
        turso::params![job.recording_id.clone(),super::locations::maintenance_identity(&version.token)?,job.source_path.clone(),to_i64(job.bytes,"restore bytes")?]).await?;
    anyhow::ensure!(changed == 1, "restored recording catalog identity changed");
    let changed=connection.execute("UPDATE storage_volume_allocations SET state='published',file_identity=?2,digest=?3,materialized_bytes=bytes,location_revision=location_revision+1 WHERE operation=?1 AND state='reserved'",
        turso::params![job.source_operation.clone(),version.token.clone(),digest.to_vec()]).await?;
    anyhow::ensure!(changed == 1, "restore allocation is unavailable");
    connection.execute("UPDATE archive_replicas SET local_evicted=0,source_identity=?2,source_retirement_operation=NULL,
        restore_operation=NULL,restore_identity=NULL,restore_bytes=0,action='',action_attempts=0,error=NULL WHERE job_id=?1",(id,version.token.as_str())).await?;
    super::locations::bump_revision(connection).await?;
    super::bump_catalog_revision(connection).await?;
    Ok(Reply::Done)
}

async fn validate_source_location(
    connection: &turso::Connection,
    job: &Job,
    state: &str,
) -> anyhow::Result<()> {
    let mut rows=connection.query("SELECT 1 FROM storage_volume_allocations a JOIN storage_volume_bindings b ON b.id=a.volume_id AND b.generation=a.generation
        WHERE a.operation=?1 AND a.kind=?2 AND a.object_id=?3 AND a.state=?4 AND a.bytes=?5
        AND a.relative_key=?6 AND a.destination_path=replace(?7,char(92),'/') COLLATE NOCASE
        AND b.root=?8 AND b.filesystem=?9 AND b.root_identity=?10
        AND (?4='reserved' OR (a.file_identity=?11 AND a.digest=?12))
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.object_id=a.object_id AND m.kind=a.kind
            AND (m.phase NOT IN('complete','cancelled') OR m.receipt_acknowledged=0))
        AND NOT EXISTS(SELECT 1 FROM storage_export_cleanup e WHERE a.kind='export' AND e.object_id=a.object_id)",
        turso::params![job.source_operation.clone(),job.kind.clone(),job.recording_id.clone(),state,to_i64(job.bytes,"source bytes")?,job.source_relative_key.clone(),job.source_path.clone(),
            job.source_root.to_string_lossy().into_owned(),job.source_root_identity.filesystem.clone(),job.source_root_identity.directory.clone(),job.source_identity.clone(),job.digest.to_vec()]).await?;
    anyhow::ensure!(
        rows.next().await?.is_some(),
        "archive primary location changed"
    );
    if job.kind == "recording" {
        let mut rows=connection.query("SELECT 1 FROM recording_files WHERE id=?1 AND finalized=1 AND cleanup_pending=0
            AND replace(path,char(92),'/')=replace(?2,char(92),'/') COLLATE NOCASE AND file_bytes=?3",
            turso::params![job.recording_id.clone(),job.source_path.clone(),to_i64(job.bytes,"source bytes")?]).await?;
        anyhow::ensure!(
            rows.next().await?.is_some(),
            "archive recording identity changed"
        );
    }
    Ok(())
}
