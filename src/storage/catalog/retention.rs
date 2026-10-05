//! Durable retention obligations. These records do not authorize file deletion.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::RecordingCatalogHandle;
use crate::storage::metadata::TimelineEvent;
use crate::storage::retention::{Interval, MAX_EVENTS, MAX_RULES, Policy, Reason, Recording};

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

pub(super) fn connect(database: &turso::Database) -> Result<Arc<Mutex<turso::Connection>>> {
    let connection = database.connect()?;
    connection.busy_timeout(super::BUSY_TIMEOUT)?;
    pollster::block_on(connection.execute_batch("PRAGMA foreign_keys = ON"))?;
    Ok(Arc::new(Mutex::new(connection)))
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
    Ok(())
}

impl RecordingCatalogHandle {
    /// Commits current canonical evidence without shortening an existing deadline.
    /// Revisions must increase when rules change. This does not activate a policy or fence cleanup.
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
            .try_lock()
            .map_err(|_| anyhow::anyhow!("retention catalog is busy or poisoned"))?;
        pollster::block_on(async {
            connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
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
            .try_lock()
            .map_err(|_| anyhow::anyhow!("retention catalog is busy or poisoned"))?;
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

async fn commit(
    connection: &turso::Connection,
    id: &str,
    revision: u64,
    policy: &Policy,
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
    let events = matching_events(connection, &snapshot).await?;
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
                           WHERE recording_id = ?1 AND active = 1)",
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
    let mut rows = connection
        .query(
            "SELECT substr(id, 1, 257), camera_id, stream, substr(source, 1, 65), substr(kind, 1, 257),
                start_time_ms, end_time_ms, NULL, NULL, NULL, NULL, revision,
                NULL, '[]', NULL, '', NULL, NULL, NULL
         FROM recording_events WHERE camera_id = ?1 AND (stream IS NULL OR stream = ?2)
           AND start_time_ms < ?4
           AND (end_time_ms IS NULL OR end_time_ms > ?3
                OR (end_time_ms = start_time_ms AND start_time_ms >= ?3))
         ORDER BY start_time_ms, id LIMIT ?5",
            turso::params![
                snapshot.camera_id.as_str(),
                snapshot.stream_id.as_str(),
                snapshot.interval.start_ms(),
                snapshot.interval.end_ms(),
                (MAX_EVENTS + 1) as i64
            ],
        )
        .await?;
    let mut events = Vec::with_capacity(MAX_EVENTS);
    while let Some(row) = rows.next().await? {
        ensure!(
            events.len() < MAX_EVENTS,
            "retention event snapshot limit exceeded"
        );
        let event = super::event_from_row(&row, false)?;
        validate_identity(&event.id)?;
        validate_identity(&event.kind)?;
        events.push(event);
    }
    Ok(events)
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
