//! Reserves confirmed recording objects before any filesystem operation.
//!
//! Reservations fence catalog mutation and automatic retention. They do not prove
//! filesystem ownership, authorize removal, or change recording bytes.

use super::{Action, Failure, Job, State, check_revision, load, validate_action};
use crate::storage::catalog::maintenance::{
    FileIdentity, MAX_RECORDINGS, Recording, Scope, check_deadline,
};
use crate::storage::catalog::{BUSY_TIMEOUT, Command, RecordingCatalogHandle};
use crate::storage::long_term::inspection::{IDENTITY_BYTES_MAX, PATH_BYTES_MAX};
use std::{fmt, path::PathBuf, sync::mpsc, time::Instant};

/// Retains one durable reservation without exposing its internal path or token.
#[derive(Clone, PartialEq, Eq)]
pub struct Claim {
    pub(in crate::storage) job_id: String,
    pub(in crate::storage) recording_id: String,
    pub(in crate::storage) token: String,
    pub(in crate::storage) path: PathBuf,
    pub(in crate::storage) file_identity: FileIdentity,
    pub(in crate::storage) file_bytes: u64,
}

impl Claim {
    pub fn recording_id(&self) -> &str {
        &self.recording_id
    }
}

impl fmt::Debug for Claim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Claim")
            .field("recording_id", &self.recording_id)
            .field("file_bytes", &self.file_bytes)
            .finish_non_exhaustive()
    }
}

impl RecordingCatalogHandle {
    /// Reserves a confirmed selection atomically without touching recording files.
    ///
    /// The caller must authenticate and authorize the actor immediately before calling.
    /// Repeated calls return the same reservations, including after catalog restart.
    ///
    /// # Errors
    /// Rejects unavailable, stale, cancelled, corrupt, changed, or competing selections.
    /// A timeout may have committed reservations; retry the same job identity.
    pub fn recording_deletion_claims(&self, actor: &str, id: &str) -> anyhow::Result<Vec<Claim>> {
        anyhow::ensure!(id.len() == 32, Failure::Invalid);
        validate_action(actor, &Action::Read { id: id.to_owned() })?;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::ClaimRecordings {
                actor: actor.to_owned(),
                id: id.to_owned(),
                deadline,
                reply,
            })
            .map_err(|_| Failure::Unavailable)?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failure::Unavailable)?
            .map_err(|error| {
                error
                    .downcast_ref::<Failure>()
                    .copied()
                    .unwrap_or(Failure::Unavailable)
                    .into()
            })
    }
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS recording_maintenance_claims (
            job_id TEXT NOT NULL, ordinal INTEGER NOT NULL,
            recording_id TEXT NOT NULL, token TEXT NOT NULL UNIQUE,
            path TEXT NOT NULL, file_identity BLOB NOT NULL, file_bytes INTEGER NOT NULL,
            active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
            PRIMARY KEY (job_id, ordinal)
         );
         CREATE UNIQUE INDEX IF NOT EXISTS recording_maintenance_active_recording
             ON recording_maintenance_claims(recording_id) WHERE active = 1;
         CREATE UNIQUE INDEX IF NOT EXISTS recording_maintenance_active_path
             ON recording_maintenance_claims(path) WHERE active = 1;
         CREATE TRIGGER IF NOT EXISTS recording_maintenance_fence_update
         BEFORE UPDATE ON recording_files
         WHEN EXISTS (SELECT 1 FROM recording_maintenance_claims
                                            WHERE (recording_id = OLD.id OR path = NEW.path)
                                            AND NOT EXISTS (SELECT 1 FROM recording_maintenance_execution
                                                WHERE recording_maintenance_execution.job_id = recording_maintenance_claims.job_id
                                                AND recording_maintenance_execution.recording_id = recording_maintenance_claims.recording_id AND phase IN ('deleted', 'cancelled')))
         BEGIN SELECT RAISE(ABORT, 'recording maintenance is pending'); END;
         CREATE TRIGGER IF NOT EXISTS recording_maintenance_fence_insert
         BEFORE INSERT ON recording_files
         WHEN EXISTS (SELECT 1 FROM recording_maintenance_claims
                                            WHERE (recording_id = NEW.id OR path = NEW.path)
                                            AND NOT EXISTS (SELECT 1 FROM recording_maintenance_execution
                                                WHERE recording_maintenance_execution.job_id = recording_maintenance_claims.job_id
                                                AND recording_maintenance_execution.recording_id = recording_maintenance_claims.recording_id AND phase IN ('deleted', 'cancelled')))
         BEGIN SELECT RAISE(ABORT, 'recording maintenance is pending'); END;
         CREATE TRIGGER IF NOT EXISTS recording_maintenance_fence_delete
         BEFORE DELETE ON recording_files
                 WHEN EXISTS (SELECT 1 FROM recording_maintenance_claims WHERE recording_id = OLD.id
                                            AND NOT EXISTS (SELECT 1 FROM recording_maintenance_execution
                                                WHERE recording_maintenance_execution.job_id = recording_maintenance_claims.job_id
                                                AND recording_maintenance_execution.recording_id = recording_maintenance_claims.recording_id AND phase IN ('deleted', 'cancelled')))
         BEGIN SELECT RAISE(ABORT, 'recording maintenance is pending'); END;",
        )
        .await?;
    Ok(())
}

pub(in crate::storage::catalog) async fn reserve(
    connection: &turso::Connection,
    actor: &str,
    id: &str,
    deadline: Instant,
) -> anyhow::Result<Vec<Claim>> {
    check_deadline(deadline)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = reserve_inner(connection, actor, id, deadline).await;
    match result {
        Ok(claims) => {
            if let Err(error) = check_deadline(deadline) {
                connection.execute_batch("ROLLBACK").await?;
                return Err(error);
            }
            connection.execute_batch("COMMIT").await?;
            Ok(claims)
        }
        Err(error) => {
            connection
                .execute_batch("ROLLBACK")
                .await
                .map_err(|rollback| {
                    anyhow::anyhow!("claim failed: {error}; rollback failed: {rollback}")
                })?;
            Err(error)
        }
    }
}

async fn reserve_inner(
    connection: &turso::Connection,
    actor: &str,
    id: &str,
    deadline: Instant,
) -> anyhow::Result<Vec<Claim>> {
    let (job, _) = load(connection, actor, id, deadline).await?;
    anyhow::ensure!(
        matches!(job.state, State::Queued | State::Cancelled),
        Failure::InvalidState
    );
    let existing = read(connection, &job, deadline).await?;
    if !existing.is_empty() || job.state == State::Cancelled {
        return Ok(existing);
    }
    check_revision(connection, job.revision).await?;
    super::check_evidence(connection, &job.snapshot).await?;
    let mut insert = connection
        .prepare(
            "INSERT INTO recording_maintenance_claims
         (job_id, ordinal, recording_id, token, path, file_identity, file_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .await?;
    for (ordinal, expected) in job.snapshot.recordings.iter().enumerate() {
        check_deadline(deadline)?;
        let claim = candidate(connection, &job, expected).await?;
        insert
            .execute(turso::params![
                id,
                i64::try_from(ordinal)?,
                claim.recording_id.as_str(),
                claim.token.as_str(),
                claim.path.to_str().ok_or(Failure::Invalid)?,
                claim.file_identity.0.to_vec(),
                i64::try_from(claim.file_bytes)?
            ])
            .await
            .map_err(|_| Failure::Conflict)?;
    }
    read(connection, &job, deadline).await
}

async fn candidate(
    connection: &turso::Connection,
    job: &Job,
    expected: &Recording,
) -> anyhow::Result<Claim> {
    let (source, stream) = match &job.snapshot.scope {
        Scope::Recording {
            source_id,
            stream_id,
            ..
        }
        | Scope::TimeRange {
            source_id,
            stream_id,
            ..
        } => (source_id, stream_id),
    };
    let mut rows = connection
        .query(
            "SELECT CASE WHEN length(CAST(path AS BLOB)) <= ?2 THEN path END,
         CASE WHEN length(CAST(file_identity AS BLOB)) <= ?3 THEN file_identity END,
         file_bytes, source_id, logical_stream_id, started_at_ms, ended_at_ms,
         finalized, protected, cleanup_pending FROM recording_files WHERE id = ?1",
            turso::params![
                expected.recording_id.as_str(),
                i64::try_from(PATH_BYTES_MAX)?,
                i64::try_from(IDENTITY_BYTES_MAX)?
            ],
        )
        .await?;
    let row = rows.next().await?.ok_or(Failure::Conflict)?;
    anyhow::ensure!(
        row.get::<i64>(7)? == 1 && row.get::<i64>(8)? == 0 && row.get::<i64>(9)? == 0,
        Failure::Blocked
    );
    anyhow::ensure!(
        row.get::<Option<String>>(3)?.as_ref() == Some(source)
            && row.get::<Option<String>>(4)?.as_ref() == Some(stream)
            && row.get::<i64>(5)? == expected.started_at_ms
            && row.get::<Option<i64>>(6)? == expected.ended_at_ms,
        Failure::Conflict
    );
    let bytes = u64::try_from(row.get::<i64>(2)?).map_err(|_| Failure::Invalid)?;
    let identity = row
        .get::<Option<String>>(1)?
        .and_then(|value| FileIdentity::parse(&value))
        .ok_or(Failure::Blocked)?;
    anyhow::ensure!(
        Some(identity) == expected.file_identity && bytes == expected.catalog_bytes,
        Failure::Conflict
    );
    Ok(Claim {
        job_id: job.id.clone(),
        recording_id: expected.recording_id.clone(),
        token: format!("{:032x}", rand::random::<u128>()),
        path: PathBuf::from(row.get::<Option<String>>(0)?.ok_or(Failure::Blocked)?),
        file_identity: identity,
        file_bytes: bytes,
    })
}

pub(super) async fn read(
    connection: &turso::Connection,
    job: &Job,
    deadline: Instant,
) -> anyhow::Result<Vec<Claim>> {
    let mut rows = connection
        .query(
            "SELECT ordinal, recording_id, token,
         CASE WHEN length(CAST(path AS BLOB)) <= ?3 THEN path END,
         CASE WHEN length(file_identity) = 32 THEN file_identity END, file_bytes
         FROM recording_maintenance_claims WHERE job_id = ?1 ORDER BY ordinal LIMIT ?2",
            turso::params![
                job.id.as_str(),
                i64::try_from(MAX_RECORDINGS + 1)?,
                i64::try_from(PATH_BYTES_MAX)?
            ],
        )
        .await?;
    let mut claims = Vec::with_capacity(job.objects.len());
    while let Some(row) = rows.next().await? {
        check_deadline(deadline)?;
        let expected = job
            .snapshot
            .recordings
            .get(claims.len())
            .ok_or(Failure::Invalid)?;
        let token: String = row.get(2)?;
        anyhow::ensure!(
            token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit()),
            Failure::Invalid
        );
        let identity = FileIdentity(
            row.get::<Vec<u8>>(4)?
                .try_into()
                .map_err(|_| Failure::Invalid)?,
        );
        let bytes = u64::try_from(row.get::<i64>(5)?).map_err(|_| Failure::Invalid)?;
        anyhow::ensure!(
            row.get::<i64>(0)? == i64::try_from(claims.len())?
                && row.get::<String>(1)? == expected.recording_id
                && Some(identity) == expected.file_identity
                && bytes == expected.catalog_bytes,
            Failure::Invalid
        );
        claims.push(Claim {
            job_id: job.id.clone(),
            recording_id: expected.recording_id.clone(),
            token,
            path: PathBuf::from(row.get::<Option<String>>(3)?.ok_or(Failure::Invalid)?),
            file_identity: identity,
            file_bytes: bytes,
        });
    }
    anyhow::ensure!(
        claims.is_empty() || claims.len() == job.objects.len(),
        Failure::Invalid
    );
    Ok(claims)
}

pub(super) async fn release(connection: &turso::Connection, id: &str) -> anyhow::Result<()> {
    connection
        .execute(
            "DELETE FROM recording_maintenance_claims WHERE job_id = ?1
             AND NOT EXISTS (SELECT 1 FROM recording_maintenance_execution
                WHERE recording_maintenance_execution.job_id = recording_maintenance_claims.job_id)",
            turso::params![id],
        )
        .await?;
    Ok(())
}
