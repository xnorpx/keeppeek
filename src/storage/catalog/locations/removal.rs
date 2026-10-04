use super::Reply;

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS storage_volume_move_pending_roots
        ON storage_volume_moves(source_operation,destination_operation)
        WHERE phase NOT IN ('complete','cancelled') OR receipt_acknowledged=0;
        CREATE INDEX IF NOT EXISTS storage_image_retirement_pending_root
        ON storage_image_retirements(operation) WHERE acknowledged=0;
        CREATE INDEX IF NOT EXISTS storage_recording_retirement_pending_root
        ON storage_recording_retirements(operation) WHERE acknowledged=0;
        CREATE INDEX IF NOT EXISTS storage_recording_recovery_pending_root
        ON storage_recording_recovery(operation) WHERE acknowledged=0;",
        )
        .await?;
    Ok(())
}

pub(super) async fn check(connection: &turso::Connection, volume: &str) -> anyhow::Result<Reply> {
    check_archives(connection, volume).await?;
    let mut rows = connection
        .query(
            "SELECT (draining OR operator_draining),allocated_bytes,reserved_bytes
        FROM storage_volume_bindings WHERE id=?1",
            [volume],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(Reply::Bound);
    };
    anyhow::ensure!(
        row.get::<i64>(0)? != 0,
        "stop new writes before removing a bound volume"
    );
    // Every live allocation has positive bytes; the ledger includes admitted unfinished writers.
    anyhow::ensure!(
        row.get::<i64>(1)? == 0 && row.get::<i64>(2)? == 0,
        "volume still owns stored objects or admitted writes"
    );
    drop(rows);
    let mut pending = connection
        .query(include_str!("removal.sql"), [volume])
        .await?;
    anyhow::ensure!(
        pending.next().await?.is_none(),
        "volume has pending cleanup receipts"
    );
    // ponytail: removing a definition leaves immutable bindings and acknowledged history intact.
    Ok(Reply::Bound)
}

async fn check_archives(connection: &turso::Connection, volume: &str) -> anyhow::Result<()> {
    // ponytail: removal inspects at most 1024 pending policies; larger backlogs must finish first.
    const POLICIES_MAX: usize = 1024;
    let mut rows = connection
        .query(
            "SELECT q.policy FROM storage_volume_archives q
        JOIN storage_volume_allocations a ON a.operation=q.operation
        WHERE q.done=0 AND a.state!='cancelled' LIMIT 1025",
            (),
        )
        .await?;
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        anyhow::ensure!(
            count < POLICIES_MAX,
            "finish the archive backlog before removal"
        );
        count += 1;
        let policy: super::archives::Policy = serde_json::from_str(&row.get::<String>(0)?)?;
        anyhow::ensure!(
            !policy.configuration.placement.iter().any(|rule| {
                rule.candidates
                    .iter()
                    .any(|candidate| candidate.as_str() == volume)
            }),
            "volume is referenced by a pending archive policy"
        );
    }
    Ok(())
}
