//! Applies archive requests inside the catalog transaction.

use super::*;

pub(super) async fn dispatch(
    connection: &turso::Connection,
    registry: &Arc<readers::Registry>,
    request: Request,
) -> anyhow::Result<Reply> {
    match request {
        Request::Bind(binding) => bind(connection, &binding).await,
        Request::Enqueue(admission) => enqueue(connection, &admission).await,
        Request::Get(id) => Ok(Reply::Job(load(connection, &id).await?.map(Box::new))),
        Request::FindExport(id) => find_export(connection, &id).await,
        Request::List { after, limit } => page(connection, &after, limit).await,
        Request::Pending { after, limit } => pending(connection, &after, limit).await,
        Request::Candidates {
            after,
            before_ms,
            limit,
        } => candidates(connection, &after, before_ms, limit).await,
        Request::Start {
            id,
            maximum_attempts,
        } => start(connection, &id, maximum_attempts).await,
        Request::Stage { id, version } => stage(connection, &id, &version).await,
        Request::Ready {
            id,
            version,
            digest,
            chunk_hashes,
        } => ready(connection, &id, &version, digest, &chunk_hashes).await,
        Request::Fail(id) => update_state(connection, &id, "failed").await,
        Request::Retry(id) => retry(connection, &id).await,
        Request::Cancel(id) => cancel(connection, &id).await,
        Request::ProbeDeleted(id) => probe_deleted(connection, &id).await,
        Request::ProbeAcknowledged(id) => probe_acknowledged(connection, &id).await,
        Request::Action { id, action } => self::action(connection, &id, &action).await,
        Request::ReserveTransfer { id, capacity } => {
            reserve_transfer(connection, &id, &capacity).await
        }
        Request::CleanupEvidence {
            id,
            version,
            digest,
            key,
        } => cleanup_evidence(connection, &id, &version, digest, &key).await,
        Request::CleanupComplete(id) => cleanup_complete(connection, &id).await,
        Request::CleanupAcknowledged(id) => cleanup_acknowledged(connection, &id).await,
        Request::CleanupFailed(id) => cleanup_failed(connection, &id).await,
        Request::StartAction {
            id,
            maximum_attempts,
        } => start_action(connection, &id, maximum_attempts).await,
        Request::ActionFailed { id } => action_failed(connection, &id).await,
        Request::ActionComplete { id } => action_complete(connection, &id).await,
        Request::BeginRestore { id, capacity } => begin_restore(connection, &id, &capacity).await,
        Request::RestoreStage { id, version } => restore_stage(connection, &id, &version).await,
        Request::RestoreReady {
            id,
            version,
            digest,
        } => restore_ready(connection, registry, &id, &version, digest).await,
        Request::BeginEviction {
            recording_id,
            operation,
        } => begin_eviction(connection, registry, &recording_id, &operation).await,
        Request::FinishEviction {
            recording_id,
            operation,
        } => finish_eviction(connection, &recording_id, &operation).await,
    }
}

async fn find_export(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    let mut rows = connection
        .query(
            "SELECT job_id FROM archive_replicas WHERE kind='export' AND export_job_id=?1",
            [id],
        )
        .await?;
    let job_id = rows
        .next()
        .await?
        .map(|row| row.get::<String>(0))
        .transpose()?;
    drop(rows);
    Ok(Reply::Job(if let Some(job_id) = job_id {
        load(connection, &job_id).await?.map(Box::new)
    } else {
        None
    }))
}

async fn probe_deleted(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        job.kind == "probe" && matches!(job.state.as_str(), "ready" | "deleted"),
        "probe retirement intent changed"
    );
    connection
        .execute(
            "UPDATE archive_replicas SET state='deleted' WHERE job_id=?1",
            [id],
        )
        .await?;
    Ok(Reply::Done)
}

async fn probe_acknowledged(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    connection.execute("UPDATE archive_replicas SET probe_acknowledged=1 WHERE job_id=?1 AND kind='probe' AND state='deleted'",[id]).await?;
    Ok(Reply::Done)
}

async fn cleanup_complete(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    connection.execute("UPDATE archive_replicas SET cleanup_complete=1 WHERE job_id=?1 AND cancelled=1 AND (cleanup_identity IS NOT NULL OR (stage_identity IS NULL AND action!='restore') OR (action='restore' AND restore_identity IS NULL))",[id]).await?;
    Ok(Reply::Done)
}

async fn cleanup_acknowledged(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    anyhow::ensure!(
        job.cancelled && job.cleanup_complete,
        "archive cleanup is incomplete"
    );
    if job.action == "restore" {
        let changed=connection.execute("UPDATE storage_volume_allocations SET state='cancelled' WHERE operation=?1 AND state='reserved'",[job.source_operation.as_str()]).await?;
        anyhow::ensure!(
            changed == 1 || job.restore_operation.is_none(),
            "cancelled restore allocation changed"
        );
        connection.execute("UPDATE archive_replicas SET cancelled=0,action='',action_attempts=0,restore_operation=NULL,restore_identity=NULL,restore_bytes=0,
            cleanup_identity=NULL,cleanup_bytes=NULL,cleanup_digest=NULL,cleanup_key=NULL,cleanup_complete=0,cleanup_acknowledged=0,error=NULL WHERE job_id=?1",[id]).await?;
    } else {
        connection.execute("UPDATE archive_replicas SET cleanup_acknowledged=1,target_reserved_bytes=0,target_materialized_bytes=0,error=NULL WHERE job_id=?1 AND state='cancelled' AND cleanup_complete=1",[id]).await?;
    }
    super::super::locations::bump_revision(connection).await?;
    Ok(Reply::Done)
}

async fn cleanup_failed(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    connection.execute("UPDATE archive_replicas SET error='Archive cleanup failed; check target availability and retry' WHERE job_id=?1 AND cancelled=1",[id]).await?;
    Ok(Reply::Done)
}

async fn action_failed(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    connection.execute("UPDATE archive_replicas SET error='Archive action failed; check target availability and retry' WHERE job_id=?1 AND action!=''",[id]).await?;
    Ok(Reply::Done)
}

async fn action_complete(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    connection.execute("UPDATE archive_replicas SET action='',action_attempts=0,error=NULL WHERE job_id=?1 AND cancelled=0",[id]).await?;
    Ok(Reply::Done)
}

async fn begin_eviction(
    connection: &turso::Connection,
    registry: &Arc<readers::Registry>,
    recording_id: &str,
    operation: &str,
) -> anyhow::Result<Reply> {
    Ok(Reply::Eviction(Box::new(
        super::super::archive_playback::begin_eviction(
            connection,
            registry,
            recording_id,
            operation,
        )
        .await?,
    )))
}

async fn finish_eviction(
    connection: &turso::Connection,
    recording_id: &str,
    operation: &str,
) -> anyhow::Result<Reply> {
    super::super::archive_playback::finish_eviction(connection, recording_id, operation).await?;
    Ok(Reply::Done)
}
