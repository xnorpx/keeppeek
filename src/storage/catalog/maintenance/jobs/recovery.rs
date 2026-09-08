//! Reconciles abandoned deletion checkpoints without removing any present media.

use super::{Epoch, Failure, Job, claims, execution, load};
use crate::storage::{
    catalog::{BUSY_TIMEOUT, Command, RecordingCatalogHandle},
    long_term::inspection::Archive,
};
use std::{sync::mpsc, time::Instant};

/// Counts completed unlink evidence and objects that still require an authenticated retry.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub completed: u32,
    pub unresolved: u32,
}

impl RecordingCatalogHandle {
    /// Reconciles bounded startup state without authorizing further filesystem deletion.
    ///
    /// # Errors
    /// Rejects corrupt records, a busy catalog, an expired operation, or an unavailable archive.
    pub fn recover_recording_deletions(&self, archive: &Archive) -> anyhow::Result<Summary> {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::RecoverDeletions {
                archive: archive.try_clone()?,
                deadline,
                reply,
            })
            .map_err(|_| Failure::Unavailable)?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failure::Unavailable)?
    }
}

pub(in crate::storage::catalog) async fn recover(
    connection: &turso::Connection,
    epoch: &Epoch,
    archive: &Archive,
    deadline: Instant,
) -> anyhow::Result<Summary> {
    super::check_deadline(deadline)?;
    let mut rows = connection.query(
        "SELECT DISTINCT intents.id, intents.actor FROM recording_maintenance_intents AS intents
         JOIN recording_maintenance_claims AS claims ON claims.job_id = intents.id
         WHERE claims.active = 1 ORDER BY intents.id LIMIT 257", (),
    ).await?;
    let mut jobs = Vec::with_capacity(256);
    while let Some(row) = rows.next().await? {
        super::check_deadline(deadline)?;
        anyhow::ensure!(jobs.len() < 256, Failure::Quota);
        jobs.push((row.get::<String>(0)?, row.get::<String>(1)?));
    }
    drop(rows);
    let mut summary = Summary::default();
    for (id, actor) in jobs {
        let (job, _) = load(connection, &actor, &id, deadline).await?;
        for claim in claims::read(connection, &job, deadline).await? {
            super::check_deadline(deadline)?;
            let result = settle(connection, epoch, archive, &job, &claim, deadline).await?;
            summary.completed += u32::from(result == Some(true));
            summary.unresolved += u32::from(result == Some(false));
        }
    }
    Ok(summary)
}

async fn settle(
    connection: &turso::Connection,
    epoch: &Epoch,
    archive: &Archive,
    job: &Job,
    claim: &claims::Claim,
    deadline: Instant,
) -> anyhow::Result<Option<bool>> {
    if epoch
        .executors
        .lock()
        .unwrap()
        .get(&claim.token)
        .is_some_and(|lease| lease.strong_count() > 0)
    {
        return Ok(None);
    }
    super::check_deadline(deadline)?;
    connection.busy_timeout(
        deadline
            .saturating_duration_since(Instant::now())
            .min(BUSY_TIMEOUT),
    )?;
    let result = async {
        connection.execute_batch("BEGIN IMMEDIATE").await?;
        let result = settle_inner(connection, epoch, archive, job, claim, deadline).await?;
        super::check_deadline(deadline)?;
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(result)
    }
    .await;
    if result.is_err() && !connection.is_autocommit()? {
        connection.execute_batch("ROLLBACK").await?;
    }
    connection.busy_timeout(BUSY_TIMEOUT)?;
    result
}

async fn settle_inner(
    connection: &turso::Connection,
    epoch: &Epoch,
    archive: &Archive,
    job: &Job,
    claim: &claims::Claim,
    deadline: Instant,
) -> anyhow::Result<Option<bool>> {
    let mut rows = connection.query(
        "SELECT execution.phase, substr(execution.staged_directory, 1, 33) FROM recording_maintenance_claims AS claims
         LEFT JOIN recording_maintenance_execution AS execution ON execution.job_id = claims.job_id AND execution.recording_id = claims.recording_id
         WHERE claims.job_id = ?1 AND claims.recording_id = ?2 AND claims.active = 1",
        turso::params![claim.job_id.as_str(), claim.recording_id.as_str()],
    ).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    if matches!(
        row.get::<Option<String>>(0)?.as_deref(),
        Some("deleted" | "cancelled")
    ) {
        return Ok(None);
    }
    let directory = execution::decode_identity(row.get(1)?)?;
    drop(rows);
    let completed = directory.is_some_and(|identity| {
        archive
            .check_removed_claim(claim, identity, deadline)
            .is_ok()
    });
    super::check_deadline(deadline)?;
    execution::persist_transition(
        connection,
        job,
        epoch,
        claim,
        if completed { "deleted" } else { "failed" },
        None,
    )
    .await?;
    Ok(Some(completed))
}
