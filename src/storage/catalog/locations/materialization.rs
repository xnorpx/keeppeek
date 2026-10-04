use super::{Reply, bump_revision, to_i64, to_u64};

/// Synchronized bytes already reflected in physical free-space observations.
#[derive(Clone)]
pub struct Materialization {
    pub operation: String,
    pub bytes: u64,
    pub file_identity: String,
}

impl std::fmt::Debug for Materialization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Materialization")
            .field("operation", &self.operation)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

pub(super) async fn migrate(connection: &turso::Connection) -> anyhow::Result<()> {
    let mut rows = connection.query(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'storage_volume_allocations'",
        (),
    ).await?;
    if rows.next().await?.is_some() {
        super::super::ensure_column(connection, "storage_volume_allocations", "materialized_bytes",
            "INTEGER NOT NULL DEFAULT 0 CHECK(typeof(materialized_bytes) = 'integer' AND materialized_bytes >= 0 AND materialized_bytes <= bytes)").await?;
    }
    Ok(())
}

pub(super) async fn checkpoint(
    connection: &turso::Connection,
    materialized: &Materialization,
) -> anyhow::Result<Reply> {
    super::moves::ensure_writable(connection, &materialized.operation).await?;
    super::images::retirement::ensure_writable(connection, &materialized.operation).await?;
    let mut rows = connection.query(
        "SELECT bytes, materialized_bytes, file_identity, state FROM storage_volume_allocations WHERE operation = ?1",
        [materialized.operation.as_str()],
    ).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("allocation does not exist"))?;
    let reserved = to_u64(row.get::<i64>(0)?, "reserved bytes")?;
    let previous = to_u64(row.get::<i64>(1)?, "materialized bytes")?;
    let identity = row.get::<Option<String>>(2)?;
    anyhow::ensure!(
        row.get::<String>(3)? == "reserved",
        "allocation is no longer pending"
    );
    anyhow::ensure!(
        materialized.bytes >= previous && materialized.bytes <= reserved,
        "materialized bytes are outside the reservation"
    );
    anyhow::ensure!(
        identity
            .as_ref()
            .is_none_or(|value| value == &materialized.file_identity),
        "allocation file identity changed"
    );
    if previous != materialized.bytes || identity.is_none() {
        connection.execute(
            "UPDATE storage_volume_allocations SET materialized_bytes = ?2, file_identity = ?3 WHERE operation = ?1",
            turso::params![materialized.operation.clone(), to_i64(materialized.bytes, "materialized bytes")?, materialized.file_identity.clone()],
        ).await?;
        bump_revision(connection).await?;
    }
    Ok(Reply::Reserved {
        operation: materialized.operation.clone(),
        bytes: reserved,
    })
}
