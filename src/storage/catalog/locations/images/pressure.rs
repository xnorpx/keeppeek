//! Admits one current image for volume-local pressure without erasing event metadata.

use super::super::{Location, bump_revision};

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TRIGGER IF NOT EXISTS storage_image_retirement_protection_fence
        BEFORE UPDATE OF protected ON recording_files WHEN NEW.protected=1 AND OLD.protected=0
        AND EXISTS(SELECT 1 FROM recording_event_keyframes k
            JOIN storage_event_images i ON i.event_id=k.event_id AND i.active=1
            JOIN storage_volume_allocations a ON a.kind='thumbnail' AND a.object_id=i.object_id
            JOIN storage_image_retirements r ON r.operation=a.operation
            WHERE k.recording_id=OLD.id AND r.complete=0)
        BEGIN SELECT RAISE(ABORT,'image retirement owns this evidence'); END;
        CREATE TRIGGER IF NOT EXISTS storage_image_retirement_link_insert_fence
        BEFORE INSERT ON recording_event_keyframes
        WHEN EXISTS(SELECT 1 FROM recording_files f
            JOIN storage_event_images i ON i.event_id=NEW.event_id AND i.active=1
            JOIN storage_volume_allocations a ON a.kind='thumbnail' AND a.object_id=i.object_id
            JOIN storage_image_retirements r ON r.operation=a.operation
            WHERE f.id=NEW.recording_id AND f.protected=1 AND r.complete=0)
        BEGIN SELECT RAISE(ABORT,'image retirement owns this evidence'); END;
        CREATE TRIGGER IF NOT EXISTS storage_image_retirement_link_update_fence
        BEFORE UPDATE ON recording_event_keyframes
        WHEN EXISTS(SELECT 1 FROM recording_files f
            JOIN storage_event_images i ON i.event_id=NEW.event_id AND i.active=1
            JOIN storage_volume_allocations a ON a.kind='thumbnail' AND a.object_id=i.object_id
            JOIN storage_image_retirements r ON r.operation=a.operation
            WHERE f.id=NEW.recording_id AND f.protected=1 AND r.complete=0)
        BEGIN SELECT RAISE(ABORT,'image retirement owns this evidence'); END;",
        )
        .await?;
    Ok(())
}

pub(in crate::storage::catalog::locations) async fn begin(
    connection: &turso::Connection,
    volume: &str,
) -> anyhow::Result<bool> {
    let mut pending = connection
        .query(
            "SELECT 1 FROM storage_image_retirements r
        JOIN storage_volume_allocations a ON a.operation=r.operation
        WHERE a.volume_id=?1 AND r.acknowledged=0 LIMIT 1",
            [volume],
        )
        .await?;
    if pending.next().await?.is_some() {
        return Ok(true);
    }
    drop(pending);
    let mut rows = connection.query("SELECT a.operation FROM storage_volume_allocations a
        JOIN storage_volume_bindings b ON b.id=a.volume_id
        JOIN storage_event_images i ON i.object_id=a.object_id AND i.active=1
        JOIN recording_events e ON e.id=i.event_id
        WHERE a.volume_id=?1 AND a.kind='thumbnail' AND a.state='published' AND b.writable=1
        AND NOT EXISTS(SELECT 1 FROM storage_image_retirements r WHERE r.operation=a.operation)
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.kind='thumbnail' AND m.object_id=a.object_id
            AND (m.phase NOT IN ('complete','cancelled') OR m.receipt_acknowledged=0))
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.source_operation=a.operation AND m.phase IN ('published','retiring','complete'))
        AND NOT EXISTS(SELECT 1 FROM recording_event_keyframes k JOIN recording_files f ON f.id=k.recording_id
            WHERE k.event_id=e.id AND f.protected=1)
        ORDER BY e.start_time_ms,e.id,i.attachment_id LIMIT 1", [volume]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    let operation: String = row.get(0)?;
    drop(rows);
    connection
        .execute(
            "INSERT INTO storage_image_retirements(operation) VALUES (?1)",
            [operation],
        )
        .await?;
    bump_revision(connection).await?;
    Ok(true)
}

pub(super) async fn retired_location(
    connection: &turso::Connection,
    object: &str,
) -> anyhow::Result<Option<Location>> {
    let mut rows = connection
        .query(
            "SELECT r.operation FROM storage_image_retirements r
        JOIN storage_volume_allocations a ON a.operation=r.operation
        WHERE a.kind='thumbnail' AND a.object_id=?1",
            [object],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let operation: String = row.get(0)?;
    drop(rows);
    Ok(super::retirement::load(connection, &operation)
        .await?
        .map(|job| job.location))
}
