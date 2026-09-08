//! Durable recording-deletion intent, separate from automatic retention claims.
//!
//! Queued intent does not claim a recording or permit filesystem deletion. An executor must
//! independently verify current authorization, file identity, confinement, and evidence relationships.

use super::{Scope, Snapshot, check_deadline, validate_identifier};
use crate::storage::catalog::{
    BUSY_TIMEOUT, Command, RecordingCatalogHandle, current_unix_time_ms,
};
use sha2::{Digest, Sha256};
use std::{fmt, sync::mpsc, time::Instant};

mod ledger;
pub mod preflight;

/// Prepared intentions are short-lived so a lost reply cannot retain them indefinitely.
const PLAN_TTL_MS: i64 = 10 * 60 * 1_000;
/// Keeps both the indexed row count and expiry pruning small on the writer.
const MAX_PLANS: i64 = 128;
/// Completed execution is not enabled; queued intent remains durable until explicitly cancelled.
const MAX_JOBS: i64 = 256;
/// Preserves cancellation evidence across ordinary retry windows before quota recovery.
const CANCEL_RETENTION_MS: i64 = 30 * 86_400_000;
/// Bounds one serialized 128-recording catalog snapshot, including escaped identifiers.
const MAX_SNAPSHOT_BYTES: usize = 512 * 1_024;

/// Distinguishes ordinary operator removal from privacy-motivated removal in the audit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Reason {
    Operator,
    Privacy,
}

/// Describes non-executing deletion intent; no state claims that media was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Prepared,
    Expired,
    Queued,
    Cancelled,
}

/// Tracks work admission without asserting that a recording was claimed or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectState {
    Queued,
    Cancelled,
}

/// Associates durable work state with one recording in the confirmed snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub recording_id: String,
    pub state: ObjectState,
}

/// Selects an authoritative scope at a previously observed catalog revision.
#[derive(Debug, Clone)]
pub struct Intent {
    pub scope: Scope,
    pub expected_revision: u64,
    pub reason: Reason,
}

/// Carries a confirmation secret without exposing it through diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct Nonce(String);

impl Nonce {
    /// Parses the exact hexadecimal nonce format; malformed input is rejected.
    pub fn parse(value: &str) -> Result<Self, Failure> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Failure::Invalid);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn generate() -> Self {
        Self(format!(
            "{:032x}{:032x}",
            rand::random::<u128>(),
            rand::random::<u128>()
        ))
    }

    fn digest(&self) -> String {
        Sha256::digest(self.0.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

impl fmt::Debug for Nonce {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Nonce([REDACTED])")
    }
}

/// Operates on durable intent, not on recording files or retention claims.
#[derive(Debug, Clone)]
pub enum Action {
    Prepare(Intent),
    Read {
        id: String,
    },
    Confirm {
        id: String,
        nonce: Nonce,
        expected_revision: u64,
    },
    Cancel {
        id: String,
    },
}

/// Stores requester, scope, reason, and transition times for one stable job identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub actor: String,
    pub state: State,
    pub reason: Reason,
    pub revision: u64,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub confirmed_at_ms: Option<i64>,
    pub cancelled_at_ms: Option<i64>,
    pub snapshot: Snapshot,
    /// Contains one entry per confirmed recording, in snapshot order; otherwise empty.
    pub objects: Vec<Object>,
    /// Returned only by preparation; persistence retains its SHA-256 digest instead.
    pub confirmation: Option<Nonce>,
}

/// Reports stable failure classes without echoing source identities, credentials, or host paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Invalid,
    NotFound,
    Conflict,
    Blocked,
    Expired,
    InvalidState,
    Quota,
    Unavailable,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid deletion intent",
            Self::NotFound => "deletion intent is unavailable to this actor",
            Self::Conflict => "deletion preview is stale; inspect the scope again",
            Self::Blocked => "deletion scope includes ineligible recordings",
            Self::Expired => "deletion preview has expired",
            Self::InvalidState => "deletion intent cannot make that transition",
            Self::Quota => "deletion intent capacity is exhausted",
            Self::Unavailable => "deletion intent outcome is unknown; query or retry its identity",
        })
    }
}

impl std::error::Error for Failure {}

pub(in crate::storage::catalog) struct Request {
    actor: String,
    action: Action,
    snapshot: Option<Snapshot>,
    deadline: Instant,
}

pub(in crate::storage::catalog) struct Epoch {
    id: String,
    origin: Instant,
}

impl Epoch {
    pub(in crate::storage::catalog) fn new() -> Self {
        Self {
            id: format!("{:032x}", rand::random::<u128>()),
            origin: Instant::now(),
        }
    }

    fn now(&self) -> anyhow::Result<Moment> {
        Ok(Moment {
            epoch: self.id.clone(),
            utc_ms: current_unix_time_ms(),
            elapsed_ms: i64::try_from(self.origin.elapsed().as_millis())
                .map_err(|_| Failure::Expired)?,
        })
    }
}

struct Moment {
    epoch: String,
    utc_ms: i64,
    elapsed_ms: i64,
}

struct Authorization {
    nonce_hash: String,
    epoch: String,
    expires_after_ms: i64,
}

impl RecordingCatalogHandle {
    /// Prepares, confirms, reads, or cancels durable intent without changing recording state.
    ///
    /// The caller must derive the actor from an authenticated Administrator and authorize the
    /// scope. This internal API is not a network authorization boundary. Confirmation is safe to
    /// retry using the same ID, nonce, and revision after a lost reply, including across restart.
    /// A prepared-plan reply lost before confirmation expires without side effects on recordings.
    ///
    /// # Errors
    /// Rejects stale/expired/blocked scopes, wrong owners or nonces, invalid transitions, bounded
    /// quotas and unavailable queues. A reply timeout may have committed intent, but never media work.
    pub fn recording_deletion_intent(&self, actor: &str, action: Action) -> anyhow::Result<Job> {
        validate_action(actor, &action)?;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let snapshot = if let Action::Prepare(intent) = &action {
            let snapshot = self.recording_maintenance_snapshot(intent.scope.clone())?;
            validate_snapshot(&snapshot, intent.expected_revision)?;
            Some(snapshot)
        } else {
            None
        };
        check_deadline(deadline)?;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::DeletionIntent {
                request: Request {
                    actor: actor.to_owned(),
                    action,
                    snapshot,
                    deadline,
                },
                reply,
            })
            .map_err(|_| Failure::Unavailable)?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failure::Unavailable)?
    }
}

fn validate_action(actor: &str, action: &Action) -> anyhow::Result<()> {
    validate_identifier(actor).map_err(|_| Failure::Invalid)?;
    match action {
        Action::Prepare(intent) => {
            intent.scope.validate().map_err(|_| Failure::Invalid)?;
            i64::try_from(intent.expected_revision).map_err(|_| Failure::Invalid)?;
        }
        Action::Read { id } | Action::Cancel { id } | Action::Confirm { id, .. } => {
            anyhow::ensure!(
                id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
                Failure::Invalid
            );
        }
    }
    Ok(())
}

fn validate_snapshot(snapshot: &Snapshot, revision: u64) -> anyhow::Result<()> {
    anyhow::ensure!(snapshot.revision == revision, Failure::Conflict);
    anyhow::ensure!(!snapshot.recordings.is_empty(), Failure::NotFound);
    anyhow::ensure!(
        snapshot.recordings.len() <= super::MAX_RECORDINGS,
        Failure::Invalid
    );
    anyhow::ensure!(
        snapshot.recordings.iter().all(|recording| {
            recording.finalized
                && !recording.protected
                && !recording.cleanup_pending
                && recording.ended_at_ms.is_some()
        }),
        Failure::Blocked
    );
    Ok(())
}

pub(in crate::storage::catalog) async fn initialize(
    connection: &turso::Connection,
) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS recording_maintenance_intents (
            id TEXT PRIMARY KEY, actor TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('prepared', 'queued', 'cancelled')),
            reason TEXT NOT NULL CHECK (reason IN ('operator', 'privacy')),
            revision INTEGER NOT NULL, nonce_hash TEXT NOT NULL,
            epoch TEXT NOT NULL, expires_after_ms INTEGER NOT NULL,
            snapshot_json TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL, expires_at_ms INTEGER NOT NULL,
            confirmed_at_ms INTEGER, cancelled_at_ms INTEGER
         );
         CREATE INDEX IF NOT EXISTS recording_maintenance_intents_expiry
                ON recording_maintenance_intents(state, expires_at_ms);
            CREATE TABLE IF NOT EXISTS recording_maintenance_objects (
                job_id TEXT NOT NULL, ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                recording_id TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('queued', 'cancelled')),
                PRIMARY KEY (job_id, ordinal), UNIQUE (job_id, recording_id)
            );",
        )
        .await?;
    Ok(())
}

pub(in crate::storage::catalog) async fn execute(
    connection: &turso::Connection,
    epoch: &Epoch,
    request: Request,
) -> anyhow::Result<Job> {
    execute_with_clock(connection, request, || epoch.now()).await
}

async fn execute_with_clock(
    connection: &turso::Connection,
    request: Request,
    clock: impl FnOnce() -> anyhow::Result<Moment>,
) -> anyhow::Result<Job> {
    let deadline = request.deadline;
    check_deadline(deadline)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        check_deadline(deadline)?;
        let now = clock()?;
        anyhow::ensure!(now.utc_ms >= 0 && now.elapsed_ms >= 0, Failure::Invalid);
        let job = transition(connection, request, &now).await?;
        check_deadline(deadline)?;
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(job)
    }
    .await;
    if let Err(error) = &result {
        let autocommit = connection.is_autocommit().map_err(|state| {
            anyhow::anyhow!(
                "deletion intent failed: {error}; transaction state unavailable: {state}"
            )
        })?;
        if !autocommit {
            connection
                .execute_batch("ROLLBACK")
                .await
                .map_err(|rollback| {
                    anyhow::anyhow!("deletion intent failed: {error}; rollback failed: {rollback}")
                })?;
        }
    }
    result
}

async fn transition(
    connection: &turso::Connection,
    request: Request,
    now: &Moment,
) -> anyhow::Result<Job> {
    match request.action {
        Action::Prepare(intent) => {
            prepare(
                connection,
                &request.actor,
                intent,
                request.snapshot,
                now,
                request.deadline,
            )
            .await
        }
        Action::Read { id } => {
            let (mut job, authorization) =
                load(connection, &request.actor, &id, request.deadline).await?;
            if job.state == State::Prepared && !plan_is_live(&job, &authorization, now) {
                job.state = State::Expired;
            }
            Ok(job)
        }
        Action::Confirm {
            id,
            nonce,
            expected_revision,
        } => {
            confirm(
                connection,
                &request.actor,
                &id,
                &nonce,
                expected_revision,
                now,
                request.deadline,
            )
            .await
        }
        Action::Cancel { id } => {
            cancel(
                connection,
                &request.actor,
                &id,
                now.utc_ms,
                request.deadline,
            )
            .await
        }
    }
}

async fn prepare(
    connection: &turso::Connection,
    actor: &str,
    intent: Intent,
    snapshot: Option<Snapshot>,
    now: &Moment,
    deadline: Instant,
) -> anyhow::Result<Job> {
    let snapshot = snapshot.ok_or(Failure::Invalid)?;
    validate_snapshot(&snapshot, intent.expected_revision)?;
    anyhow::ensure!(snapshot.scope == intent.scope, Failure::Invalid);
    check_revision(connection, intent.expected_revision).await?;
    prune(connection, now, deadline).await?;
    check_quota(connection, true, MAX_PLANS).await?;
    let serialized = serde_json::to_string(&snapshot)?;
    anyhow::ensure!(serialized.len() <= MAX_SNAPSHOT_BYTES, Failure::Invalid);
    let nonce = Nonce::generate();
    let id = format!("{:032x}", rand::random::<u128>());
    let expires_at_ms = now
        .utc_ms
        .checked_add(PLAN_TTL_MS)
        .ok_or(Failure::Invalid)?;
    let expires_after_ms = now
        .elapsed_ms
        .checked_add(PLAN_TTL_MS)
        .ok_or(Failure::Invalid)?;
    let reason = match intent.reason {
        Reason::Operator => "operator",
        Reason::Privacy => "privacy",
    };
    connection.execute(
        "INSERT INTO recording_maintenance_intents
            (id, actor, state, reason, revision, nonce_hash, snapshot_json, created_at_ms, expires_at_ms,
             epoch, expires_after_ms)
         VALUES (?1, ?2, 'prepared', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        turso::params![id.as_str(), actor, reason, i64::try_from(snapshot.revision)?, nonce.digest(),
            serialized, now.utc_ms, expires_at_ms, now.epoch.as_str(), expires_after_ms],
    ).await?;
    let mut job = load(connection, actor, &id, deadline).await?.0;
    job.confirmation = Some(nonce);
    Ok(job)
}

async fn prune(
    connection: &turso::Connection,
    now: &Moment,
    deadline: Instant,
) -> anyhow::Result<()> {
    let expired = "(state = 'prepared' AND
        (expires_at_ms <= ?1 OR epoch != ?3 OR expires_after_ms <= ?4)) OR
        (state = 'cancelled' AND cancelled_at_ms <= ?2)";
    let statements = [
        format!(
            "DELETE FROM recording_maintenance_objects WHERE job_id IN
            (SELECT id FROM recording_maintenance_intents WHERE {expired})"
        ),
        format!("DELETE FROM recording_maintenance_intents WHERE {expired}"),
    ];
    for statement in statements {
        check_deadline(deadline)?;
        connection
            .execute(
                &statement,
                turso::params![
                    now.utc_ms,
                    now.utc_ms.saturating_sub(CANCEL_RETENTION_MS),
                    now.epoch.as_str(),
                    now.elapsed_ms
                ],
            )
            .await?;
    }
    Ok(())
}

async fn check_revision(connection: &turso::Connection, revision: u64) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT revision FROM recording_catalog_state WHERE id = 1",
            (),
        )
        .await?;
    let row = rows.next().await?.ok_or(Failure::Conflict)?;
    anyhow::ensure!(
        row.get::<i64>(0)? == i64::try_from(revision).map_err(|_| Failure::Conflict)?,
        Failure::Conflict
    );
    Ok(())
}

async fn check_quota(
    connection: &turso::Connection,
    prepared: bool,
    maximum: i64,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT count(*) FROM recording_maintenance_intents WHERE (state = 'prepared') = ?1",
            turso::params![i64::from(prepared)],
        )
        .await?;
    let row = rows.next().await?.ok_or(Failure::Quota)?;
    anyhow::ensure!(row.get::<i64>(0)? < maximum, Failure::Quota);
    Ok(())
}

async fn load(
    connection: &turso::Connection,
    actor: &str,
    id: &str,
    deadline: Instant,
) -> anyhow::Result<(Job, Authorization)> {
    let mut rows = connection.query(
        "SELECT state, reason, revision, nonce_hash, snapshot_json, created_at_ms, expires_at_ms,
            confirmed_at_ms, cancelled_at_ms, epoch, expires_after_ms
         FROM recording_maintenance_intents WHERE id = ?1 AND actor = ?2",
        turso::params![id, actor],
    ).await?;
    let row = rows.next().await?.ok_or(Failure::NotFound)?;
    let serialized: String = row.get(4)?;
    anyhow::ensure!(serialized.len() <= MAX_SNAPSHOT_BYTES, Failure::Invalid);
    let state = match row.get::<String>(0)?.as_str() {
        "prepared" => State::Prepared,
        "queued" => State::Queued,
        "cancelled" => State::Cancelled,
        _ => return Err(Failure::Invalid.into()),
    };
    let reason = match row.get::<String>(1)?.as_str() {
        "operator" => Reason::Operator,
        "privacy" => Reason::Privacy,
        _ => return Err(Failure::Invalid.into()),
    };
    let revision = u64::try_from(row.get::<i64>(2)?).map_err(|_| Failure::Invalid)?;
    let snapshot: Snapshot = serde_json::from_str(&serialized)?;
    validate_snapshot(&snapshot, revision)?;
    let (mut job, authorization) = (
        Job {
            id: id.to_owned(),
            actor: actor.to_owned(),
            state,
            reason,
            revision,
            created_at_ms: row.get(5)?,
            expires_at_ms: row.get(6)?,
            confirmed_at_ms: row.get(7)?,
            cancelled_at_ms: row.get(8)?,
            snapshot,
            objects: Vec::new(),
            confirmation: None,
        },
        Authorization {
            nonce_hash: row.get(3)?,
            epoch: row.get(9)?,
            expires_after_ms: row.get(10)?,
        },
    );
    drop(rows);
    job.objects = ledger::read(connection, &job, deadline).await?;
    Ok((job, authorization))
}

async fn confirm(
    connection: &turso::Connection,
    actor: &str,
    id: &str,
    nonce: &Nonce,
    revision: u64,
    now: &Moment,
    deadline: Instant,
) -> anyhow::Result<Job> {
    let (job, authorization) = load(connection, actor, id, deadline).await?;
    anyhow::ensure!(
        nonce.digest() == authorization.nonce_hash,
        Failure::NotFound
    );
    anyhow::ensure!(revision == job.revision, Failure::Conflict);
    if job.state == State::Queued {
        return Ok(job);
    }
    anyhow::ensure!(job.state == State::Prepared, Failure::InvalidState);
    anyhow::ensure!(plan_is_live(&job, &authorization, now), Failure::Expired);
    check_revision(connection, revision).await?;
    check_quota(connection, false, MAX_JOBS).await?;
    ledger::enqueue(connection, &job, deadline).await?;
    connection.execute(
        "UPDATE recording_maintenance_intents SET state = 'queued', confirmed_at_ms = ?1 WHERE id = ?2",
        turso::params![now.utc_ms, id],
    ).await?;
    Ok(load(connection, actor, id, deadline).await?.0)
}

fn plan_is_live(job: &Job, authorization: &Authorization, now: &Moment) -> bool {
    now.epoch == authorization.epoch
        && now.elapsed_ms >= 0
        && now.elapsed_ms < authorization.expires_after_ms
        && now.utc_ms >= job.created_at_ms
        && now.utc_ms < job.expires_at_ms
}

async fn cancel(
    connection: &turso::Connection,
    actor: &str,
    id: &str,
    now_ms: i64,
    deadline: Instant,
) -> anyhow::Result<Job> {
    let (job, _) = load(connection, actor, id, deadline).await?;
    if job.state == State::Cancelled {
        return Ok(job);
    }
    anyhow::ensure!(now_ms >= job.created_at_ms, Failure::Invalid);
    anyhow::ensure!(
        job.confirmed_at_ms
            .is_none_or(|confirmed| now_ms >= confirmed),
        Failure::Invalid
    );
    if job.state == State::Prepared {
        check_quota(connection, false, MAX_JOBS).await?;
    }
    connection
        .execute(
            "UPDATE recording_maintenance_objects SET state = 'cancelled' WHERE job_id = ?1",
            turso::params![id],
        )
        .await?;
    connection.execute(
        "UPDATE recording_maintenance_intents SET state = 'cancelled', cancelled_at_ms = ?1 WHERE id = ?2",
        turso::params![now_ms, id],
    ).await?;
    Ok(load(connection, actor, id, deadline).await?.0)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod persistence_tests {
    use super::{Action, Intent, Job, Reason, State};
    use crate::storage::catalog::maintenance::Scope;
    use crate::storage::catalog::{CatalogRecording, RecordingCatalog};

    fn prepared_recording() -> (std::path::PathBuf, RecordingCatalog, Job) {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-maintenance-jobs-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let media = root.join("recording.mp4");
        std::fs::write(&media, [42; 64]).unwrap();
        let database = root.join("recordings.db");
        let catalog = RecordingCatalog::open(&database).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front/sub".to_owned(),
                source_id: Some("front".to_owned()),
                logical_stream_id: Some("sub".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(2_000),
                path: media.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 8,
                finalized: true,
            })
            .unwrap();
        handle
            .update_recording_path("recording-1", &media, true)
            .unwrap();
        let snapshot = handle
            .recording_maintenance_snapshot(Scope::Recording {
                source_id: "front".to_owned(),
                stream_id: "sub".to_owned(),
                recording_id: "recording-1".to_owned(),
            })
            .unwrap();
        let prepared = handle
            .recording_deletion_intent(
                "administrator",
                Action::Prepare(Intent {
                    scope: snapshot.scope,
                    expected_revision: snapshot.revision,
                    reason: Reason::Operator,
                }),
            )
            .unwrap();
        (root, catalog, prepared)
    }

    #[test]
    fn lost_confirmation_reply_preserves_one_durable_ledger() {
        let (root, catalog, prepared) = prepared_recording();
        let confirm = Action::Confirm {
            id: prepared.id.clone(),
            nonce: prepared.confirmation.unwrap(),
            expected_revision: prepared.revision,
        };
        let handle = catalog.handle();
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        drop(response);
        handle
            .tx
            .try_send(super::Command::DeletionIntent {
                request: super::Request {
                    actor: "administrator".to_owned(),
                    action: confirm.clone(),
                    snapshot: None,
                    deadline: std::time::Instant::now() + super::BUSY_TIMEOUT,
                },
                reply,
            })
            .unwrap_or_else(|error| panic!("fixture writer queue rejected confirmation: {error}"));
        let committed = handle
            .recording_deletion_intent("administrator", Action::Read { id: prepared.id })
            .unwrap();
        assert_eq!(committed.state, State::Queued);
        assert_eq!(committed.objects.len(), 1);
        assert_eq!(
            handle
                .recording_deletion_intent("administrator", confirm.clone())
                .unwrap(),
            committed
        );
        drop(handle);
        catalog.shutdown();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        assert_eq!(
            catalog
                .handle()
                .recording_deletion_intent("administrator", confirm)
                .unwrap(),
            committed
        );
        assert_eq!(std::fs::read(root.join("recording.mp4")).unwrap(), [42; 64]);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn confirmation_is_actor_bound_idempotent_and_durable_without_deleting_media() {
        let (root, catalog, prepared) = prepared_recording();
        let database = root.join("recordings.db");
        let media = root.join("recording.mp4");
        let handle = catalog.handle();
        assert_eq!(prepared.state, State::Prepared);
        let confirm = Action::Confirm {
            id: prepared.id.clone(),
            nonce: prepared.confirmation.as_ref().unwrap().clone(),
            expected_revision: prepared.revision,
        };
        assert!(
            handle
                .recording_deletion_intent("other", confirm.clone())
                .is_err()
        );
        let queued = handle
            .recording_deletion_intent("administrator", confirm.clone())
            .unwrap();
        assert_eq!(queued.state, State::Queued);
        assert_eq!(
            handle
                .recording_deletion_intent("administrator", confirm.clone())
                .unwrap(),
            queued
        );
        assert!(handle.pending_cleanup_candidate().unwrap().is_none());
        assert_eq!(std::fs::read(&media).unwrap(), [42; 64]);
        drop(handle);
        catalog.shutdown();

        let catalog = RecordingCatalog::open(&database).unwrap();
        let recovered = catalog
            .handle()
            .recording_deletion_intent("administrator", confirm)
            .unwrap();
        assert_eq!(recovered, queued);
        assert_eq!(std::fs::read(&media).unwrap(), [42; 64]);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_intent_survives_restart_without_recording_mutation() {
        let (root, catalog, prepared) = prepared_recording();
        let cancel = Action::Cancel { id: prepared.id };
        let cancelled = catalog
            .handle()
            .recording_deletion_intent("administrator", cancel.clone())
            .unwrap();
        assert_eq!(cancelled.state, State::Cancelled);
        catalog.shutdown();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let retried = catalog
            .handle()
            .recording_deletion_intent("administrator", cancel)
            .unwrap();
        assert_eq!(retried, cancelled);
        assert!(
            catalog
                .handle()
                .pending_cleanup_candidate()
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read(root.join("recording.mp4")).unwrap(), [42; 64]);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
}
