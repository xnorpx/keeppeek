//! Finds interrupted image allocations without adopting untracked files.

use super::super::{Reply, bump_revision, export_cleanup::Owned, to_u64};

pub(in crate::storage::catalog::locations) async fn pending(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<Option<Owned>> {
    let mut rows = connection.query("SELECT a.volume_id,a.generation,a.relative_key,a.bytes,a.materialized_bytes,a.file_identity
        FROM storage_volume_allocations a WHERE a.operation=?1 AND a.kind='thumbnail' AND a.state='reserved'
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.destination_operation=a.operation)
        AND NOT EXISTS(SELECT 1 FROM storage_image_retirements r WHERE r.operation=a.operation)", [operation]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(Owned {
        operation: operation.into(),
        volume: row.get(0)?,
        generation: to_u64(row.get(1)?, "image volume generation")?,
        relative_key: row.get(2)?,
        bytes: to_u64(row.get(3)?, "reserved image bytes")?,
        materialized_bytes: to_u64(row.get(4)?, "materialized image bytes")?,
        file_identity: row.get(5)?,
        digest: None,
    }))
}

pub(in crate::storage::catalog::locations) async fn empty(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<Reply> {
    let Some(pending) = pending(connection, operation).await? else {
        return Ok(Reply::Bound);
    };
    anyhow::ensure!(
        pending.file_identity.is_none(),
        "image file was materialized"
    );
    connection
        .execute(
            "UPDATE storage_volume_allocations SET state='cancelled' WHERE operation=?1",
            [operation],
        )
        .await?;
    bump_revision(connection).await?;
    Ok(Reply::Bound)
}
