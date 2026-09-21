//! Persists retention decisions through the catalog's serialized writer.
//!
//! These records do not authorize deletion. A future cleanup claimant must also
//! validate complete evidence, recording identity, active maintenance, and holds.

use super::{Command, RecordingCatalogHandle, to_i64, to_u64};
use std::{sync::mpsc, time::Instant};

/// Durable evaluation state, independent of a recording's current filesystem path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub revision: u64,
    pub policy_revision: u64,
    pub deadline_ms: Option<i64>,
    pub evidence_through_ms: i64,
}

/// A compare-and-swap update; absence of a revision requires a new record.
#[derive(Debug, Clone)]
pub struct Update {
    pub expected_revision: Option<u64>,
    pub policy_revision: u64,
    pub deadline_ms: Option<i64>,
    pub evidence_through_ms: i64,
}

pub(super) enum Request {
    Get(String),
    Commit {
        recording_id: String,
        update: Update,
    },
}

impl RecordingCatalogHandle {
    pub fn recording_retention(&self, recording_id: &str) -> anyhow::Result<Option<Snapshot>> {
        self.retention_request(Request::Get(recording_id.to_owned()))
    }

    /// Commits only finalized, unclaimed media and never shortens its deadline.
    /// A timed-out request may have committed; reload before retrying its revision.
    pub fn commit_recording_retention(
        &self,
        recording_id: &str,
        update: Update,
    ) -> anyhow::Result<Snapshot> {
        self.retention_request(Request::Commit {
            recording_id: recording_id.to_owned(),
            update,
        })?
        .ok_or_else(|| anyhow::anyhow!("retention update returned no committed state"))
    }

    fn retention_request(&self, request: Request) -> anyhow::Result<Option<Snapshot>> {
        let deadline = Instant::now() + super::BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::Retention {
                request,
                deadline,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable or busy"))?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| {
                anyhow::anyhow!("recording retention reply is unavailable; reload state")
            })?
    }
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS recording_retention (
             recording_id TEXT PRIMARY KEY REFERENCES recording_files(id) ON DELETE CASCADE,
             revision INTEGER NOT NULL CHECK (revision > 0),
             policy_revision INTEGER NOT NULL CHECK (policy_revision >= 0),
             deadline_ms INTEGER,
             evidence_through_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS recording_retention_expiry
             ON recording_retention(deadline_ms, recording_id);",
        )
        .await?;
    Ok(())
}

pub(super) async fn execute(
    connection: &turso::Connection,
    request: Request,
    deadline: Instant,
) -> anyhow::Result<Option<Snapshot>> {
    anyhow::ensure!(Instant::now() < deadline, "retention request expired");
    match request {
        Request::Get(id) => get(connection, &id).await,
        Request::Commit {
            recording_id,
            update,
        } => {
            connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                let snapshot = commit(connection, &recording_id, update).await?;
                anyhow::ensure!(Instant::now() < deadline, "retention request expired");
                connection.execute_batch("COMMIT").await?;
                Ok(Some(snapshot))
            }
            .await;
            if result.is_err() {
                connection
                    .execute_batch("ROLLBACK")
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "retention transaction rollback failed: {error}; original: {result:?}"
                        )
                    })?;
            }
            result
        }
    }
}

async fn get(connection: &turso::Connection, id: &str) -> anyhow::Result<Option<Snapshot>> {
    let mut rows = connection
        .query(
            "SELECT revision, policy_revision, deadline_ms, evidence_through_ms
         FROM recording_retention WHERE recording_id = ?1",
            turso::params![id],
        )
        .await?;
    rows.next()
        .await?
        .map(|row| {
            Ok(Snapshot {
                revision: to_u64(row.get(0)?, "retention revision")?,
                policy_revision: to_u64(row.get(1)?, "retention policy revision")?,
                deadline_ms: row.get(2)?,
                evidence_through_ms: row.get(3)?,
            })
        })
        .transpose()
}

async fn commit(
    connection: &turso::Connection,
    id: &str,
    update: Update,
) -> anyhow::Result<Snapshot> {
    let mut rows = connection
        .query(
            "SELECT ended_at_ms FROM recording_files WHERE id = ?1 AND finalized = 1
         AND ended_at_ms IS NOT NULL AND cleanup_pending = 0
         AND NOT EXISTS (SELECT 1 FROM recording_maintenance_claims
                         WHERE recording_id = ?1 AND active = 1)",
            turso::params![id],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording is unavailable for retention evaluation"))?;
    let ended_at_ms: i64 = row.get(0)?;
    drop(rows);
    anyhow::ensure!(
        update.evidence_through_ms >= ended_at_ms,
        "retention evidence is incomplete"
    );
    let previous = get(connection, id).await?;
    anyhow::ensure!(
        previous.as_ref().map(|state| state.revision) == update.expected_revision,
        "retention revision conflict"
    );
    let mut next = Snapshot {
        revision: update
            .expected_revision
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("retention revision exhausted"))?,
        policy_revision: update.policy_revision,
        deadline_ms: update.deadline_ms,
        evidence_through_ms: update.evidence_through_ms,
    };
    if let Some(previous) = previous {
        anyhow::ensure!(
            next.policy_revision >= previous.policy_revision,
            "stale retention policy"
        );
        next.deadline_ms = next.deadline_ms.max(previous.deadline_ms);
        next.evidence_through_ms = next.evidence_through_ms.max(previous.evidence_through_ms);
    }
    connection.execute(
        "INSERT INTO recording_retention (recording_id, revision, policy_revision, deadline_ms, evidence_through_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(recording_id) DO UPDATE SET revision = excluded.revision,
             policy_revision = excluded.policy_revision, deadline_ms = excluded.deadline_ms,
             evidence_through_ms = excluded.evidence_through_ms",
        turso::params![id, to_i64(next.revision, "retention revision")?,
            to_i64(next.policy_revision, "retention policy revision")?, next.deadline_ms, next.evidence_through_ms],
    ).await?;
    Ok(next)
}
