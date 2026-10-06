//! Durable retention obligations. These records do not authorize file deletion.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::RecordingCatalogHandle;
use crate::storage::metadata::TimelineEvent;
use crate::storage::retention::{Interval, MAX_EVENTS, MAX_RULES, Policy, Reason, Recording};

pub(super) mod event_index;
pub mod runtime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDecision {
    pub recording_id: String,
    pub policy_revision: u64,
    pub event_revision: u64,
    pub deadline_ms: Option<i64>,
    pub matching_rules: Vec<String>,
    pub reason: Reason,
}

struct Previous {
    decision: StoredDecision,
    fingerprint: Vec<u8>,
}

struct Snapshot {
    camera_id: String,
    stream_id: String,
    interval: Interval,
    protected: bool,
}

pub(super) struct Connection {
    connection: Mutex<turso::Connection>,
    authority: Arc<super::authority::Lease>,
}

pub(super) fn connect(
    database: &turso::Database,
    authority: Arc<super::authority::Lease>,
) -> Result<Arc<Connection>> {
    let connection = database.connect()?;
    connection.busy_timeout(super::BUSY_TIMEOUT)?;
    pollster::block_on(connection.execute_batch("PRAGMA foreign_keys = ON"))?;
    Ok(Arc::new(Connection {
        connection: Mutex::new(connection),
        authority,
    }))
}

pub(super) async fn initialize(connection: &turso::Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS recording_retention_decisions (
             recording_id TEXT PRIMARY KEY REFERENCES recording_files(id) ON DELETE CASCADE,
             policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
             policy_fingerprint BLOB NOT NULL CHECK (length(policy_fingerprint) = 32),
             event_revision INTEGER NOT NULL CHECK (event_revision >= 0),
             deadline_ms INTEGER,
             matching_rules_json TEXT NOT NULL CHECK (length(matching_rules_json) <= 4096),
             reason_json TEXT NOT NULL CHECK (length(reason_json) <= 64)
         );
         CREATE INDEX IF NOT EXISTS recording_retention_deadline
             ON recording_retention_decisions(deadline_ms, recording_id);
         CREATE TABLE IF NOT EXISTS recording_retention_event_state (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             revision INTEGER NOT NULL CHECK (typeof(revision) = 'integer' AND revision >= 0)
         );
         INSERT OR IGNORE INTO recording_retention_event_state VALUES (1, 0);
         CREATE TRIGGER IF NOT EXISTS recording_retention_event_insert
         AFTER INSERT ON recording_events BEGIN
             UPDATE recording_retention_event_state SET revision = revision + 1 WHERE id = 1;
         END;
         CREATE TRIGGER IF NOT EXISTS recording_retention_event_update
         AFTER UPDATE ON recording_events BEGIN
             UPDATE recording_retention_event_state SET revision = revision + 1 WHERE id = 1;
         END;
         CREATE TRIGGER IF NOT EXISTS recording_retention_event_delete
         AFTER DELETE ON recording_events BEGIN
             UPDATE recording_retention_event_state SET revision = revision + 1 WHERE id = 1;
         END;",
        )
        .await?;
    event_index::initialize(connection).await?;
    runtime::initialize(connection).await?;
    Ok(())
}

impl RecordingCatalogHandle {
    /// Rebuilds at most 256 temporal index entries. Returns true when the index is current.
    /// This maintains derived metadata and does not authorize recording deletion.
    pub fn reconcile_retention_events(&self, max_rows: usize) -> Result<bool> {
        ensure!(
            (1..=MAX_EVENTS).contains(&max_rows),
            "retention event index batch must be 1 to 256"
        );
        let started = Instant::now();
        self.check_retention_available(started)?;
        let owner = self
            .retention
            .upgrade()
            .context("retention catalog is closed")?;
        let connection = owner
            .connection
            .try_lock()
            .map_err(|_| anyhow::anyhow!("retention catalog is busy or poisoned"))?;
        pollster::block_on(async {
            connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                owner.authority.verify_transaction(&connection)?;
                let complete = event_index::reconcile(&connection, max_rows).await?;
                self.check_retention_available(started)?;
                connection.execute_batch("COMMIT").await?;
                Ok(complete)
            }
            .await;
            if result.is_err() {
                connection
                    .execute_batch("ROLLBACK")
                    .await
                    .context("rollback retention event index")?;
            }
            result
        })
    }

    /// Commits current canonical evidence without shortening an existing deadline.
    /// Revisions must increase when rules change. This does not activate a policy.
    /// Automatic cleanup preserves committed deadlines until they expire.
    pub fn commit_retention(
        &self,
        recording_id: &str,
        policy_revision: u64,
        policy: &Policy,
    ) -> Result<StoredDecision> {
        validate_identity(recording_id)?;
        ensure!(
            policy_revision > 0,
            "retention policy revision must be positive"
        );
        super::to_i64(policy_revision, "retention policy revision")?;
        let started = Instant::now();
        self.check_retention_available(started)?;
        // ponytail: One catalog transaction replaces a separate retention worker and queue.
        let owner = self
            .retention
            .upgrade()
            .context("retention catalog is closed")?;
        let connection = owner
            .connection
            .try_lock()
            .map_err(|_| anyhow::anyhow!("retention catalog is busy or poisoned"))?;
        pollster::block_on(async {
            connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                owner.authority.verify_transaction(&connection)?;
                let decision = commit(&connection, recording_id, policy_revision, policy).await?;
                self.check_retention_available(started)?;
                connection.execute_batch("COMMIT").await?;
                Ok(decision)
            }
            .await;
            if result.is_err() {
                connection
                    .execute_batch("ROLLBACK")
                    .await
                    .context("rollback retention commitment")?;
            }
            result
        })
    }

    /// Reads an audit decision; current holds require separate authorization.
    pub fn retention_decision(&self, recording_id: &str) -> Result<Option<StoredDecision>> {
        validate_identity(recording_id)?;
        let started = Instant::now();
        self.check_retention_available(started)?;
        let owner = self
            .retention
            .upgrade()
            .context("retention catalog is closed")?;
        let connection = owner
            .connection
            .try_lock()
            .map_err(|_| anyhow::anyhow!("retention catalog is busy or poisoned"))?;
        owner.authority.verify(&connection)?;
        let previous = pollster::block_on(read_previous(&connection, recording_id));
        self.check_retention_available(started)?;
        Ok(previous?.map(|previous| previous.decision))
    }

    fn check_retention_available(&self, started: Instant) -> Result<()> {
        ensure!(
            !self.retention_shutdown.load(Ordering::Acquire),
            "recording catalog is unavailable"
        );
        ensure!(
            started.elapsed() < super::BUSY_TIMEOUT,
            "retention transaction time budget exceeded"
        );
        Ok(())
    }
}

fn validate_identity(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 256 && !value.contains('\0'),
        "retention identity must contain 1 to 256 bytes without NUL"
    );
    Ok(())
}

pub(super) fn now_ms() -> Result<i64> {
    Ok(i64::try_from(
        time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000,
    )?)
}

pub(super) async fn ensure_cleanup_allowed(connection: &turso::Connection, id: &str) -> Result<()> {
    if let Some(previous) = read_previous(connection, id).await? {
        let now = now_ms()?;
        ensure!(
            previous
                .decision
                .deadline_ms
                .is_none_or(|deadline| deadline <= now),
            "committed retention deadline prevents automatic cleanup"
        );
    }
    Ok(())
}

async fn commit(
    connection: &turso::Connection,
    id: &str,
    revision: u64,
    policy: &Policy,
) -> Result<StoredDecision> {
    commit_snapshot(connection, id, revision, policy, true).await
}

async fn commit_snapshot(
    connection: &turso::Connection,
    id: &str,
    revision: u64,
    policy: &Policy,
    requires_events: bool,
) -> Result<StoredDecision> {
    let snapshot = recording_snapshot(connection, id).await?;
    let previous = read_previous(connection, id).await?;
    let fingerprint = Sha256::digest(serde_json::to_vec(policy)?);
    if let Some(previous) = &previous {
        ensure!(
            revision >= previous.decision.policy_revision,
            "stale retention policy revision"
        );
        ensure!(
            revision != previous.decision.policy_revision
                || previous.fingerprint.as_slice() == fingerprint.as_slice(),
            "retention policy revision was reused with different rules"
        );
    }
    let events = if requires_events {
        matching_events(connection, &snapshot).await?
    } else {
        Vec::new()
    };
    let event_revision = event_revision(connection).await?;
    let resolved = policy.resolve(
        Recording {
            camera_id: &snapshot.camera_id,
            stream_id: &snapshot.stream_id,
            interval: snapshot.interval,
            protected: snapshot.protected,
            committed_deadline_ms: previous.and_then(|previous| previous.decision.deadline_ms),
        },
        &events,
    )?;
    let decision = StoredDecision {
        recording_id: id.to_owned(),
        policy_revision: revision,
        event_revision,
        deadline_ms: resolved.deadline_ms,
        matching_rules: resolved
            .matching_rules
            .into_iter()
            .map(str::to_owned)
            .collect(),
        reason: resolved.reason,
    };
    write_decision(connection, &decision, fingerprint.as_slice()).await?;
    Ok(decision)
}

async fn event_revision(connection: &turso::Connection) -> Result<u64> {
    let mut rows = connection
        .query(
            "SELECT revision FROM recording_retention_event_state WHERE id = 1",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .context("retention event revision is missing")?;
    super::to_u64(row.get(0)?, "retention event revision")
}

async fn recording_snapshot(connection: &turso::Connection, id: &str) -> Result<Snapshot> {
    let mut rows = connection
        .query(
            "SELECT substr(source_id, 1, 257), substr(logical_stream_id, 1, 257),
                started_at_ms, ended_at_ms, protected
         FROM recording_files WHERE id = ?1 AND finalized = 1 AND cleanup_pending = 0
           AND NOT EXISTS (SELECT 1 FROM recording_maintenance_claims
                           WHERE recording_id = ?1 AND active = 1)
           AND NOT EXISTS (SELECT 1 FROM storage_recording_retirements
                           WHERE recording_id = ?1 AND complete = 0)",
            turso::params![id],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .context("retention requires an unclaimed finalized recording")?;
    let camera_id: String = row
        .get::<Option<String>>(0)?
        .context("recording camera identity is missing")?;
    let stream_id: String = row
        .get::<Option<String>>(1)?
        .context("recording stream identity is missing")?;
    validate_identity(&camera_id)?;
    validate_identity(&stream_id)?;
    let end = row
        .get::<Option<i64>>(3)?
        .context("finalized recording end is missing")?;
    Ok(Snapshot {
        camera_id,
        stream_id,
        interval: Interval::new(row.get(2)?, end)?,
        protected: row.get::<i64>(4)? != 0,
    })
}

async fn matching_events(
    connection: &turso::Connection,
    snapshot: &Snapshot,
) -> Result<Vec<TimelineEvent>> {
    event_index::matching(connection, snapshot).await
}

async fn read_previous(connection: &turso::Connection, id: &str) -> Result<Option<Previous>> {
    let mut rows = connection
        .query(
            "SELECT policy_revision, event_revision, deadline_ms,
                substr(matching_rules_json, 1, 4097), substr(reason_json, 1, 65),
                substr(policy_fingerprint, 1, 33)
         FROM recording_retention_decisions WHERE recording_id = ?1",
            turso::params![id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let matching_json: String = row.get(3)?;
    let reason_json: String = row.get(4)?;
    ensure!(
        matching_json.len() <= 4096 && reason_json.len() <= 64,
        "retention metadata limit exceeded"
    );
    let matching_rules: Vec<String> = serde_json::from_str(&matching_json)?;
    ensure!(
        matching_rules.len() <= MAX_RULES,
        "retention rule metadata limit exceeded"
    );
    for rule in &matching_rules {
        crate::storage::retention::validate_selector(rule)?;
    }
    let policy_revision = super::to_u64(row.get(0)?, "retention policy revision")?;
    ensure!(
        policy_revision > 0,
        "retention policy revision must be positive"
    );
    let fingerprint: Vec<u8> = row.get(5)?;
    ensure!(
        fingerprint.len() == 32,
        "invalid retention policy fingerprint"
    );
    Ok(Some(Previous {
        decision: StoredDecision {
            recording_id: id.to_owned(),
            policy_revision,
            event_revision: super::to_u64(row.get(1)?, "retention event revision")?,
            deadline_ms: row.get(2)?,
            matching_rules,
            reason: serde_json::from_str(&reason_json)?,
        },
        fingerprint,
    }))
}

async fn write_decision(
    connection: &turso::Connection,
    decision: &StoredDecision,
    fingerprint: &[u8],
) -> Result<()> {
    connection.execute(
        "INSERT INTO recording_retention_decisions
             (recording_id, policy_revision, policy_fingerprint, event_revision,
              deadline_ms, matching_rules_json, reason_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(recording_id) DO UPDATE SET policy_revision = excluded.policy_revision,
             policy_fingerprint = excluded.policy_fingerprint, event_revision = excluded.event_revision,
             deadline_ms = excluded.deadline_ms, matching_rules_json = excluded.matching_rules_json,
             reason_json = excluded.reason_json",
        turso::params![decision.recording_id.as_str(), super::to_i64(decision.policy_revision, "retention policy revision")?, fingerprint,
            super::to_i64(decision.event_revision, "retention event revision")?, decision.deadline_ms,
            serde_json::to_string(&decision.matching_rules)?, serde_json::to_string(&decision.reason)?],
    ).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::RecordingCatalog;

    fn cleanup_recording(
        root: &std::path::Path,
        id: &str,
        start: i64,
    ) -> super::super::CatalogRecording {
        let file = root.join(format!("{id}.mp4"));
        std::fs::write(&file, b"media").unwrap();
        super::super::CatalogRecording {
            id: id.into(),
            stream_id: "front/main".into(),
            source_id: Some("front".into()),
            logical_stream_id: Some("main".into()),
            started_at_ms: start,
            ended_at_ms: Some(start + 100),
            path: file.to_string_lossy().into_owned(),
            init_offset: 0,
            init_len: 0,
            finalized: true,
        }
    }

    #[test]
    fn automatic_cleanup_preserves_committed_deadlines_across_restart() {
        use crate::storage::retention::{Policy, Predicate, Rule};
        let root = std::env::temp_dir().join(format!("retention-cleanup-{}", uuid::Uuid::new_v4()));
        let path = root.join("catalog.db");
        let catalog = RecordingCatalog::open(&path).unwrap();
        let handle = catalog.handle();
        for (id, start) in [("held", 1000), ("eligible", 2000)] {
            handle
                .upsert_recording(cleanup_recording(&root, id, start))
                .unwrap();
        }
        let policy = Policy::new(vec![
            Rule::new("continuous", i64::MAX as u64 - 1100, Predicate::Continuous).unwrap(),
        ])
        .unwrap();
        let before = handle.commit_retention("held", 1, &policy).unwrap();
        {
            let owner = handle.retention.upgrade().unwrap();
            let connection = owner.connection.lock().unwrap();
            // Simulate an older cleanup admission so ordinary cancellation stays available.
            pollster::block_on(
                connection
                    .execute_batch("UPDATE recording_files SET cleanup_pending=1 WHERE id='held'"),
            )
            .unwrap();
        }
        catalog.shutdown();
        let catalog = RecordingCatalog::open(&path).unwrap();
        let handle = catalog.handle();
        assert_eq!(
            handle
                .pending_cleanup_candidate()
                .unwrap()
                .unwrap()
                .recording_id,
            "held"
        );
        handle.cancel_cleanup("held").unwrap();
        let candidate = handle.claim_cleanup_candidate().unwrap().unwrap();
        assert_eq!(candidate.recording_id, "eligible");
        let store = crate::storage::long_term::LongTermStore::new(root.clone());
        assert_eq!(store.remove_catalog_recording(&candidate.path).unwrap(), 5);
        handle
            .complete_cleanup(
                "eligible",
                super::super::CatalogDeletionReason::ArchiveLimit,
            )
            .unwrap();
        assert!(!root.join("eligible.mp4").exists());
        assert!(handle.claim_cleanup_candidate().unwrap().is_none());
        assert_eq!(std::fs::read(root.join("held.mp4")).unwrap(), b"media");
        assert_eq!(handle.retention_decision("held").unwrap(), Some(before));
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_retention_metadata_blocks_cleanup_and_recovers_after_repair() {
        use crate::storage::retention::{Policy, Predicate, Rule};
        let root = std::env::temp_dir().join(format!(
            "retention-invalid-cleanup-{}",
            uuid::Uuid::new_v4()
        ));
        let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
        let handle = catalog.handle();
        let file = root.join("recording.mp4");
        handle
            .upsert_recording(cleanup_recording(&root, "recording", 1000))
            .unwrap();
        let policy = Policy::new(vec![
            Rule::new("continuous", 1, Predicate::Continuous).unwrap(),
        ])
        .unwrap();
        let before = handle.commit_retention("recording", 1, &policy).unwrap();
        let owner = handle.retention.upgrade().unwrap();
        {
            let connection = owner.connection.lock().unwrap();
            pollster::block_on(connection.execute_batch(
                "UPDATE recording_retention_decisions SET matching_rules_json='broken' WHERE recording_id='recording'",
            )).unwrap();
        }
        assert!(handle.claim_cleanup_candidate().is_err());
        assert!(handle.pending_cleanup_candidate().unwrap().is_none());
        assert_eq!(std::fs::read(&file).unwrap(), b"media");
        {
            let connection = owner.connection.lock().unwrap();
            pollster::block_on(connection.execute_batch(
                "UPDATE recording_retention_decisions SET matching_rules_json='[\"continuous\"]' WHERE recording_id='recording'",
            )).unwrap();
        }
        assert_eq!(
            handle.retention_decision("recording").unwrap(),
            Some(before)
        );
        assert_eq!(
            handle
                .claim_cleanup_candidate()
                .unwrap()
                .unwrap()
                .recording_id,
            "recording"
        );
        drop(owner);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn in_flight_retention_keeps_the_catalog_authority_until_completion() {
        let root = std::env::temp_dir().join(format!("retention-lease-{}", uuid::Uuid::new_v4()));
        let path = root.join("catalog.db");
        let catalog = RecordingCatalog::open(&path).unwrap();
        let handle = catalog.handle();
        let in_flight = handle.retention.upgrade().unwrap();
        catalog.shutdown();
        assert!(RecordingCatalog::open(&path).is_err());
        assert!(handle.retention_decision("recording").is_err());
        drop(in_flight);
        let reopened = RecordingCatalog::open(&path).unwrap();
        reopened.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn fenced_authority_rejects_retention_reads_and_commits() {
        let root = std::env::temp_dir().join(format!("retention-fence-{}", uuid::Uuid::new_v4()));
        let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
        let handle = catalog.handle();
        let owner = handle.retention.upgrade().unwrap();
        {
            let connection = owner.connection.lock().unwrap();
            let destination =
                super::super::authority::Lease::acquire(&root.join("destination.db")).unwrap();
            let generation = owner.authority.verify(&connection).unwrap().generation;
            owner
                .authority
                .fence(
                    &connection,
                    generation,
                    &uuid::Uuid::new_v4().to_string(),
                    &destination,
                )
                .unwrap();
        }
        let policy = crate::storage::retention::Policy::new(Vec::new()).unwrap();
        let error = handle
            .commit_retention("recording", 1, &policy)
            .unwrap_err();
        assert!(error.to_string().contains("fenced"), "{error:#}");
        let error = handle.retention_decision("recording").unwrap_err();
        assert!(error.to_string().contains("fenced"), "{error:#}");
        {
            let connection = owner.connection.lock().unwrap();
            assert!(connection.is_autocommit().unwrap());
            pollster::block_on(connection.execute_batch(
                "UPDATE recording_catalog_authority SET state = 0, handoff = NULL,
                 destination = NULL, destination_lock = NULL WHERE singleton = 1",
            ))
            .unwrap();
        }
        assert!(handle.retention_decision("recording").unwrap().is_none());
        drop(owner);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn surviving_handles_do_not_keep_the_database_open_after_shutdown() {
        let root =
            std::env::temp_dir().join(format!("retention-shutdown-{}", uuid::Uuid::new_v4()));
        let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
        let handle = catalog.handle();
        assert!(handle.retention.upgrade().is_some());
        catalog.shutdown();
        assert!(handle.retention.upgrade().is_none());
        assert!(handle.retention_decision("recording").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
