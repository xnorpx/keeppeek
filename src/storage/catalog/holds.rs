//! Durable, independently releasable recording protection through the catalog writer.

use super::{Command, RecordingCatalogHandle, to_i64, to_u64};
use std::{sync::mpsc, time::Instant};

const MAX_HOLDS_PER_RECORDING: i64 = 256;
const MAX_ID_BYTES: usize = 128;
const MAX_ACTOR_BYTES: usize = 128;
const MAX_REASON_BYTES: usize = 256;

/// Stored hold state; released identities retain their revision to reject stale requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub revision: u64,
    pub active: bool,
    pub actor: String,
    pub reason: String,
}

/// The authenticated caller supplies attribution after authorizing the operation.
#[derive(Debug, Clone)]
pub struct Update {
    pub expected_revision: Option<u64>,
    pub active: bool,
    pub actor: String,
    pub reason: String,
}

pub(super) enum Request {
    Get {
        recording_id: String,
        hold_id: String,
    },
    Update {
        recording_id: String,
        hold_id: String,
        update: Update,
    },
}

impl RecordingCatalogHandle {
    pub fn recording_hold(
        &self,
        recording_id: &str,
        hold_id: &str,
    ) -> anyhow::Result<Option<Snapshot>> {
        validate_ids(recording_id, hold_id)?;
        self.hold_request(Request::Get {
            recording_id: recording_id.to_owned(),
            hold_id: hold_id.to_owned(),
        })
    }

    /// Protects finalized, unclaimed catalog media without an expiration time.
    /// A lost reply may have committed; reload the hold before retrying its revision.
    pub fn update_recording_hold(
        &self,
        recording_id: &str,
        hold_id: &str,
        update: Update,
    ) -> anyhow::Result<Snapshot> {
        validate_ids(recording_id, hold_id)?;
        validate_text(&update.actor, MAX_ACTOR_BYTES)?;
        validate_text(&update.reason, MAX_REASON_BYTES)?;
        self.hold_request(Request::Update {
            recording_id: recording_id.to_owned(),
            hold_id: hold_id.to_owned(),
            update,
        })?
        .ok_or_else(|| anyhow::anyhow!("hold update returned no committed state"))
    }

    fn hold_request(&self, request: Request) -> anyhow::Result<Option<Snapshot>> {
        let deadline = Instant::now() + super::BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::Hold {
                request,
                deadline,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable or busy"))?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| anyhow::anyhow!("recording hold reply is unavailable; reload state"))?
    }
}

fn validate_ids(recording_id: &str, hold_id: &str) -> anyhow::Result<()> {
    validate_text(recording_id, MAX_ID_BYTES)?;
    validate_text(hold_id, MAX_ID_BYTES)
}

fn validate_text(value: &str, bytes_max: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.len() <= bytes_max && !value.trim().is_empty(),
        "invalid hold text length"
    );
    anyhow::ensure!(
        !value.chars().any(char::is_control),
        "invalid hold text characters"
    );
    Ok(())
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS recording_hold_baselines (
            recording_id TEXT PRIMARY KEY REFERENCES recording_files(id) ON DELETE CASCADE,
            protected INTEGER NOT NULL CHECK (protected IN (0, 1))
         );
         CREATE TABLE IF NOT EXISTS recording_holds (
            recording_id TEXT NOT NULL REFERENCES recording_files(id) ON DELETE CASCADE,
            hold_id TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK (revision > 0),
            active INTEGER NOT NULL CHECK (active IN (0, 1)),
            actor TEXT NOT NULL, reason TEXT NOT NULL,
            PRIMARY KEY (recording_id, hold_id)
         );
         CREATE TRIGGER IF NOT EXISTS recording_hold_fence_delete
         BEFORE DELETE ON recording_files
         WHEN EXISTS (SELECT 1 FROM recording_holds WHERE recording_id = OLD.id AND active = 1)
         BEGIN SELECT RAISE(ABORT, 'recording is held'); END;
         CREATE TRIGGER IF NOT EXISTS recording_hold_fence_unprotect
         BEFORE UPDATE OF protected ON recording_files
         WHEN NEW.protected = 0 AND EXISTS
            (SELECT 1 FROM recording_holds WHERE recording_id = OLD.id AND active = 1)
         BEGIN SELECT RAISE(ABORT, 'recording is held'); END;",
        )
        .await?;
    Ok(())
}

pub(super) async fn execute(
    connection: &turso::Connection,
    request: Request,
    deadline: Instant,
) -> anyhow::Result<Option<Snapshot>> {
    anyhow::ensure!(Instant::now() < deadline, "hold request expired");
    match request {
        Request::Get {
            recording_id,
            hold_id,
        } => get(connection, &recording_id, &hold_id).await,
        Request::Update {
            recording_id,
            hold_id,
            update,
        } => {
            connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                let snapshot = commit(connection, &recording_id, &hold_id, update).await?;
                anyhow::ensure!(Instant::now() < deadline, "hold request expired");
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
                            "hold transaction rollback failed: {error}; original: {result:?}"
                        )
                    })?;
            }
            result
        }
    }
}

async fn get(
    connection: &turso::Connection,
    recording_id: &str,
    hold_id: &str,
) -> anyhow::Result<Option<Snapshot>> {
    let mut rows = connection.query(
        "SELECT revision, active, actor, reason FROM recording_holds WHERE recording_id = ?1 AND hold_id = ?2",
        turso::params![recording_id, hold_id],
    ).await?;
    rows.next()
        .await?
        .map(|row| {
            let snapshot = Snapshot {
                revision: to_u64(row.get(0)?, "hold revision")?,
                active: row.get::<i64>(1)? != 0,
                actor: row.get(2)?,
                reason: row.get(3)?,
            };
            anyhow::ensure!(snapshot.revision > 0, "invalid hold revision");
            validate_text(&snapshot.actor, MAX_ACTOR_BYTES)?;
            validate_text(&snapshot.reason, MAX_REASON_BYTES)?;
            Ok(snapshot)
        })
        .transpose()
}

async fn commit(
    connection: &turso::Connection,
    recording_id: &str,
    hold_id: &str,
    update: Update,
) -> anyhow::Result<Snapshot> {
    let previous = get(connection, recording_id, hold_id).await?;
    anyhow::ensure!(
        previous.as_ref().map(|state| state.revision) == update.expected_revision,
        "hold revision conflict"
    );
    anyhow::ensure!(
        update.active || previous.as_ref().is_some_and(|state| state.active),
        "hold is not active"
    );
    let snapshot = Snapshot {
        revision: update
            .expected_revision
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("hold revision exhausted"))?,
        active: update.active,
        actor: update.actor,
        reason: update.reason,
    };
    let revision = to_i64(snapshot.revision, "hold revision")?;
    prepare_baseline(connection, recording_id, previous.is_none()).await?;
    connection
        .execute(
            "INSERT INTO recording_holds (recording_id, hold_id, revision, active, actor, reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(recording_id, hold_id) DO UPDATE SET revision = excluded.revision,
             active = excluded.active, actor = excluded.actor, reason = excluded.reason",
            turso::params![
                recording_id,
                hold_id,
                revision,
                i64::from(snapshot.active),
                snapshot.actor.clone(),
                snapshot.reason.clone()
            ],
        )
        .await?;
    connection
        .execute(
            "UPDATE recording_files SET protected = (
             SELECT protected FROM recording_hold_baselines WHERE recording_id = ?1
         ) OR EXISTS (SELECT 1 FROM recording_holds WHERE recording_id = ?1 AND active = 1)
         WHERE id = ?1",
            turso::params![recording_id],
        )
        .await?;
    Ok(snapshot)
}

async fn prepare_baseline(
    connection: &turso::Connection,
    id: &str,
    new_hold: bool,
) -> anyhow::Result<()> {
    let mut rows = connection.query(
        "SELECT protected,
            (SELECT COUNT(*) FROM recording_holds WHERE recording_id = ?1),
            EXISTS (SELECT 1 FROM recording_holds WHERE recording_id = ?1 AND active = 1)
         FROM recording_files WHERE id = ?1 AND finalized = 1 AND ended_at_ms IS NOT NULL
            AND cleanup_pending = 0 AND NOT EXISTS
                (SELECT 1 FROM recording_maintenance_claims WHERE recording_id = ?1 AND active = 1)",
        turso::params![id],
    ).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording is unavailable for hold updates"))?;
    let protected: i64 = row.get(0)?;
    let count: i64 = row.get(1)?;
    let active: i64 = row.get(2)?;
    drop(rows);
    anyhow::ensure!(
        !new_hold || count < MAX_HOLDS_PER_RECORDING,
        "recording hold limit reached"
    );
    if active == 0 {
        // ponytail: Reuse the existing cleanup flag and preserve its independent protection.
        connection
            .execute(
                "INSERT INTO recording_hold_baselines (recording_id, protected) VALUES (?1, ?2)
             ON CONFLICT(recording_id) DO UPDATE SET protected = excluded.protected",
                turso::params![id, protected],
            )
            .await?;
    }
    Ok(())
}

pub(super) async fn reject_legacy_update(
    connection: &turso::Connection,
    id: &str,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM recording_holds WHERE recording_id = ?1 AND active = 1 LIMIT 1",
            turso::params![id],
        )
        .await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "release named holds before changing legacy protection"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
