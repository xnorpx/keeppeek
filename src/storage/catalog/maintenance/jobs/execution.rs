//! Executes reserved recording objects and retains per-object outcomes across restart.

use super::{Failure, State, claims::Claim, load};
use crate::storage::catalog::maintenance::{FileIdentity, MAX_RECORDINGS, check_deadline};
use crate::storage::catalog::{
    BUSY_TIMEOUT, CatalogDeletionReason, Command, RecordingCatalogHandle, current_unix_time_ms,
    record_deletion,
};
use crate::storage::long_term::inspection::Archive;
use std::{
    sync::{Arc, Weak, mpsc},
    time::Instant,
};

/// Describes durable work, not a filesystem observation inferred from an absent path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Reserved,
    Working,
    Staged,
    Deleted,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub recording_id: String,
    pub status: Status,
    pub changed_at_ms: Option<i64>,
    pub error: Option<String>,
    pub(crate) staged_directory: Option<FileIdentity>,
}

/// Contains bounded per-object progress without paths or claim tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub job_id: String,
    pub objects: Vec<Object>,
    pub deleted: u32,
    pub failed: u32,
    pub cancelled: bool,
}

pub(in crate::storage::catalog) enum Action {
    Read,
    Reject,
    Begin(Claim, Weak<()>),
    Staged(Claim, FileIdentity),
    Finish(Claim),
    Fail(Claim),
    Cancel(Claim),
}

impl RecordingCatalogHandle {
    /// Executes a confirmed selection against a trusted archive with durable object outcomes.
    ///
    /// The caller must authenticate and authorize the actor and coordinate archive migration,
    /// restore, evidence protection, and active consumers before invoking this internal method.
    /// Cancellation stops before the next object. Only synthetic media should be used in tests.
    ///
    /// # Errors
    /// Rejects stale or competing reservations and unavailable catalog operations. Per-file
    /// failures remain in the returned report; a lost reply requires querying the same job.
    pub fn execute_recording_deletion(
        &self,
        actor: &str,
        id: &str,
        archive: &Archive,
    ) -> anyhow::Result<Report> {
        self.execute_recording_deletion_authorized(actor, id, archive, |_| Ok(()))
    }

    pub(crate) fn execute_recording_deletion_authorized(
        &self,
        actor: &str,
        id: &str,
        archive: &Archive,
        authorize: impl Fn(bool) -> anyhow::Result<()>,
    ) -> anyhow::Result<Report> {
        authorize(false)?;
        archive.validate_removal()?;
        let claims = self.recording_deletion_claims(actor, id)?;
        let lease = Arc::new(());
        for claim in claims {
            authorize(false)?;
            let report = match self.execution_action(
                actor,
                id,
                Action::Begin(claim.clone(), Arc::downgrade(&lease)),
            ) {
                Ok(report) => report,
                Err(error) if error.downcast_ref::<Failure>() == Some(&Failure::InvalidState) => {
                    break;
                }
                Err(error) => return Err(error),
            };
            let object = report
                .objects
                .iter()
                .find(|object| object.recording_id == claim.recording_id)
                .ok_or(Failure::Invalid)?;
            if matches!(object.status, Status::Deleted | Status::Cancelled) {
                continue;
            }
            authorize(object.staged_directory.is_none())?;
            if report.cancelled
                && object.staged_directory.is_none()
                && self.cancel_claim(actor, id, &claim, archive)?
            {
                continue;
            }
            let staged = archive.stage_claim(&claim, object.staged_directory);
            match staged {
                Ok(Some(staged)) => {
                    self.execution_action(
                        actor,
                        id,
                        Action::Staged(claim.clone(), staged.directory_identity()),
                    )?;
                    authorize(false)?;
                    if staged.remove().is_err() {
                        self.execution_action(actor, id, Action::Fail(claim))?;
                    } else {
                        self.execution_action(actor, id, Action::Finish(claim))?;
                    }
                }
                Ok(None) => {
                    self.execution_action(actor, id, Action::Finish(claim))?;
                }
                Err(_) => {
                    self.execution_action(actor, id, Action::Fail(claim))?;
                }
            }
        }
        self.recording_deletion_progress(actor, id)
    }

    fn cancel_claim(
        &self,
        actor: &str,
        id: &str,
        claim: &Claim,
        archive: &Archive,
    ) -> anyhow::Result<bool> {
        let action = match archive.has_staged_claim(claim) {
            Ok(true) => return Ok(false),
            Ok(false) if archive.validate_unstaged_claim(claim).is_ok() => {
                Action::Cancel(claim.clone())
            }
            Ok(false) | Err(_) => Action::Fail(claim.clone()),
        };
        self.execution_action(actor, id, action)?;
        Ok(true)
    }

    /// Returns actor-owned durable progress without inspecting media.
    ///
    /// # Errors
    /// Rejects invalid identities, wrong owners, unavailable workers, and corrupt state.
    pub fn recording_deletion_progress(&self, actor: &str, id: &str) -> anyhow::Result<Report> {
        self.execution_action(actor, id, Action::Read)
    }

    pub(crate) fn reject_recording_deletion(
        &self,
        actor: &str,
        id: &str,
    ) -> anyhow::Result<Report> {
        self.execution_action(actor, id, Action::Reject)
    }

    fn execution_action(&self, actor: &str, id: &str, action: Action) -> anyhow::Result<Report> {
        anyhow::ensure!(id.len() == 32, Failure::Invalid);
        super::validate_action(actor, &super::Action::Read { id: id.to_owned() })?;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::DeletionWork {
                actor: actor.to_owned(),
                id: id.to_owned(),
                action,
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
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS recording_maintenance_execution (
            job_id TEXT NOT NULL, recording_id TEXT NOT NULL,
            phase TEXT NOT NULL CHECK (phase IN ('working', 'staged', 'deleted', 'failed', 'cancelled')),
            executor TEXT NOT NULL, changed_at_ms INTEGER NOT NULL,
            staged_directory BLOB CHECK (staged_directory IS NULL OR
                (typeof(staged_directory) = 'blob' AND length(staged_directory) = 32)),
            PRIMARY KEY (job_id, recording_id)
         );"
    ).await?;
    Ok(())
}

pub(in crate::storage::catalog) async fn execute(
    connection: &turso::Connection,
    epoch: &super::Epoch,
    actor: &str,
    id: &str,
    action: Action,
    deadline: Instant,
) -> anyhow::Result<Report> {
    check_deadline(deadline)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let (job, _) = load(connection, actor, id, deadline).await?;
        if !matches!(action, Action::Read) {
            transition(connection, &job, epoch, action).await?;
        }
        let report = read(connection, &job, &epoch.id, deadline).await?;
        check_deadline(deadline)?;
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(report)
    }
    .await;
    if result.is_err() && !connection.is_autocommit()? {
        connection.execute_batch("ROLLBACK").await?;
    }
    result
}

async fn transition(
    connection: &turso::Connection,
    job: &super::Job,
    epoch: &super::Epoch,
    action: Action,
) -> anyhow::Result<()> {
    let (claim, next, lease, directory) = match action {
        Action::Begin(claim, lease) => (claim, "working", Some(lease), None),
        Action::Staged(claim, directory) => (claim, "staged", None, Some(directory)),
        Action::Finish(claim) => (claim, "deleted", None, None),
        Action::Fail(claim) => (claim, "failed", None, None),
        Action::Cancel(claim) => (claim, "cancelled", None, None),
        Action::Reject => return reject(connection, &job.id).await,
        Action::Read => return Ok(()),
    };
    let mut rows = connection
        .query(
            "SELECT claims.token, execution.phase, execution.executor, substr(execution.staged_directory, 1, 33)
         FROM recording_maintenance_claims AS claims
         LEFT JOIN recording_maintenance_execution AS execution
           ON execution.job_id = claims.job_id AND execution.recording_id = claims.recording_id
         WHERE claims.job_id = ?1 AND claims.recording_id = ?2",
            turso::params![job.id.as_str(), claim.recording_id.as_str()],
        )
        .await?;
    let row = rows.next().await?.ok_or(Failure::InvalidState)?;
    anyhow::ensure!(
        claim.job_id == job.id && row.get::<String>(0)? == claim.token,
        Failure::Invalid
    );
    let phase: Option<String> = row.get(1)?;
    let executor: Option<String> = row.get(2)?;
    let previous_directory = decode_identity(row.get(3)?)?;
    drop(rows);
    if matches!(phase.as_deref(), Some("deleted" | "cancelled")) {
        return Ok(());
    }
    if next == "working" {
        if previous_directory.is_none() && job.state == State::Queued {
            super::check_evidence(connection, &job.snapshot).await?;
        }
        admit(job, epoch, &claim, phase.is_some(), lease)?;
    } else {
        anyhow::ensure!(
            matches!(phase.as_deref(), Some("working" | "staged"))
                && executor.as_deref() == Some(epoch.id.as_str()),
            Failure::InvalidState
        );
    }
    anyhow::ensure!(
        next != "deleted" || previous_directory.is_some(),
        Failure::InvalidState
    );
    if let (Some(previous), Some(current)) = (previous_directory, directory) {
        anyhow::ensure!(previous == current, Failure::Conflict);
    }
    persist_transition(connection, job, epoch, &claim, next, directory).await
}

pub(super) async fn persist_transition(
    connection: &turso::Connection,
    job: &super::Job,
    epoch: &super::Epoch,
    claim: &Claim,
    next: &str,
    directory: Option<FileIdentity>,
) -> anyhow::Result<()> {
    connection.execute(
        "INSERT INTO recording_maintenance_execution (job_id, recording_id, phase, executor, changed_at_ms, staged_directory)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(job_id, recording_id) DO UPDATE SET phase = excluded.phase,
            executor = excluded.executor, changed_at_ms = excluded.changed_at_ms,
            staged_directory = COALESCE(recording_maintenance_execution.staged_directory, excluded.staged_directory)",
        turso::params![job.id.as_str(), claim.recording_id.as_str(), next, epoch.id.as_str(), current_unix_time_ms(), directory.as_ref().map(|identity| identity.0.as_slice())]
    ).await?;
    if next == "deleted" {
        record_deletion(
            connection,
            &claim.recording_id,
            CatalogDeletionReason::Reconciliation,
        )
        .await?;
        connection
            .execute(
                "DELETE FROM recording_files WHERE id = ?1",
                turso::params![claim.recording_id.as_str()],
            )
            .await?;
    }
    if matches!(next, "deleted" | "cancelled") {
        connection.execute("UPDATE recording_maintenance_claims SET active = 0 WHERE job_id = ?1 AND recording_id = ?2",
            turso::params![job.id.as_str(), claim.recording_id.as_str()]).await?;
    }
    Ok(())
}

fn admit(
    job: &super::Job,
    epoch: &super::Epoch,
    claim: &Claim,
    started: bool,
    lease: Option<Weak<()>>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        job.state == State::Queued || (job.state == State::Cancelled && started),
        Failure::InvalidState
    );
    let lease = lease.ok_or(Failure::Invalid)?;
    anyhow::ensure!(lease.strong_count() > 0, Failure::Unavailable);
    let mut executors = epoch.executors.lock().unwrap();
    executors.retain(|_, previous| previous.strong_count() > 0);
    anyhow::ensure!(!executors.contains_key(&claim.token), Failure::Conflict);
    anyhow::ensure!(
        executors.len() < usize::try_from(super::MAX_JOBS)? * MAX_RECORDINGS,
        Failure::Quota
    );
    executors.insert(claim.token.clone(), lease);
    Ok(())
}

async fn read(
    connection: &turso::Connection,
    job: &super::Job,
    epoch: &str,
    deadline: Instant,
) -> anyhow::Result<Report> {
    let mut rows = connection
        .query(
            "SELECT objects.ordinal, objects.recording_id,
            CASE WHEN execution.phase IN ('working', 'staged') AND execution.executor <> ?3
                THEN 'failed' ELSE execution.phase END,
            execution.changed_at_ms, substr(execution.staged_directory, 1, 33)
         FROM recording_maintenance_objects AS objects
         LEFT JOIN recording_maintenance_execution AS execution
           ON execution.job_id = objects.job_id AND execution.recording_id = objects.recording_id
         WHERE objects.job_id = ?1 ORDER BY objects.ordinal LIMIT ?2",
            turso::params![job.id.as_str(), i64::try_from(MAX_RECORDINGS + 1)?, epoch],
        )
        .await?;
    let mut report = Report {
        job_id: job.id.clone(),
        objects: Vec::with_capacity(job.objects.len()),
        deleted: 0,
        failed: 0,
        cancelled: job.state == State::Cancelled,
    };
    while let Some(row) = rows.next().await? {
        check_deadline(deadline)?;
        let expected = job
            .objects
            .get(report.objects.len())
            .ok_or(Failure::Invalid)?;
        anyhow::ensure!(
            row.get::<i64>(0)? == i64::try_from(report.objects.len())?
                && row.get::<String>(1)? == expected.recording_id,
            Failure::Invalid
        );
        let status = match row.get::<Option<String>>(2)?.as_deref() {
            None if report.cancelled => Status::Cancelled,
            None => Status::Reserved,
            Some("working") => Status::Working,
            Some("staged") => Status::Staged,
            Some("deleted") => {
                report.deleted += 1;
                Status::Deleted
            }
            Some("failed") => {
                report.failed += 1;
                Status::Failed
            }
            Some("cancelled") => Status::Cancelled,
            _ => return Err(Failure::Invalid.into()),
        };
        report.objects.push(Object {
            recording_id: expected.recording_id.clone(),
            status,
            changed_at_ms: row.get(3)?,
            error: (status == Status::Failed)
                .then(|| "recording removal requires retry or inspection".to_owned()),
            staged_directory: decode_identity(row.get(4)?)?,
        });
    }
    anyhow::ensure!(report.objects.len() == job.objects.len(), Failure::Invalid);
    Ok(report)
}

pub(super) fn decode_identity(value: Option<Vec<u8>>) -> anyhow::Result<Option<FileIdentity>> {
    value
        .map(|bytes| {
            bytes
                .try_into()
                .map(FileIdentity)
                .map_err(|_| Failure::Invalid.into())
        })
        .transpose()
}

pub(super) async fn cancel_unstarted(
    connection: &turso::Connection,
    id: &str,
) -> anyhow::Result<()> {
    connection
        .execute(
            "INSERT INTO recording_maintenance_execution
         (job_id, recording_id, phase, executor, changed_at_ms, staged_directory)
         SELECT job_id, recording_id, 'cancelled', '', ?2, NULL FROM recording_maintenance_claims
         WHERE job_id = ?1 AND EXISTS
            (SELECT 1 FROM recording_maintenance_execution WHERE job_id = ?1)
         ON CONFLICT(job_id, recording_id) DO NOTHING",
            turso::params![id, current_unix_time_ms()],
        )
        .await?;
    deactivate_cancelled(connection, id).await?;
    Ok(())
}

async fn reject(connection: &turso::Connection, id: &str) -> anyhow::Result<()> {
    connection.execute(
        "INSERT INTO recording_maintenance_execution (job_id, recording_id, phase, executor, changed_at_ms, staged_directory)
         SELECT job_id, recording_id, 'failed', '', ?2, NULL FROM recording_maintenance_objects WHERE job_id = ?1
         ON CONFLICT(job_id, recording_id) DO UPDATE SET
             phase = CASE WHEN phase IN ('deleted', 'cancelled') THEN phase ELSE 'failed' END,
             changed_at_ms = CASE WHEN phase IN ('deleted', 'cancelled') THEN changed_at_ms ELSE excluded.changed_at_ms END",
        turso::params![id, current_unix_time_ms()],
    ).await?;
    deactivate_cancelled(connection, id).await?;
    Ok(())
}

async fn deactivate_cancelled(connection: &turso::Connection, id: &str) -> anyhow::Result<()> {
    connection.execute(
        "UPDATE recording_maintenance_claims SET active = 0 WHERE job_id = ?1
         AND EXISTS (SELECT 1 FROM recording_maintenance_execution AS execution
             WHERE execution.job_id = recording_maintenance_claims.job_id
             AND execution.recording_id = recording_maintenance_claims.recording_id AND phase = 'cancelled')",
        turso::params![id],
    ).await?;
    Ok(())
}

#[cfg(test)]
mod tests;
