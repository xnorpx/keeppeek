//! Keeps export cancellation authoritative even when a worker has not reserved its file.

use super::moves::Cancellation;
use super::{Kind, Object, Reply, bump_revision};

mod journal;
use journal::{load, verify};

pub(in crate::storage::catalog) async fn readable(
    connection: &turso::Connection,
    id: &str,
) -> anyhow::Result<Option<super::Location>> {
    validate_id(id)?;
    let object = Object {
        kind: Kind::Export,
        id: id.into(),
    };
    ensure_active(connection, &object).await?;
    let Reply::Location(location) = super::ownership::lookup(connection, &object).await? else {
        anyhow::bail!("invalid export location reply");
    };
    Ok(location)
}

#[derive(Debug, Clone)]
pub enum Action {
    Load(String),
    Verify(String, Cancellation),
    Complete(String),
    Acknowledge(String),
}

impl Action {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        if let Self::Load(id) = self {
            return super::identifier(id);
        }
        let id = match self {
            Self::Load(id) | Self::Verify(id, _) | Self::Complete(id) | Self::Acknowledge(id) => id,
        };
        validate_id(id)?;
        if let Self::Verify(_, evidence) = self {
            anyhow::ensure!(
                serde_json::to_string(evidence)?.len() <= 2048,
                "export evidence is too large"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub allocation: Option<Owned>,
    pub evidence: Option<Cancellation>,
    pub complete: bool,
    pub acknowledged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owned {
    pub operation: String,
    pub volume: String,
    pub generation: u64,
    pub relative_key: String,
    pub bytes: u64,
    pub materialized_bytes: u64,
    pub file_identity: Option<String>,
    pub digest: Option<[u8; 32]>,
}

pub(super) fn validate_id(id: &str) -> anyhow::Result<()> {
    let parsed = uuid::Uuid::parse_str(id)?;
    anyhow::ensure!(
        parsed.to_string() == id || parsed.simple().to_string() == id,
        "invalid export artifact identifier"
    );
    Ok(())
}

pub(super) async fn dispatch(
    connection: &turso::Connection,
    action: Action,
) -> anyhow::Result<Reply> {
    match action {
        Action::Load(id) => {
            return Ok(Reply::ExportCleanup(
                load(connection, &id).await?.map(Box::new),
            ));
        }
        Action::Verify(id, evidence) => verify(connection, &id, &evidence).await?,
        Action::Complete(id) => complete(connection, &id).await?,
        Action::Acknowledge(id) => {
            let job = load(connection, &id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("export cleanup missing"))?;
            anyhow::ensure!(job.complete, "export cleanup is not complete");
            connection
                .execute(
                    "UPDATE storage_export_cleanup SET acknowledged=1 WHERE object_id=?1",
                    [id],
                )
                .await?;
        }
    }
    Ok(Reply::Bound)
}

async fn complete(connection: &turso::Connection, id: &str) -> anyhow::Result<()> {
    let job = load(connection, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("export cleanup missing"))?;
    if job.complete {
        return Ok(());
    }
    anyhow::ensure!(job.evidence.is_some(), "export removal evidence missing");
    if let Some(allocation) = job.allocation {
        connection
            .execute(
                "UPDATE storage_volume_allocations SET state='cancelled' WHERE operation=?1",
                [allocation.operation],
            )
            .await?;
    }
    connection
        .execute(
            "UPDATE storage_export_cleanup SET complete=1 WHERE object_id=?1",
            [id],
        )
        .await?;
    bump_revision(connection).await
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS storage_export_cleanup (
        object_id TEXT PRIMARY KEY,
        evidence TEXT,
        complete INTEGER NOT NULL DEFAULT 0 CHECK(complete IN (0,1)),
        acknowledged INTEGER NOT NULL DEFAULT 0 CHECK(acknowledged IN (0,1))
    );
    CREATE INDEX IF NOT EXISTS storage_export_cleanup_pending ON storage_export_cleanup(acknowledged,object_id);").await?;
    Ok(())
}

pub(super) async fn retire(connection: &turso::Connection, id: &str) -> anyhow::Result<Reply> {
    let mut existing = connection
        .query(
            "SELECT 1 FROM storage_export_cleanup WHERE object_id=?1",
            [id],
        )
        .await?;
    if existing.next().await?.is_some() {
        return Ok(Reply::Bound);
    }
    drop(existing);
    let mut pending = connection
        .query(
            "SELECT COUNT(*) FROM storage_export_cleanup WHERE acknowledged=0",
            (),
        )
        .await?;
    anyhow::ensure!(
        pending
            .next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing export cleanup count"))?
            .get::<i64>(0)?
            < 4096,
        "export cleanup queue is full"
    );
    drop(pending);
    connection
        .execute(
            "INSERT INTO storage_export_cleanup(object_id) VALUES (?1)",
            [id],
        )
        .await?;
    bump_revision(connection).await?;
    Ok(Reply::Bound)
}

pub(super) async fn ensure_active(
    connection: &turso::Connection,
    object: &Object,
) -> anyhow::Result<()> {
    if object.kind != Kind::Export {
        return Ok(());
    }
    let mut rows = connection
        .query(
            "SELECT 1 FROM storage_export_cleanup WHERE object_id=?1",
            [object.id.as_str()],
        )
        .await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "export retirement owns this object"
    );
    Ok(())
}

pub(super) async fn ensure_active_operation(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<()> {
    // An admitted move must finish before cleanup can inspect its final allocation.
    let mut rows = connection.query("SELECT 1 FROM storage_volume_allocations a JOIN storage_export_cleanup e ON e.object_id=a.object_id WHERE a.operation=?1 AND a.kind='export' AND NOT EXISTS(SELECT 1 FROM storage_volume_moves m WHERE m.destination_operation=a.operation)", [operation]).await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "export retirement owns this allocation"
    );
    Ok(())
}

pub(super) async fn ensure_identity_capture(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<()> {
    // Cancellation may race file creation. Preserve its identity while the worker lease is held.
    let mut rows = connection.query("SELECT 1 FROM storage_volume_allocations a JOIN storage_export_cleanup e ON e.object_id=a.object_id WHERE a.operation=?1 AND a.kind='export' AND e.evidence IS NOT NULL", [operation]).await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "export cleanup evidence is already captured"
    );
    Ok(())
}
