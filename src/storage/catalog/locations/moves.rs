//! Reserves a second copy without changing the readable object location.

use super::{
    Allocation, Location, Object, Publication, Reply, bump_revision, ownership, reserve, to_i64,
};

mod cancellation;
pub use cancellation::Cancellation;

/// A bounded scan ordered by immutable job ID. Use the final ID as the next cursor.
#[derive(Debug, Clone)]
pub struct Page {
    pub after: Option<String>,
    pub limit: u16,
    pub include_terminal: bool,
}

pub(super) async fn find(
    connection: &turso::Connection,
    id: &str,
) -> anyhow::Result<Option<Box<Job>>> {
    let mut rows = connection
        .query("SELECT 1 FROM storage_volume_moves WHERE id = ?1", [id])
        .await?;
    let exists = rows.next().await?.is_some();
    drop(rows);
    if exists {
        Ok(Some(Box::new(load(connection, id).await?)))
    } else {
        Ok(None)
    }
}

pub(super) async fn page(connection: &turso::Connection, page: &Page) -> anyhow::Result<Vec<Job>> {
    let mut rows = connection
        .query(
            "SELECT id FROM storage_volume_moves WHERE (?1 IS NULL OR id > ?1)
         AND (?2 = 1 OR phase NOT IN ('complete','cancelled') OR receipt_acknowledged = 0) ORDER BY id LIMIT ?3",
            turso::params![
                page.after.clone(),
                i64::from(page.include_terminal),
                i64::from(page.limit)
            ],
        )
        .await?;
    let mut ids = Vec::with_capacity(usize::from(page.limit));
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    drop(rows);
    let mut jobs = Vec::with_capacity(ids.len());
    for id in ids {
        jobs.push(load(connection, &id).await?);
    }
    Ok(jobs)
}

#[derive(Debug, Clone)]
pub enum Step {
    Verified(Publication),
    FilePublished(String),
    Publish(String),
    Cancel(String),
    Retiring(String),
    Retired(Publication),
    Acknowledged(String),
    CancellationVerified { id: String, evidence: Cancellation },
    Cancelled(String),
}

impl Step {
    pub(super) fn id(&self) -> &str {
        match self {
            Self::Verified(evidence) | Self::Retired(evidence) => &evidence.operation,
            Self::CancellationVerified { id, .. } | Self::Cancelled(id) => id,
            Self::FilePublished(id)
            | Self::Publish(id)
            | Self::Cancel(id)
            | Self::Retiring(id)
            | Self::Acknowledged(id) => id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Intent {
    pub id: String,
    pub object: Object,
    pub expected_revision: u64,
    pub destination: Allocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub object: Object,
    pub source: Location,
    pub destination_operation: String,
    pub destination: Destination,
    pub phase: String,
    pub cancellation_requested: bool,
    pub receipt_acknowledged: bool,
    pub cancellation: Option<Cancellation>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Destination {
    pub volume: String,
    pub generation: u64,
    pub relative_key: String,
    pub bytes: u64,
    pub materialized_bytes: u64,
    pub file_identity: Option<String>,
}

impl std::fmt::Debug for Destination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Destination")
            .field("volume", &self.volume)
            .field("bytes", &self.bytes)
            .field("materialized_bytes", &self.materialized_bytes)
            .finish_non_exhaustive()
    }
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS storage_volume_moves (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        object_id TEXT NOT NULL,
        source_operation TEXT NOT NULL,
        destination_operation TEXT NOT NULL UNIQUE,
        source_revision INTEGER NOT NULL CHECK(source_revision > 0),
        cancellation_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancellation_requested IN (0,1)),
        receipt_acknowledged INTEGER NOT NULL DEFAULT 0 CHECK(receipt_acknowledged IN (0,1)),
        cancellation TEXT CHECK(cancellation IS NULL OR length(cancellation) <= 2048),
        phase TEXT NOT NULL CHECK(phase IN ('reserved','verified','file_published','published','retiring','complete','cancelled')),
        FOREIGN KEY(source_operation) REFERENCES storage_volume_allocations(operation),
        FOREIGN KEY(destination_operation) REFERENCES storage_volume_allocations(operation)
    );
    CREATE UNIQUE INDEX IF NOT EXISTS storage_volume_move_object ON storage_volume_moves(kind,object_id)
        WHERE phase NOT IN ('complete','cancelled');").await?;
    super::super::ensure_column(
        connection,
        "storage_volume_moves",
        "cancellation_requested",
        "INTEGER NOT NULL DEFAULT 0 CHECK(cancellation_requested IN (0,1))",
    )
    .await?;
    super::super::ensure_column(
        connection,
        "storage_volume_moves",
        "receipt_acknowledged",
        "INTEGER NOT NULL DEFAULT 0 CHECK(receipt_acknowledged IN (0,1))",
    )
    .await?;
    super::super::ensure_column(
        connection,
        "storage_volume_moves",
        "cancellation",
        "TEXT CHECK(cancellation IS NULL OR length(cancellation) <= 2048)",
    )
    .await?;
    Ok(())
}

pub(super) async fn begin(connection: &turso::Connection, intent: &Intent) -> anyhow::Result<Job> {
    super::recordings::ensure_active(connection, &intent.object).await?;
    super::images::retirement::ensure_not_retiring(connection, &intent.object).await?;
    super::export_cleanup::ensure_active(connection, &intent.object).await?;
    let mut existing = connection.query("SELECT kind, object_id, source_revision, destination_operation FROM storage_volume_moves WHERE id = ?1", [intent.id.as_str()]).await?;
    if let Some(row) = existing.next().await? {
        anyhow::ensure!(
            row.get::<String>(0)? == intent.object.kind.as_str()
                && row.get::<String>(1)? == intent.object.id
                && row.get::<i64>(2)? == to_i64(intent.expected_revision, "source revision")?
                && row.get::<String>(3)? == intent.destination.operation,
            "move intent changed"
        );
        validate_destination_intent(connection, &intent.destination).await?;
        return load(connection, &intent.id).await;
    }
    let mut destinations = connection
        .query(
            "SELECT 1 FROM storage_volume_allocations WHERE operation = ?1",
            [intent.destination.operation.as_str()],
        )
        .await?;
    anyhow::ensure!(
        destinations.next().await?.is_none(),
        "move destination belongs to another operation"
    );
    drop(destinations);
    let Reply::Location(Some(source)) = ownership::lookup(connection, &intent.object).await? else {
        anyhow::bail!("move source has no authoritative location");
    };
    anyhow::ensure!(
        source.revision == intent.expected_revision,
        "move source revision changed"
    );
    anyhow::ensure!(
        source.volume != intent.destination.volume,
        "move destination is already authoritative"
    );
    anyhow::ensure!(
        intent.destination.bytes >= source.bytes,
        "move reservation is too small"
    );
    admit_job(connection).await?;
    reserve(connection, &intent.destination).await?;
    connection.execute("INSERT INTO storage_volume_moves(id,kind,object_id,source_operation,destination_operation,source_revision,phase)
        SELECT ?1,kind,object_id,operation,?2,location_revision,'reserved' FROM storage_volume_allocations
        WHERE kind = ?3 AND object_id = ?4 AND state = 'published'",
        turso::params![intent.id.clone(), intent.destination.operation.clone(), intent.object.kind.as_str(), intent.object.id.clone()]).await?;
    bump_revision(connection).await?;
    load(connection, &intent.id).await
}

async fn admit_job(connection: &turso::Connection) -> anyhow::Result<()> {
    // ponytail: retain 1024 terminal receipts; active jobs have a separate bounded budget.
    connection.execute("DELETE FROM storage_volume_moves WHERE phase IN ('complete','cancelled') AND receipt_acknowledged = 1 AND rowid NOT IN (SELECT rowid FROM storage_volume_moves WHERE phase IN ('complete','cancelled') ORDER BY rowid DESC LIMIT 1024)", ()).await?;
    let mut counts = connection
        .query(
            "SELECT COUNT(*) FROM storage_volume_moves WHERE phase NOT IN ('complete','cancelled') OR receipt_acknowledged = 0",
            (),
        )
        .await?;
    anyhow::ensure!(
        counts.next().await?.expect("count row").get::<i64>(0)? < 4_096,
        "active move journal limit reached"
    );
    Ok(())
}

pub(super) async fn ensure_writable(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<()> {
    let mut rows = connection.query("SELECT 1 FROM storage_volume_moves WHERE destination_operation = ?1 AND (phase != 'reserved' OR cancellation_requested = 1)", [operation]).await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "verified move copies cannot be changed"
    );
    Ok(())
}

async fn validate_destination_intent(
    connection: &turso::Connection,
    destination: &Allocation,
) -> anyhow::Result<()> {
    let mut rows = connection.query("SELECT volume_id,generation,relative_key,intent_bytes FROM storage_volume_allocations WHERE operation = ?1", [destination.operation.as_str()]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("move reservation missing"))?;
    anyhow::ensure!(
        row.get::<String>(0)? == destination.volume
            && row.get::<i64>(1)? == to_i64(destination.generation, "generation")?
            && row.get::<String>(2)? == destination.relative_key
            && row.get::<i64>(3)? == to_i64(destination.bytes, "intent bytes")?,
        "move destination intent changed"
    );
    Ok(())
}

pub(super) async fn load(connection: &turso::Connection, id: &str) -> anyhow::Result<Job> {
    let mut rows = connection.query("SELECT m.kind,m.object_id,m.destination_operation,m.phase,
        a.volume_id,a.generation,a.relative_key,m.source_revision,a.bytes,a.file_identity,a.digest,
        d.volume_id,d.generation,d.relative_key,d.bytes,d.materialized_bytes,d.file_identity,m.cancellation_requested,m.receipt_acknowledged,m.cancellation
        FROM storage_volume_moves m JOIN storage_volume_allocations a ON a.operation = m.source_operation
        JOIN storage_volume_allocations d ON d.operation = m.destination_operation WHERE m.id = ?1", [id]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("move does not exist"))?;
    let object = Object {
        kind: ownership::parse_kind(&row.get::<String>(0)?)?,
        id: row.get::<String>(1)?,
    };
    Ok(Job {
        id: id.to_owned(),
        object: object.clone(),
        destination_operation: row.get::<String>(2)?,
        destination: Destination {
            volume: row.get::<String>(11)?,
            generation: super::to_u64(row.get::<i64>(12)?, "destination generation")?,
            relative_key: row.get::<String>(13)?,
            bytes: super::to_u64(row.get::<i64>(14)?, "destination bytes")?,
            materialized_bytes: super::to_u64(row.get::<i64>(15)?, "materialized bytes")?,
            file_identity: row.get::<Option<String>>(16)?,
        },
        phase: row.get::<String>(3)?,
        cancellation_requested: row.get::<i64>(17)? == 1,
        receipt_acknowledged: row.get::<i64>(18)? == 1,
        cancellation: row
            .get::<Option<String>>(19)?
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        source: Location {
            object,
            volume: row.get::<String>(4)?,
            generation: super::to_u64(row.get::<i64>(5)?, "generation")?,
            relative_key: row.get::<String>(6)?,
            revision: super::to_u64(row.get::<i64>(7)?, "revision")?,
            bytes: super::to_u64(row.get::<i64>(8)?, "source bytes")?,
            file_identity: row.get::<String>(9)?,
            digest: row
                .get::<Vec<u8>>(10)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid source digest"))?,
        },
    })
}

pub(super) async fn advance(connection: &turso::Connection, step: &Step) -> anyhow::Result<Job> {
    let job = load(connection, step.id()).await?;
    anyhow::ensure!(
        !job.cancellation_requested
            || matches!(
                step,
                Step::Cancel(_)
                    | Step::CancellationVerified { .. }
                    | Step::Cancelled(_)
                    | Step::Acknowledged(_)
            ),
        "move cancellation is pending"
    );
    match step {
        Step::Verified(evidence) => verify(connection, &job, evidence).await?,
        Step::FilePublished(_) => {
            transition(connection, &job, "verified", "file_published").await?;
        }
        Step::Publish(_) => publish(connection, &job).await?,
        Step::Retiring(_) => {
            verify_authority(connection, &job).await?;
            transition(connection, &job, "published", "retiring").await?;
        }
        Step::Retired(evidence) => retire(connection, &job, evidence).await?,
        Step::CancellationVerified { evidence, .. } => {
            cancellation::verify(connection, &job, evidence).await?;
        }
        Step::Cancelled(_) => cancellation::complete(connection, &job).await?,
        Step::Acknowledged(_) => acknowledge(connection, &job).await?,
        Step::Cancel(_) => cancel(connection, &job).await?,
    }
    load(connection, step.id()).await
}

async fn acknowledge(connection: &turso::Connection, job: &Job) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(job.phase.as_str(), "complete" | "cancelled"),
        "move cleanup is not complete"
    );
    if !job.receipt_acknowledged {
        connection
            .execute(
                "UPDATE storage_volume_moves SET receipt_acknowledged = 1 WHERE id = ?1",
                [job.id.as_str()],
            )
            .await?;
        bump_revision(connection).await?;
    }
    Ok(())
}

async fn cancel(connection: &turso::Connection, job: &Job) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(
            job.phase.as_str(),
            "reserved" | "verified" | "file_published"
        ),
        "published moves cannot be cancelled"
    );
    if !job.cancellation_requested {
        connection
            .execute(
                "UPDATE storage_volume_moves SET cancellation_requested = 1 WHERE id = ?1",
                [job.id.as_str()],
            )
            .await?;
        bump_revision(connection).await?;
    }
    Ok(())
}

async fn verify_authority(connection: &turso::Connection, job: &Job) -> anyhow::Result<()> {
    let Reply::Location(Some(current)) = ownership::lookup(connection, &job.object).await? else {
        anyhow::bail!("move destination is no longer authoritative");
    };
    anyhow::ensure!(
        current.volume == job.destination.volume
            && current.generation == job.destination.generation
            && current.relative_key == job.destination.relative_key
            && Some(&current.file_identity) == job.destination.file_identity.as_ref()
            && current.bytes == job.source.bytes
            && current.digest == job.source.digest
            && job.source.revision.checked_add(1) == Some(current.revision),
        "move destination authority changed"
    );
    Ok(())
}

async fn retire(
    connection: &turso::Connection,
    job: &Job,
    evidence: &Publication,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        evidence.bytes == job.source.bytes
            && evidence.digest == job.source.digest
            && evidence.file_identity == job.source.file_identity,
        "retired source evidence changed"
    );
    if job.phase == "complete" {
        return Ok(());
    }
    anyhow::ensure!(
        job.phase == "retiring",
        "move source retirement was not authorized"
    );
    verify_authority(connection, job).await?;
    let changed = connection
        .execute(
            "UPDATE storage_volume_allocations SET state = 'cancelled' WHERE operation =
         (SELECT source_operation FROM storage_volume_moves WHERE id = ?1) AND state = 'published'",
            [job.id.as_str()],
        )
        .await?;
    anyhow::ensure!(changed == 1, "source ownership changed during retirement");
    transition(connection, job, "retiring", "complete").await
}

async fn transition(
    connection: &turso::Connection,
    job: &Job,
    before: &str,
    after: &str,
) -> anyhow::Result<()> {
    if job.phase == after {
        return Ok(());
    }
    anyhow::ensure!(job.phase == before, "move phase changed");
    let changed = connection
        .execute(
            "UPDATE storage_volume_moves SET phase = ?2 WHERE id = ?1 AND phase = ?3",
            turso::params![job.id.clone(), after, before],
        )
        .await?;
    anyhow::ensure!(changed == 1, "move transition was refused");
    bump_revision(connection).await
}

async fn verify(
    connection: &turso::Connection,
    job: &Job,
    evidence: &Publication,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        evidence.bytes == job.source.bytes && evidence.digest == job.source.digest,
        "move copy does not match the authoritative source"
    );
    anyhow::ensure!(
        matches!(job.phase.as_str(), "reserved" | "verified"),
        "move cannot accept copy evidence"
    );
    let mut rows = connection.query("SELECT file_identity,digest,materialized_bytes,bytes FROM storage_volume_allocations WHERE operation = ?1 AND state = 'reserved'", [job.destination_operation.as_str()]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("move destination is not reserved"))?;
    anyhow::ensure!(
        row.get::<Option<String>>(0)?
            .is_none_or(|identity| identity == evidence.file_identity),
        "move copy identity changed"
    );
    if job.phase == "verified" {
        anyhow::ensure!(
            row.get::<Vec<u8>>(1)? == evidence.digest,
            "move evidence changed"
        );
        return Ok(());
    }
    anyhow::ensure!(
        super::to_u64(row.get::<i64>(3)?, "reservation bytes")? >= evidence.bytes,
        "move copy exceeds reservation"
    );
    connection.execute("UPDATE storage_volume_allocations SET file_identity = ?2, digest = ?3, materialized_bytes = ?4 WHERE operation = ?1",
        turso::params![job.destination_operation.clone(), evidence.file_identity.clone(), evidence.digest.to_vec(), to_i64(evidence.bytes, "copy bytes")?]).await?;
    transition(connection, job, "reserved", "verified").await
}

async fn publish(connection: &turso::Connection, job: &Job) -> anyhow::Result<()> {
    if job.phase == "published" {
        return Ok(());
    }
    anyhow::ensure!(job.phase == "file_published", "move file is not published");
    let Reply::Location(Some(current)) = ownership::lookup(connection, &job.object).await? else {
        anyhow::bail!("move source is no longer authoritative");
    };
    anyhow::ensure!(current == job.source, "move source changed");
    let next_revision = job
        .source
        .revision
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("location revision exhausted"))?;
    let mut rows = connection.query("SELECT destination_path,file_identity,digest,bytes FROM storage_volume_allocations WHERE operation = ?1 AND state = 'reserved'", [job.destination_operation.as_str()]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("move destination reservation missing"))?;
    let path = row.get::<String>(0)?;
    let identity = row.get::<String>(1)?;
    anyhow::ensure!(
        row.get::<Vec<u8>>(2)? == job.source.digest,
        "move destination digest changed"
    );
    connection.execute("UPDATE storage_volume_allocations SET object_id = ?2 WHERE operation = (SELECT source_operation FROM storage_volume_moves WHERE id = ?1)",
        turso::params![job.id.clone(), format!("retired:{}", job.id)]).await?;
    if job.object.kind == super::Kind::Recording {
        let changed = connection.execute("UPDATE recording_files SET path = ?2, file_identity = ?3 WHERE id = ?1 AND finalized = 1",
            turso::params![job.object.id.clone(), path, identity]).await?;
        anyhow::ensure!(changed == 1, "move recording is not finalized");
    }
    connection.execute("UPDATE storage_volume_allocations SET object_id = ?2, state = 'published', bytes = ?3, location_revision = ?4 WHERE operation = ?1",
        turso::params![job.destination_operation.clone(), job.object.id.clone(), to_i64(job.source.bytes, "copy bytes")?, to_i64(next_revision, "location revision")?]).await?;
    transition(connection, job, "file_published", "published").await
}
