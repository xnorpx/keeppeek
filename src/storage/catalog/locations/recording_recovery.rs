//! Records a verified prefix before an interrupted recording is shortened.

use super::{Publication, Reply, bump_revision, export_cleanup::Owned, to_i64, to_u64};
mod abandonment;
mod conflicts;

pub const FRAGMENTS_MAX: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub sequence: u64,
    pub offset: u64,
    pub bytes: u64,
    pub key_offset: Option<u64>,
    pub key_bytes: Option<u64>,
    pub start_ms: i64,
    pub duration_ms: u64,
    pub random_access: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Pending {
    pub owned: Owned,
    pub recording: String,
    pub path: String,
    pub started_at_ms: i64,
    pub init_offset: u64,
    pub init_bytes: u64,
    pub fragments: Vec<Fragment>,
    pub plan: Option<Plan>,
    pub complete: bool,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingRecording")
            .field("recording", &self.recording)
            .field("fragments", &self.fragments.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub mode: Mode,
    pub evidence: Publication,
    pub original_bytes: u64,
    pub last_sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Seal,
    Abandon,
}

impl Mode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Seal => "seal",
            Self::Abandon => "abandon",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    Load(String),
    ReleaseUnopened(Box<Pending>),
    Begin(Box<Pending>, Plan),
    Complete(Publication),
    Acknowledge(String),
}

impl Action {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Load(id) | Self::Acknowledge(id) => super::identifier(id),
            Self::ReleaseUnopened(pending) => super::identifier(&pending.owned.operation),
            Self::Complete(evidence) => validate_evidence(evidence),
            Self::Begin(pending, plan) => {
                validate_evidence(&plan.evidence)?;
                anyhow::ensure!(
                    pending.fragments.len() <= FRAGMENTS_MAX,
                    "too many recovery fragments"
                );
                anyhow::ensure!(
                    plan.evidence.operation == pending.owned.operation
                        && plan.evidence.bytes <= plan.original_bytes
                        && plan.original_bytes <= pending.owned.bytes
                        && match plan.mode {
                            Mode::Seal => plan.last_sequence > 0 && plan.evidence.bytes > 0,
                            Mode::Abandon =>
                                plan.last_sequence == 0
                                    && plan.original_bytes == plan.evidence.bytes,
                        },
                    "invalid recording recovery plan"
                );
                Ok(())
            }
        }
    }
}

fn validate_evidence(evidence: &Publication) -> anyhow::Result<()> {
    super::identifier(&evidence.operation)?;
    super::identifier(&evidence.file_identity)?;
    to_i64(evidence.bytes, "recording recovery bytes")?;
    Ok(())
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS storage_recording_recovery (
        operation TEXT PRIMARY KEY REFERENCES storage_volume_allocations(operation),
        recording_id TEXT NOT NULL UNIQUE,
        mode TEXT NOT NULL CHECK(mode IN ('seal','abandon')),
        original_bytes INTEGER NOT NULL CHECK(original_bytes >= retained_bytes),
        retained_bytes INTEGER NOT NULL CHECK(retained_bytes >= 0),
        last_sequence INTEGER NOT NULL CHECK(last_sequence >= 0),
        file_identity TEXT NOT NULL,
        digest BLOB NOT NULL CHECK(length(digest)=32),
        complete INTEGER NOT NULL DEFAULT 0 CHECK(complete IN (0,1)),
        acknowledged INTEGER NOT NULL DEFAULT 0 CHECK(acknowledged IN (0,1)),
        CHECK((mode='seal' AND retained_bytes>0 AND last_sequence>0) OR (mode='abandon' AND original_bytes=retained_bytes AND last_sequence=0))
    );
    CREATE TRIGGER IF NOT EXISTS storage_recording_recovery_update_fence
    BEFORE UPDATE ON recording_files WHEN EXISTS(SELECT 1 FROM storage_recording_recovery WHERE recording_id=OLD.id AND complete=0)
    BEGIN SELECT RAISE(ABORT,'recording recovery owns this object'); END;
    CREATE TRIGGER IF NOT EXISTS storage_recording_recovery_allocation_fence
    BEFORE UPDATE ON storage_volume_allocations WHEN EXISTS(SELECT 1 FROM storage_recording_recovery WHERE operation=OLD.operation AND complete=0)
    BEGIN SELECT RAISE(ABORT,'recording recovery owns this allocation'); END;").await?;
    conflicts::initialize(connection).await?;
    for table in ["recording_fragments", "recording_keyframes"] {
        for (action, row) in [("INSERT", "NEW"), ("UPDATE", "OLD"), ("DELETE", "OLD")] {
            let owner = if action == "UPDATE" {
                "recording_id IN (OLD.recording_id,NEW.recording_id)".to_owned()
            } else {
                format!("recording_id={row}.recording_id")
            };
            connection.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS {table}_recovery_{action}
                BEFORE {action} ON {table} WHEN EXISTS(SELECT 1 FROM storage_recording_recovery WHERE {owner} AND complete=0)
                BEGIN SELECT RAISE(ABORT,'recording recovery owns this object'); END;")).await?;
        }
    }
    Ok(())
}

pub(super) async fn dispatch(
    connection: &turso::Connection,
    action: Action,
) -> anyhow::Result<Reply> {
    match action {
        Action::Load(id) => Ok(Reply::PendingRecording(
            load(connection, &id).await?.map(Box::new),
        )),
        Action::Begin(pending, plan) => begin(connection, &pending, &plan).await,
        Action::ReleaseUnopened(pending) => release_unopened(connection, &pending).await,
        Action::Complete(evidence) => complete(connection, &evidence).await,
        Action::Acknowledge(id) => abandonment::acknowledge(connection, &id).await,
    }
}

pub(super) async fn load(
    connection: &turso::Connection,
    operation: &str,
) -> anyhow::Result<Option<Pending>> {
    let mut rows = connection.query("SELECT a.volume_id,a.generation,a.relative_key,a.bytes,a.materialized_bytes,a.file_identity,
        a.object_id,COALESCE(r.path,a.destination_path),COALESCE(r.init_offset,0),COALESCE(r.init_len,0),COALESCE(r.started_at_ms,0),b.root,COALESCE(q.complete,0)
        FROM storage_volume_allocations a LEFT JOIN recording_files r ON r.id=a.object_id
        JOIN storage_volume_bindings b ON b.id=a.volume_id AND b.generation=a.generation
        LEFT JOIN storage_recording_recovery q ON q.operation=a.operation
        WHERE a.operation=?1 AND a.kind='recording' AND (a.state='reserved' OR (q.mode='abandon' AND q.acknowledged=0))
        AND COALESCE(r.finalized,0)=0 AND COALESCE(r.cleanup_pending,0)=0
        AND NOT EXISTS(SELECT 1 FROM storage_volume_moves WHERE destination_operation=a.operation)", [operation]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    anyhow::ensure!(
        std::path::Path::new(&row.get::<String>(7)?)
            == std::path::PathBuf::from(row.get::<String>(11)?).join(row.get::<String>(2)?),
        "recording is not at its reserved location"
    );
    let mut pending = Pending {
        owned: Owned {
            operation: operation.into(),
            volume: row.get(0)?,
            generation: to_u64(row.get(1)?, "generation")?,
            relative_key: row.get(2)?,
            bytes: to_u64(row.get(3)?, "reserved bytes")?,
            materialized_bytes: to_u64(row.get(4)?, "materialized bytes")?,
            file_identity: row.get(5)?,
            digest: None,
        },
        recording: row.get(6)?,
        path: row.get(7)?,
        started_at_ms: row.get(10)?,
        init_offset: to_u64(row.get(8)?, "init offset")?,
        init_bytes: to_u64(row.get(9)?, "init bytes")?,
        fragments: Vec::new(),
        plan: None,
        complete: row.get::<i64>(12)? != 0,
    };
    drop(rows);
    pending.fragments = fragments(connection, &pending.recording).await?;
    pending.plan = plan(connection, operation).await?;
    Ok(Some(pending))
}

async fn release_unopened(
    connection: &turso::Connection,
    expected: &Pending,
) -> anyhow::Result<Reply> {
    let current = load(connection, &expected.owned.operation)
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording allocation is no longer pending"))?;
    anyhow::ensure!(
        &current == expected && current.owned.file_identity.is_none() && current.plan.is_none(),
        "unopened recording changed"
    );
    let mut rows = connection
        .query(
            "SELECT 1 FROM recording_files WHERE id=?1",
            [current.recording.as_str()],
        )
        .await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "recording metadata exists without file evidence"
    );
    drop(rows);
    super::images::retirement::ensure_writable(connection, &current.owned.operation).await?;
    connection
        .execute(
            "UPDATE storage_volume_archives SET done=1 WHERE operation=?1",
            [current.owned.operation.as_str()],
        )
        .await?;
    connection
        .execute(
            "UPDATE storage_volume_allocations SET state='cancelled' WHERE operation=?1",
            [current.owned.operation.as_str()],
        )
        .await?;
    bump_revision(connection).await?;
    Ok(Reply::Bound)
}

async fn fragments(connection: &turso::Connection, id: &str) -> anyhow::Result<Vec<Fragment>> {
    let mut rows = connection.query("SELECT f.sequence,f.byte_offset,f.byte_len,k.byte_offset,k.byte_len,f.start_ms,f.duration_ms,f.random_access
        FROM recording_fragments f LEFT JOIN recording_keyframes k ON k.recording_id=f.recording_id AND k.fragment_sequence=f.sequence
        WHERE f.recording_id=?1 ORDER BY f.sequence LIMIT 4097", [id]).await?;
    let mut fragments = Vec::new();
    while let Some(row) = rows.next().await? {
        anyhow::ensure!(
            fragments.len() < FRAGMENTS_MAX,
            "too many recovery fragments"
        );
        fragments.push(Fragment {
            sequence: to_u64(row.get(0)?, "sequence")?,
            offset: to_u64(row.get(1)?, "fragment offset")?,
            bytes: to_u64(row.get(2)?, "fragment bytes")?,
            key_offset: row
                .get::<Option<i64>>(3)?
                .map(|value| to_u64(value, "keyframe offset"))
                .transpose()?,
            key_bytes: row
                .get::<Option<i64>>(4)?
                .map(|value| to_u64(value, "keyframe bytes"))
                .transpose()?,
            start_ms: row.get(5)?,
            duration_ms: to_u64(row.get(6)?, "fragment duration")?,
            random_access: row.get::<i64>(7)? != 0,
        });
    }
    Ok(fragments)
}

async fn plan(connection: &turso::Connection, operation: &str) -> anyhow::Result<Option<Plan>> {
    let mut rows = connection.query("SELECT original_bytes,retained_bytes,last_sequence,file_identity,digest,mode FROM storage_recording_recovery WHERE operation=?1 AND (complete=0 OR (mode='abandon' AND acknowledged=0))", [operation]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(Plan {
        mode: match row.get::<String>(5)?.as_str() {
            "seal" => Mode::Seal,
            "abandon" => Mode::Abandon,
            _ => anyhow::bail!("invalid recording recovery mode"),
        },
        original_bytes: to_u64(row.get(0)?, "original bytes")?,
        last_sequence: to_u64(row.get(2)?, "last sequence")?,
        evidence: Publication {
            operation: operation.into(),
            bytes: to_u64(row.get(1)?, "retained bytes")?,
            file_identity: row.get(3)?,
            digest: row
                .get::<Vec<u8>>(4)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid recovery digest"))?,
        },
    }))
}

async fn begin(
    connection: &turso::Connection,
    expected: &Pending,
    plan: &Plan,
) -> anyhow::Result<Reply> {
    let current = load(connection, &expected.owned.operation)
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording is no longer pending"))?;
    if let Some(existing) = &current.plan {
        anyhow::ensure!(existing == plan, "recording recovery plan changed");
        return Ok(Reply::Bound);
    }
    anyhow::ensure!(&current == expected, "recording recovery preview changed");
    conflicts::check(connection, &current, plan).await?;
    anyhow::ensure!(
        current.owned.file_identity.as_ref() == Some(&plan.evidence.file_identity),
        "recording identity changed"
    );
    if plan.mode == Mode::Abandon {
        anyhow::ensure!(
            current.fragments.is_empty(),
            "indexed recording cannot be abandoned"
        );
    } else {
        let last = current
            .fragments
            .iter()
            .find(|fragment| fragment.sequence == plan.last_sequence)
            .ok_or_else(|| anyhow::anyhow!("recovery fragment missing"))?;
        anyhow::ensure!(
            last.offset.checked_add(last.bytes) == Some(plan.evidence.bytes),
            "recovery endpoint changed"
        );
    }
    super::images::retirement::ensure_writable(connection, &expected.owned.operation).await?;
    connection.execute("INSERT INTO storage_recording_recovery(operation,recording_id,original_bytes,retained_bytes,last_sequence,file_identity,digest,mode)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", turso::params![expected.owned.operation.clone(), expected.recording.clone(),
        to_i64(plan.original_bytes,"original bytes")?, to_i64(plan.evidence.bytes,"retained bytes")?, to_i64(plan.last_sequence,"last sequence")?,
        plan.evidence.file_identity.clone(), plan.evidence.digest.to_vec(), plan.mode.as_str()]).await?;
    bump_revision(connection).await?;
    Ok(Reply::Bound)
}

async fn complete(connection: &turso::Connection, evidence: &Publication) -> anyhow::Result<Reply> {
    if abandonment::replay_complete(connection, evidence).await? {
        return Ok(Reply::Bound);
    }
    let Some(pending) = load(connection, &evidence.operation).await? else {
        return super::ownership::finalize(connection, evidence).await;
    };
    let plan = pending
        .plan
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("recording recovery plan missing"))?;
    anyhow::ensure!(
        &plan.evidence == evidence,
        "recording recovery evidence changed"
    );
    if plan.mode == Mode::Abandon {
        if pending.complete {
            return Ok(Reply::Bound);
        }
        return abandonment::complete(connection, &pending).await;
    }
    connection
        .execute(
            "UPDATE storage_recording_recovery SET complete=1,acknowledged=1 WHERE operation=?1",
            [evidence.operation.as_str()],
        )
        .await?;
    connection
        .execute(
            "DELETE FROM recording_fragments WHERE recording_id=?1 AND sequence>?2",
            turso::params![
                pending.recording,
                to_i64(plan.last_sequence, "last sequence")?
            ],
        )
        .await?;
    connection
        .execute(
            "UPDATE storage_volume_allocations SET materialized_bytes=?2 WHERE operation=?1",
            turso::params![
                evidence.operation.clone(),
                to_i64(evidence.bytes, "retained bytes")?
            ],
        )
        .await?;
    super::ownership::finalize(connection, evidence).await
}
