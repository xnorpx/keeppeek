use super::{
    Allocation, Growth, MAX_BINDINGS, OBSERVATION_LIFETIME, Object, Reply, Usage, admission,
    bump_revision, ownership, revision, to_i64, to_u64,
};
use std::time::Instant;

pub(super) async fn grow(connection: &turso::Connection, growth: &Growth) -> anyhow::Result<Reply> {
    super::moves::ensure_writable(connection, &growth.operation).await?;
    super::images::retirement::ensure_writable(connection, &growth.operation).await?;
    super::export_cleanup::ensure_active_operation(connection, &growth.operation).await?;
    let mut rows = connection.query("SELECT kind, object_id, volume_id, generation, relative_key, bytes, state FROM storage_volume_allocations WHERE operation = ?1", [growth.operation.as_str()]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("allocation does not exist"))?;
    anyhow::ensure!(
        row.get::<String>(6)? == "reserved",
        "allocation is no longer pending"
    );
    let current = to_u64(row.get::<i64>(5)?, "allocation bytes")?;
    if growth.bytes <= current {
        return Ok(Reply::Reserved {
            operation: growth.operation.clone(),
            bytes: current,
        });
    }
    let capacity = &growth.capacity;
    anyhow::ensure!(
        capacity.observed_at <= Instant::now()
            && capacity.observed_at.elapsed() <= OBSERVATION_LIFETIME,
        "volume observation expired"
    );
    anyhow::ensure!(
        capacity.ledger_revision == revision(connection).await?,
        "volume observation superseded"
    );
    let allocation = Allocation {
        operation: growth.operation.clone(),
        object: Object {
            kind: ownership::parse_kind(&row.get::<String>(0)?)?,
            id: row.get::<String>(1)?,
        },
        volume: row.get::<String>(2)?,
        generation: to_u64(row.get::<i64>(3)?, "volume generation")?,
        relative_key: row.get::<String>(4)?,
        bytes: growth.bytes - current,
        capacity: capacity.clone(),
    };
    admission(connection, &allocation, false).await?;
    connection.execute("UPDATE storage_volume_allocations SET bytes = ?2 WHERE operation = ?1 AND state = 'reserved'", turso::params![growth.operation.clone(), to_i64(growth.bytes, "allocation bytes")?]).await?;
    bump_revision(connection).await?;
    Ok(Reply::Reserved {
        operation: growth.operation.clone(),
        bytes: growth.bytes,
    })
}

pub(super) async fn usage(connection: &turso::Connection) -> anyhow::Result<Reply> {
    let mut rows = connection.query("SELECT id, allocated_bytes, reserved_bytes, filesystem FROM storage_volume_bindings ORDER BY id LIMIT ?1", [MAX_BINDINGS + 1]).await?;
    let mut usage = Vec::new();
    while let Some(row) = rows.next().await? {
        anyhow::ensure!(
            usage.len() < usize::try_from(MAX_BINDINGS)?,
            "volume binding limit exceeded"
        );
        usage.push(Usage {
            volume: row.get::<String>(0)?,
            filesystem: row.get::<String>(3)?,
            allocated_bytes: to_u64(row.get::<i64>(1)?, "allocated bytes")?,
            reserved_bytes: to_u64(row.get::<i64>(2)?, "reserved bytes")?,
        });
    }
    Ok(Reply::Usage(usage))
}
