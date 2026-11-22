//! Fences the primary between conditional archive retirement and its durable receipt.

use super::*;

pub(super) async fn claim(
    connection: &turso::Connection,
    readers: &readers::Registry,
    id: &str,
    policy: &Policy,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    let work = job
        .deletion
        .as_ref()
        .context("archive deletion work unavailable")?;
    ensure!(
        matches!(work.phase.as_str(), "queued" | "claimed"),
        "archive deletion is not pending"
    );
    let mut rows = connection.query("SELECT p.snapshot FROM archive_deletion_plans p JOIN archive_deletion_items i ON i.plan_id=p.id WHERE i.job_id=?1 AND p.confirmed_ms IS NOT NULL",[id]).await?;
    let row = rows
        .next()
        .await?
        .context("archive deletion confirmation unavailable")?;
    let snapshot: Snapshot = serde_json::from_str(&row.get::<String>(0)?)?;
    drop(rows);
    let item = snapshot
        .items
        .iter()
        .find(|item| item.job_id == id && item.state == "eligible")
        .context("archive deletion selection unavailable")?;
    ensure!(
        item.version == job.version
            && item.source_identity == job.source_identity
            && item.digest == job.digest
            && item.bytes == job.bytes
            && job.local_evicted == 0,
        "archive deletion snapshot changed"
    );
    if work.phase == "queued" {
        ensure!(
            snapshot.policy == *policy,
            "archive deletion policy changed"
        );
        ensure!(
            job.state == "ready" && job.action.is_empty() && !job.cancelled,
            "archive replica changed"
        );
        let changed = connection.execute("UPDATE archive_replicas SET state='deleting',error=NULL WHERE job_id=?1 AND state='ready'",[id]).await?;
        ensure!(changed == 1, "archive retirement state changed");
        connection.execute("UPDATE archive_deletion_items SET phase='claimed' WHERE job_id=?1 AND phase='queued'",[id]).await?;
    } else {
        ensure!(job.state == "deleting", "archive deletion claim changed");
    }
    ensure!(
        eligibility::guards(connection, readers, &job, work.phase == "claimed").await?
            == "eligible",
        "archive deletion protection changed"
    );
    Ok(Reply::Done)
}

pub(super) async fn retired(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    let changed = connection.execute("UPDATE archive_replicas SET state='deleted',error=NULL WHERE job_id=?1 AND state='deleting' AND local_evicted=0
        AND EXISTS(SELECT 1 FROM archive_deletion_items WHERE job_id=?1 AND phase='claimed')",[id]).await?;
    ensure!(changed == 1, "archive deletion has no matching claim");
    connection
        .execute(
            "UPDATE archive_deletion_items SET phase='retired',attempts=0 WHERE job_id=?1",
            [id],
        )
        .await?;
    crate::storage::catalog::bump_catalog_revision(connection).await?;
    Ok(Reply::Done)
}

pub(super) async fn acknowledged(
    connection: &turso::Connection,
    id: &str,
) -> anyhow::Result<Reply> {
    let changed = connection.execute("UPDATE archive_deletion_items SET phase='acknowledged' WHERE job_id=?1 AND phase IN('retired','acknowledged')",[id]).await?;
    ensure!(changed == 1, "archive deletion receipt unavailable");
    connection
        .execute(
            "UPDATE archive_replicas SET error=NULL WHERE job_id=?1 AND state='deleted'",
            [id],
        )
        .await?;
    Ok(Reply::Done)
}

pub(super) async fn blocked(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    let changed = connection
        .execute(
            "UPDATE archive_deletion_items SET phase='blocked' WHERE job_id=?1 AND phase='queued'",
            [id],
        )
        .await?;
    ensure!(changed == 1, "archive deletion already claimed");
    connection.execute("UPDATE archive_replicas SET error='Archive deletion blocked; check the primary and preview again' WHERE job_id=?1 AND state='ready'",[id]).await?;
    Ok(Reply::Done)
}

pub(super) async fn failed(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    connection.execute("UPDATE archive_deletion_items SET attempts=attempts+1 WHERE job_id=?1 AND phase IN('claimed','retired') AND attempts<4294967295",[id]).await?;
    connection.execute("UPDATE archive_replicas SET error='Archive deletion interrupted; reconcile the confirmed job' WHERE job_id=?1 AND state IN('deleting','deleted')",[id]).await?;
    Ok(Reply::Done)
}

pub(super) async fn reverified(
    connection: &turso::Connection,
    id: &str,
    version: &str,
) -> anyhow::Result<Reply> {
    let job = required(connection, id).await?;
    ensure!(
        job.state == "ready" && job.version.as_deref() == Some(version) && job.kind != "probe",
        "archive version changed before reverification"
    );
    connection.execute("INSERT INTO archive_verified_at(job_id,version,verified_ms) VALUES(?1,?2,?3) ON CONFLICT(job_id) DO NOTHING",
        turso::params![id,version,current_unix_time_ms()]).await?;
    connection
        .execute(
            "UPDATE archive_replicas SET error=NULL WHERE job_id=?1",
            [id],
        )
        .await?;
    Ok(Reply::Done)
}
