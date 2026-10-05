//! Releases an interrupted writer only after its owned file retirement is durable.

use super::{Pending, Publication, Reply, bump_revision, to_u64};

pub(super) async fn replay_complete(
    connection: &turso::Connection,
    evidence: &Publication,
) -> anyhow::Result<bool> {
    let mut rows = connection.query("SELECT retained_bytes,file_identity,digest FROM storage_recording_recovery WHERE operation=?1 AND mode='abandon' AND complete=1", [evidence.operation.as_str()]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    anyhow::ensure!(
        to_u64(row.get(0)?, "abandoned recording bytes")? == evidence.bytes
            && row.get::<String>(1)? == evidence.file_identity
            && row.get::<Vec<u8>>(2)? == evidence.digest,
        "recording abandonment evidence changed"
    );
    Ok(true)
}

pub(super) async fn complete(
    connection: &turso::Connection,
    pending: &Pending,
) -> anyhow::Result<Reply> {
    connection
        .execute(
            "UPDATE storage_recording_recovery SET complete=1 WHERE operation=?1",
            [pending.owned.operation.as_str()],
        )
        .await?;
    connection
        .execute(
            "UPDATE storage_volume_allocations SET state='cancelled' WHERE operation=?1",
            [pending.owned.operation.as_str()],
        )
        .await?;
    connection
        .execute(
            "UPDATE storage_volume_archives SET done=1 WHERE operation=?1",
            [pending.owned.operation.as_str()],
        )
        .await?;
    connection
        .execute(
            "DELETE FROM recording_files WHERE id=?1",
            [pending.recording.as_str()],
        )
        .await?;
    bump_revision(connection).await?;
    Ok(Reply::Bound)
}

pub(super) async fn acknowledge(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<Reply> {
    let mut rows = connection
        .query(
            "SELECT complete FROM storage_recording_recovery WHERE operation=?1 AND mode='abandon'",
            [operation],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording abandonment is missing"))?;
    anyhow::ensure!(
        row.get::<i64>(0)? == 1,
        "recording abandonment is not complete"
    );
    drop(rows);
    connection
        .execute(
            "UPDATE storage_recording_recovery SET acknowledged=1 WHERE operation=?1",
            [operation],
        )
        .await?;
    Ok(Reply::Bound)
}
