//! Journals owned image removal before releasing its reserved capacity.

use super::super::{Kind, Location, Object, Publication, Reply, bump_revision};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retirement {
    pub operation: String,
    pub location: Location,
    pub complete: bool,
    pub acknowledged: bool,
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS storage_image_retirements (
        operation TEXT PRIMARY KEY REFERENCES storage_volume_allocations(operation),
        complete INTEGER NOT NULL DEFAULT 0 CHECK(complete IN (0,1)),
        acknowledged INTEGER NOT NULL DEFAULT 0 CHECK(acknowledged IN (0,1))
    );",
        )
        .await?;
    super::super::super::ensure_column(
        connection,
        "storage_image_retirements",
        "actual_bytes",
        "INTEGER CHECK(actual_bytes IS NULL OR actual_bytes >= 0)",
    )
    .await?;
    Ok(())
}

pub(in crate::storage::catalog::locations) async fn begin(
    connection: &turso::Connection,
    id: &str,
) -> anyhow::Result<Option<Retirement>> {
    if let Some(job) = load(connection, id).await? {
        return Ok(Some(job));
    }
    let mut pending = connection
        .query(
            "SELECT r.operation FROM storage_image_retirements r
        JOIN storage_volume_allocations a ON a.operation=r.operation
        WHERE a.kind='thumbnail' AND a.object_id=?1",
            [id],
        )
        .await?;
    if let Some(row) = pending.next().await? {
        let operation: String = row.get(0)?;
        drop(pending);
        return load(connection, &operation).await;
    }
    drop(pending);
    let mut rows = connection.query("SELECT a.operation FROM storage_volume_allocations a
        JOIN storage_volume_bindings b ON b.id=a.volume_id
        WHERE a.kind='thumbnail' AND a.object_id=?1 AND a.state='published' AND b.writable=1
        AND EXISTS(SELECT 1 FROM storage_event_images i WHERE i.object_id=a.object_id AND i.active=0)
        AND NOT EXISTS(SELECT 1 FROM storage_event_images i WHERE i.object_id=a.object_id AND i.active=1)
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.kind=a.kind AND m.object_id=a.object_id AND (m.phase NOT IN ('complete','cancelled') OR m.receipt_acknowledged=0))
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.source_operation=a.operation AND m.phase IN ('published','retiring','complete'))
        AND NOT EXISTS(SELECT 1 FROM storage_image_retirements r WHERE r.operation=a.operation)", [id]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let operation = row.get::<String>(0)?;
    drop(rows);
    connection
        .execute(
            "INSERT INTO storage_image_retirements(operation) VALUES (?1)",
            [operation.as_str()],
        )
        .await?;
    bump_revision(connection).await?;
    load(connection, &operation).await
}

pub(super) async fn load(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<Option<Retirement>> {
    let mut rows = connection.query("SELECT a.object_id,a.volume_id,a.generation,a.relative_key,a.location_revision,COALESCE(r.actual_bytes,a.bytes),a.file_identity,a.digest,r.complete,r.acknowledged,b.writable
        FROM storage_image_retirements r JOIN storage_volume_allocations a ON a.operation=r.operation JOIN storage_volume_bindings b ON b.id=a.volume_id WHERE r.operation=?1", [operation]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    anyhow::ensure!(
        row.get::<i64>(10)? != 0 || row.get::<i64>(9)? != 0,
        "image volume is not writable"
    );
    let location = Location {
        object: Object {
            kind: Kind::Thumbnail,
            id: row.get(0)?,
        },
        volume: row.get(1)?,
        generation: super::super::to_u64(row.get(2)?, "volume generation")?,
        relative_key: row.get(3)?,
        revision: super::super::to_u64(row.get(4)?, "location revision")?,
        bytes: super::super::to_u64(row.get(5)?, "image bytes")?,
        file_identity: row.get(6)?,
        digest: row
            .get::<Vec<u8>>(7)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid image digest"))?,
    };
    Ok(Some(Retirement {
        operation: operation.into(),
        location,
        complete: row.get::<i64>(8)? != 0,
        acknowledged: row.get::<i64>(9)? != 0,
    }))
}

pub(in crate::storage::catalog::locations) async fn abandon(
    connection: &turso::Connection,
    evidence: &Publication,
) -> anyhow::Result<Reply> {
    if let Some(job) = load(connection, &evidence.operation).await? {
        anyhow::ensure!(
            job.location.bytes == evidence.bytes
                && job.location.file_identity == evidence.file_identity
                && job.location.digest == evidence.digest,
            "abandoned image intent changed"
        );
        return Ok(Reply::Bound);
    }
    let mut moves = connection.query("SELECT 1 FROM storage_volume_moves WHERE source_operation=?1 OR destination_operation=?1", [evidence.operation.as_str()]).await?;
    anyhow::ensure!(
        moves.next().await?.is_none(),
        "move journal owns this allocation"
    );
    drop(moves);
    let mut rows = connection.query("SELECT state,file_identity,materialized_bytes,bytes FROM storage_volume_allocations WHERE operation=?1 AND kind='thumbnail'", [evidence.operation.as_str()]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("image allocation is missing"))?;
    // A lost publication reply does not authorize removal of a committed image.
    if row.get::<String>(0)? != "reserved" {
        return Ok(Reply::Bound);
    }
    anyhow::ensure!(
        row.get::<String>(1)? == evidence.file_identity
            && evidence.bytes >= super::super::to_u64(row.get(2)?, "materialized bytes")?
            && evidence.bytes <= super::super::to_u64(row.get(3)?, "reserved bytes")?,
        "abandoned image evidence changed"
    );
    drop(rows);
    connection.execute("UPDATE storage_volume_allocations SET file_identity=?2,digest=?3,location_revision=1 WHERE operation=?1", turso::params![evidence.operation.clone(),evidence.file_identity.clone(),evidence.digest.to_vec()]).await?;
    connection
        .execute(
            "INSERT INTO storage_image_retirements(operation,actual_bytes) VALUES (?1,?2)",
            turso::params![
                evidence.operation.clone(),
                super::super::to_i64(evidence.bytes, "image bytes")?
            ],
        )
        .await?;
    bump_revision(connection).await?;
    Ok(Reply::Bound)
}

pub(in crate::storage::catalog::locations) async fn complete(
    connection: &turso::Connection,
    evidence: &Publication,
) -> anyhow::Result<Reply> {
    let job = load(connection, &evidence.operation)
        .await?
        .ok_or_else(|| anyhow::anyhow!("image retirement is missing"))?;
    anyhow::ensure!(
        job.location.bytes == evidence.bytes
            && job.location.file_identity == evidence.file_identity
            && job.location.digest == evidence.digest,
        "image retirement evidence changed"
    );
    if !job.complete {
        connection
            .execute(
                "UPDATE storage_volume_allocations SET state='cancelled' WHERE operation=?1",
                [evidence.operation.as_str()],
            )
            .await?;
        connection
            .execute(
                "UPDATE storage_image_retirements SET complete=1 WHERE operation=?1",
                [evidence.operation.as_str()],
            )
            .await?;
        bump_revision(connection).await?;
    }
    Ok(Reply::Bound)
}

pub(in crate::storage::catalog::locations) async fn acknowledge(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<Reply> {
    let job = load(connection, operation)
        .await?
        .ok_or_else(|| anyhow::anyhow!("image retirement is missing"))?;
    anyhow::ensure!(job.complete, "image retirement is not complete");
    connection
        .execute(
            "UPDATE storage_image_retirements SET acknowledged=1 WHERE operation=?1",
            [operation],
        )
        .await?;
    Ok(Reply::Bound)
}

pub(in crate::storage::catalog) async fn ensure_not_retiring(
    connection: &turso::Connection,
    object: &Object,
) -> anyhow::Result<()> {
    if object.kind != Kind::Thumbnail {
        return Ok(());
    }
    let mut rows = connection.query("SELECT 1 FROM storage_image_retirements r JOIN storage_volume_allocations a ON a.operation=r.operation WHERE a.kind='thumbnail' AND a.object_id=?1", [object.id.as_str()]).await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "image retirement owns this object"
    );
    Ok(())
}

pub(in crate::storage::catalog::locations) async fn ensure_writable(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM storage_image_retirements WHERE operation=?1",
            [operation],
        )
        .await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "image retirement owns this allocation"
    );
    Ok(())
}
