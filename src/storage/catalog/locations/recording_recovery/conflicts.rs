//! Protects other catalog identities that name a recovery source.

use super::{Mode, Pending, Plan};

pub(super) async fn check(
    connection: &turso::Connection,
    pending: &Pending,
    plan: &Plan,
) -> anyhow::Result<()> {
    if plan.mode == Mode::Seal && plan.original_bytes == plan.evidence.bytes {
        return Ok(());
    }
    let mut rows = connection.query("SELECT 1 FROM recording_files WHERE id!=?1 AND replace(path,char(92),'/')=replace(?2,char(92),'/') COLLATE NOCASE LIMIT 1",
        turso::params![pending.recording.clone(), pending.path.clone()]).await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "another recording references the recovery file"
    );
    Ok(())
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection.execute_batch("CREATE INDEX IF NOT EXISTS storage_recording_normalized_path
        ON recording_files(replace(path,char(92),'/') COLLATE NOCASE);
        CREATE INDEX IF NOT EXISTS storage_recording_recovery_active ON storage_recording_recovery(complete);
        CREATE INDEX IF NOT EXISTS storage_recording_recovery_ack ON storage_recording_recovery(operation) WHERE mode='abandon' AND acknowledged=0;
        CREATE TRIGGER IF NOT EXISTS storage_recording_recovery_alias_insert
        BEFORE INSERT ON recording_files WHEN EXISTS(
            SELECT 1 FROM storage_recording_recovery q JOIN storage_volume_allocations a ON a.operation=q.operation
            WHERE q.complete=0 AND (q.mode='abandon' OR q.retained_bytes<q.original_bytes)
            AND a.destination_path=replace(NEW.path,char(92),'/') COLLATE NOCASE)
        BEGIN SELECT RAISE(ABORT,'recording recovery owns this path'); END;
        CREATE TRIGGER IF NOT EXISTS storage_recording_recovery_alias_update
        BEFORE UPDATE OF path ON recording_files WHEN EXISTS(
            SELECT 1 FROM storage_recording_recovery q JOIN storage_volume_allocations a ON a.operation=q.operation
            WHERE q.complete=0 AND (q.mode='abandon' OR q.retained_bytes<q.original_bytes)
            AND a.destination_path=replace(NEW.path,char(92),'/') COLLATE NOCASE)
        BEGIN SELECT RAISE(ABORT,'recording recovery owns this path'); END;").await?;
    Ok(())
}
