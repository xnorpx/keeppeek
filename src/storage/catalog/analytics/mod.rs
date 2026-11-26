//! Versioned incremental rollups, completeness ledgers, and shadow rebuilds in the catalog.

use super::{RecordingCatalogHandle, current_unix_time_ms, to_i64, to_u64};
use crate::storage::analytics::{
    ANALYTICS_SCHEMA_VERSION, ActivityAnalyticsStatus, ActivitySummaryQuery, ActivitySummaryResult,
    DEFAULT_MAX_ROLLUP_ROWS, ExpectedSourceState, MAX_DELETION_EVIDENCE_ROWS,
    MAX_REBUILD_BATCH_ROWS, MAX_SOURCE_HEALTH_ROWS, RollupState, WeekStart, time,
};
use anyhow::{Context, Result, ensure};
use std::sync::mpsc;

pub(super) mod delta;
pub(super) mod query;
mod query_agg;
pub(super) mod rebuild;

/// Catalog write-thread commands for activity analytics maintenance and completeness.
pub(super) enum WriteCommand {
    StartRebuild {
        reply: mpsc::SyncSender<Result<ActivityAnalyticsStatus>>,
    },
    StepRebuild {
        max_rows: usize,
        reply: mpsc::SyncSender<Result<bool>>,
    },
    DeleteEvent {
        event_id: String,
        reason: String,
        reply: mpsc::SyncSender<Result<bool>>,
    },
    DeleteEventsBefore {
        camera_id: String,
        cutoff_ms: i64,
        limit: usize,
        reason: String,
        reply: mpsc::SyncSender<Result<u64>>,
    },
    RecordSourceHealth {
        camera_id: String,
        observed_at_ms: i64,
        state: ExpectedSourceState,
        reply: mpsc::SyncSender<Result<()>>,
    },
    SetMaxRollupRows {
        max_rows: u64,
        reply: mpsc::SyncSender<Result<()>>,
    },
    Status {
        site_timezone: String,
        week_start: WeekStart,
        reply: mpsc::SyncSender<Result<ActivityAnalyticsStatus>>,
    },
}

/// Catalog search-thread read commands for activity analytics queries.
pub(super) enum ReadCommand {
    Summary {
        query: Box<ActivitySummaryQuery>,
        reply: mpsc::SyncSender<Result<ActivitySummaryResult>>,
    },
    Status {
        site_timezone: String,
        week_start: WeekStart,
        reply: mpsc::SyncSender<Result<ActivityAnalyticsStatus>>,
    },
}

#[derive(Debug, Clone)]
pub(super) struct StateRow {
    pub schema_version: u32,
    pub state: RollupState,
    pub active_generation: u64,
    pub analytics_generation: u64,
    pub rollup_row_count: u64,
    pub max_rollup_rows: u64,
    pub rebuild_cursor: Option<String>,
    pub last_rebuild_completed_at_ms: Option<i64>,
    pub status_detail: Option<String>,
}

pub(super) const fn ledger_table(generation: u64) -> &'static str {
    if generation.is_multiple_of(2) {
        "analytics_ledger_a"
    } else {
        "analytics_ledger_b"
    }
}

pub(super) const fn hourly_table(generation: u64) -> &'static str {
    if generation.is_multiple_of(2) {
        "analytics_hourly_rollups_a"
    } else {
        "analytics_hourly_rollups_b"
    }
}

pub(super) const fn daily_table(generation: u64) -> &'static str {
    if generation.is_multiple_of(2) {
        "analytics_daily_rollups_a"
    } else {
        "analytics_daily_rollups_b"
    }
}

/// Creates versioned derived analytics tables and integrity triggers.
pub(super) async fn initialize(connection: &turso::Connection) -> Result<()> {
    connection
        .execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS analytics_state (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 schema_version INTEGER NOT NULL,
                 state TEXT NOT NULL,
                 active_generation INTEGER NOT NULL DEFAULT 0,
                 analytics_generation INTEGER NOT NULL DEFAULT 0,
                 rollup_row_count INTEGER NOT NULL DEFAULT 0,
                 max_rollup_rows INTEGER NOT NULL DEFAULT {DEFAULT_MAX_ROLLUP_ROWS},
                 rebuild_cursor TEXT,
                 rebuild_started_generation INTEGER,
                 last_rebuild_completed_at_ms INTEGER,
                 status_detail TEXT
             );
             CREATE TABLE IF NOT EXISTS analytics_rebuild_dirty (
                 event_id TEXT PRIMARY KEY,
                 deleted INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS analytics_event_deletions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 camera_id TEXT NOT NULL,
                 start_time_ms INTEGER NOT NULL,
                 end_time_ms INTEGER NOT NULL,
                 deleted_count INTEGER NOT NULL CHECK (deleted_count > 0),
                 deleted_at_ms INTEGER NOT NULL,
                 reason TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS analytics_event_deletions_camera_time
                 ON analytics_event_deletions(camera_id, start_time_ms, end_time_ms);
             CREATE TABLE IF NOT EXISTS analytics_source_health_intervals (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 camera_id TEXT NOT NULL,
                 start_time_ms INTEGER NOT NULL,
                 end_time_ms INTEGER,
                 state TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS analytics_source_health_camera_time
                 ON analytics_source_health_intervals(camera_id, start_time_ms, end_time_ms);"
        ))
        .await?;
    create_generation_tables(connection, "a").await?;
    create_generation_tables(connection, "b").await?;
    initialize_state_row(connection).await?;
    create_external_delete_trigger(connection).await?;
    Ok(())
}

async fn create_generation_tables(connection: &turso::Connection, suffix: &str) -> Result<()> {
    connection
        .execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS analytics_ledger_{suffix} (
                 event_id TEXT PRIMARY KEY,
                 revision INTEGER NOT NULL,
                 hour_utc_ms INTEGER NOT NULL,
                 day_utc_ms INTEGER NOT NULL,
                 camera_id TEXT NOT NULL,
                 origin TEXT NOT NULL,
                 event_type TEXT NOT NULL,
                 zone_key TEXT NOT NULL,
                 confidence_band INTEGER NOT NULL,
                 has_image INTEGER NOT NULL,
                 start_time_ms INTEGER NOT NULL,
                 end_time_ms INTEGER,
                 is_interval INTEGER NOT NULL,
                 is_open_interval INTEGER NOT NULL,
                 duration_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS analytics_ledger_{suffix}_camera_time
                 ON analytics_ledger_{suffix}(camera_id, start_time_ms);
             CREATE TABLE IF NOT EXISTS analytics_hourly_rollups_{suffix} (
                 hour_utc_ms INTEGER NOT NULL,
                 camera_id TEXT NOT NULL,
                 origin TEXT NOT NULL,
                 event_type TEXT NOT NULL,
                 zone_key TEXT NOT NULL,
                 confidence_band INTEGER NOT NULL,
                 has_image INTEGER NOT NULL,
                 event_count INTEGER NOT NULL CHECK (event_count >= 0),
                 interval_event_count INTEGER NOT NULL CHECK (interval_event_count >= 0),
                 open_interval_event_count INTEGER NOT NULL CHECK (open_interval_event_count >= 0),
                 total_duration_ms INTEGER NOT NULL CHECK (total_duration_ms >= 0),
                 PRIMARY KEY (hour_utc_ms, camera_id, origin, event_type, zone_key, confidence_band, has_image)
             );
             CREATE INDEX IF NOT EXISTS analytics_hourly_rollups_{suffix}_camera_hour
                 ON analytics_hourly_rollups_{suffix}(camera_id, hour_utc_ms);
             CREATE TABLE IF NOT EXISTS analytics_daily_rollups_{suffix} (
                 day_utc_ms INTEGER NOT NULL,
                 camera_id TEXT NOT NULL,
                 origin TEXT NOT NULL,
                 event_type TEXT NOT NULL,
                 zone_key TEXT NOT NULL,
                 confidence_band INTEGER NOT NULL,
                 has_image INTEGER NOT NULL,
                 event_count INTEGER NOT NULL CHECK (event_count >= 0),
                 interval_event_count INTEGER NOT NULL CHECK (interval_event_count >= 0),
                 open_interval_event_count INTEGER NOT NULL CHECK (open_interval_event_count >= 0),
                 total_duration_ms INTEGER NOT NULL CHECK (total_duration_ms >= 0),
                 PRIMARY KEY (day_utc_ms, camera_id, origin, event_type, zone_key, confidence_band, has_image)
             );
             CREATE INDEX IF NOT EXISTS analytics_daily_rollups_{suffix}_camera_day
                 ON analytics_daily_rollups_{suffix}(camera_id, day_utc_ms);"
        ))
        .await?;
    Ok(())
}

async fn initialize_state_row(connection: &turso::Connection) -> Result<()> {
    let mut existing = connection
        .query(
            "SELECT schema_version FROM analytics_state WHERE id = 1",
            (),
        )
        .await?;
    if let Some(row) = existing.next().await? {
        let stored_version = row.get::<i64>(0)?;
        drop(existing);
        if stored_version != i64::from(ANALYTICS_SCHEMA_VERSION) {
            connection
                .execute(
                    "UPDATE analytics_state
                     SET state = 'degraded',
                         status_detail = 'analytics schema version changed; rebuild required'
                     WHERE id = 1",
                    (),
                )
                .await?;
        }
        return Ok(());
    }
    drop(existing);
    let mut has_events = connection
        .query("SELECT 1 FROM recording_events LIMIT 1", ())
        .await?;
    let initial_state = if has_events.next().await?.is_some() {
        (
            "degraded",
            Some("existing catalog events require initial rollup rebuild"),
        )
    } else {
        ("ready", None)
    };
    drop(has_events);
    connection
        .execute(
            "INSERT INTO analytics_state (
                 id, schema_version, state, active_generation, analytics_generation,
                 rollup_row_count, max_rollup_rows, status_detail
             ) VALUES (1, ?1, ?2, 0, 0, 0, ?3, ?4)",
            turso::params![
                i64::from(ANALYTICS_SCHEMA_VERSION),
                initial_state.0,
                to_i64(DEFAULT_MAX_ROLLUP_ROWS, "max rollup rows")?,
                initial_state.1
            ],
        )
        .await?;
    Ok(())
}

async fn create_external_delete_trigger(connection: &turso::Connection) -> Result<()> {
    // Detects direct SQL deletions against the active ledger without interfering with shadow rebuilds.
    connection
        .execute_batch(
            "DROP TRIGGER IF EXISTS analytics_external_event_delete;
             CREATE TRIGGER analytics_external_event_delete
             AFTER DELETE ON recording_events
             WHEN (
                 ((SELECT active_generation % 2 FROM analytics_state WHERE id = 1) = 0
                  AND EXISTS (SELECT 1 FROM analytics_ledger_a WHERE event_id = OLD.id))
                 OR
                 ((SELECT active_generation % 2 FROM analytics_state WHERE id = 1) = 1
                  AND EXISTS (SELECT 1 FROM analytics_ledger_b WHERE event_id = OLD.id))
             )
             BEGIN
                 UPDATE analytics_state
                 SET state = 'degraded',
                     analytics_generation = analytics_generation + 1,
                     status_detail = 'canonical event deleted outside rollup helper; rebuild required'
                 WHERE id = 1;
             END;",
        )
        .await?;
    Ok(())
}

pub(super) async fn load_state(connection: &turso::Connection) -> Result<StateRow> {
    let mut rows = connection
        .query(
            "SELECT schema_version, state, active_generation, analytics_generation,
                    rollup_row_count, max_rollup_rows, rebuild_cursor,
                    last_rebuild_completed_at_ms, status_detail
             FROM analytics_state WHERE id = 1",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .context("analytics_state singleton row is missing")?;
    let schema_version =
        u32::try_from(row.get::<i64>(0)?).context("analytics schema version is out of range")?;
    let state_str = row.get::<String>(1)?;
    let state = RollupState::parse(&state_str).unwrap_or(RollupState::Unavailable);
    Ok(StateRow {
        schema_version,
        state,
        active_generation: to_u64(row.get(2)?, "active analytics generation")?,
        analytics_generation: to_u64(row.get(3)?, "analytics generation")?,
        rollup_row_count: to_u64(row.get(4)?, "rollup row count")?,
        max_rollup_rows: to_u64(row.get(5)?, "max rollup rows")?,
        rebuild_cursor: row.get(6)?,
        last_rebuild_completed_at_ms: row.get(7)?,
        status_detail: row.get(8)?,
    })
}

/// Advances the monotonic analytics generation inside an existing write transaction.
pub(super) async fn bump_generation(connection: &turso::Connection) -> Result<()> {
    connection
        .execute(
            "UPDATE analytics_state
             SET analytics_generation = analytics_generation + 1
             WHERE id = 1",
            (),
        )
        .await?;
    Ok(())
}

pub(super) async fn read_status(
    connection: &turso::Connection,
    site_timezone: &str,
    week_start: WeekStart,
) -> Result<ActivityAnalyticsStatus> {
    let state = load_state(connection).await?;
    let mut oldest_rows = connection
        .query("SELECT MIN(start_time_ms) FROM recording_events", ())
        .await?;
    let oldest_retained_event_ms = oldest_rows
        .next()
        .await?
        .map(|row| row.get::<Option<i64>>(0))
        .transpose()?
        .flatten();
    Ok(ActivityAnalyticsStatus {
        state: state.state,
        analytics_generation: state.analytics_generation,
        schema_version: state.schema_version,
        site_timezone: site_timezone.to_owned(),
        week_start,
        tzdb_version: time::tzdb_version().to_owned(),
        rollup_row_count: state.rollup_row_count,
        max_rollup_rows: state.max_rollup_rows,
        oldest_retained_event_ms,
        last_rebuild_completed_at_ms: state.last_rebuild_completed_at_ms,
        status_detail: state.status_detail,
    })
}

pub(super) async fn execute_write(connection: &turso::Connection, command: WriteCommand) {
    match command {
        WriteCommand::StartRebuild { reply } => {
            let _ = reply.send(rebuild::start(connection).await);
        }
        WriteCommand::StepRebuild { max_rows, reply } => {
            let _ = reply.send(rebuild::step(connection, max_rows).await);
        }
        WriteCommand::DeleteEvent {
            event_id,
            reason,
            reply,
        } => {
            let _ = reply.send(delete_event_tx(connection, &event_id, &reason).await);
        }
        WriteCommand::DeleteEventsBefore {
            camera_id,
            cutoff_ms,
            limit,
            reason,
            reply,
        } => {
            let _ = reply.send(
                delete_events_before_tx(connection, &camera_id, cutoff_ms, limit, &reason).await,
            );
        }
        WriteCommand::RecordSourceHealth {
            camera_id,
            observed_at_ms,
            state,
            reply,
        } => {
            let _ = reply
                .send(record_source_health_tx(connection, &camera_id, observed_at_ms, state).await);
        }
        WriteCommand::SetMaxRollupRows { max_rows, reply } => {
            let _ = reply.send(set_max_rollup_rows_tx(connection, max_rows).await);
        }
        WriteCommand::Status {
            site_timezone,
            week_start,
            reply,
        } => {
            let _ = reply.send(read_status(connection, &site_timezone, week_start).await);
        }
    }
}

pub(super) async fn execute_read(connection: &turso::Connection, command: ReadCommand) {
    match command {
        ReadCommand::Summary { query, reply } => {
            let _ = reply.send(query::execute(connection, *query).await);
        }
        ReadCommand::Status {
            site_timezone,
            week_start,
            reply,
        } => {
            let _ = reply.send(read_status(connection, &site_timezone, week_start).await);
        }
    }
}

async fn delete_event_tx(
    connection: &turso::Connection,
    event_id: &str,
    reason: &str,
) -> Result<bool> {
    ensure!(!event_id.is_empty(), "event ID must not be empty");
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let deleted = delta::delete_canonical_event(connection, event_id, reason).await?;
        Ok::<_, anyhow::Error>(deleted)
    }
    .await;
    match result {
        Ok(deleted) => {
            connection.execute_batch("COMMIT").await?;
            Ok(deleted)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn delete_events_before_tx(
    connection: &turso::Connection,
    camera_id: &str,
    cutoff_ms: i64,
    limit: usize,
    reason: &str,
) -> Result<u64> {
    ensure!(!camera_id.is_empty(), "camera ID must not be empty");
    ensure!(
        (1..=MAX_REBUILD_BATCH_ROWS).contains(&limit),
        "event deletion batch must be between 1 and {MAX_REBUILD_BATCH_ROWS}"
    );
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let mut rows = connection
            .query(
                "SELECT id FROM recording_events
                 WHERE camera_id = ?1 AND start_time_ms < ?2
                 ORDER BY start_time_ms, id LIMIT ?3",
                turso::params![camera_id, cutoff_ms, i64::try_from(limit)?],
            )
            .await?;
        let mut ids = Vec::with_capacity(limit);
        while let Some(row) = rows.next().await? {
            ids.push(row.get::<String>(0)?);
        }
        drop(rows);
        let mut deleted_total = 0u64;
        for id in ids {
            if delta::delete_canonical_event(connection, &id, reason).await? {
                deleted_total += 1;
            }
        }
        Ok::<_, anyhow::Error>(deleted_total)
    }
    .await;
    match result {
        Ok(count) => {
            connection.execute_batch("COMMIT").await?;
            Ok(count)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn record_source_health_tx(
    connection: &turso::Connection,
    camera_id: &str,
    observed_at_ms: i64,
    state: ExpectedSourceState,
) -> Result<()> {
    ensure!(!camera_id.is_empty(), "camera ID must not be empty");
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let mut open = connection
            .query(
                "SELECT id, start_time_ms, state FROM analytics_source_health_intervals
                 WHERE camera_id = ?1 AND end_time_ms IS NULL
                 ORDER BY id DESC LIMIT 1",
                turso::params![camera_id],
            )
            .await?;
        let current = open
            .next()
            .await?
            .map(|row| -> Result<(i64, i64, String)> {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .transpose()?;
        drop(open);
        if let Some((row_id, start_ms, current_state)) = current {
            if current_state == state.as_str() {
                return Ok::<_, anyhow::Error>(());
            }
            let close_ms = observed_at_ms.max(start_ms);
            connection
                .execute(
                    "UPDATE analytics_source_health_intervals SET end_time_ms = ?2 WHERE id = ?1",
                    turso::params![row_id, close_ms],
                )
                .await?;
        }
        connection
            .execute(
                "INSERT INTO analytics_source_health_intervals (camera_id, start_time_ms, end_time_ms, state)
                 VALUES (?1, ?2, NULL, ?3)",
                turso::params![camera_id, observed_at_ms, state.as_str()],
            )
            .await?;
        prune_bounded_rows(
            connection,
            "analytics_source_health_intervals",
            MAX_SOURCE_HEALTH_ROWS,
        )
        .await?;
        bump_generation(connection).await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn set_max_rollup_rows_tx(connection: &turso::Connection, max_rows: u64) -> Result<()> {
    ensure!(max_rows > 0, "max_rollup_rows must be greater than zero");
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = connection
        .execute(
            "UPDATE analytics_state SET max_rollup_rows = ?1 WHERE id = 1",
            turso::params![to_i64(max_rows, "max rollup rows")?],
        )
        .await;
    match result {
        Ok(_) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error.into())
        }
    }
}

pub(super) async fn record_deletion_evidence(
    connection: &turso::Connection,
    camera_id: &str,
    start_time_ms: i64,
    end_time_ms: i64,
    deleted_count: u64,
    reason: &str,
) -> Result<()> {
    if deleted_count == 0 {
        return Ok(());
    }
    let normalized_end = end_time_ms.max(start_time_ms.saturating_add(1));
    connection
        .execute(
            "INSERT INTO analytics_event_deletions (
                 camera_id, start_time_ms, end_time_ms, deleted_count, deleted_at_ms, reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            turso::params![
                camera_id,
                start_time_ms,
                normalized_end,
                to_i64(deleted_count, "deleted event count")?,
                current_unix_time_ms(),
                reason
            ],
        )
        .await?;
    prune_bounded_rows(
        connection,
        "analytics_event_deletions",
        MAX_DELETION_EVIDENCE_ROWS,
    )
    .await
}

async fn prune_bounded_rows(
    connection: &turso::Connection,
    table: &str,
    max_rows: i64,
) -> Result<()> {
    let mut rows = connection
        .query(format!("SELECT COUNT(*) FROM {table}"), ())
        .await?;
    let count = rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?
        .unwrap_or(0);
    drop(rows);
    if count > max_rows {
        let excess = count - max_rows;
        connection
            .execute(
                format!(
                    "DELETE FROM {table} WHERE id IN (
                         SELECT id FROM {table} ORDER BY id ASC LIMIT ?1
                     )"
                ),
                turso::params![excess],
            )
            .await?;
    }
    Ok(())
}

impl RecordingCatalogHandle {
    /// Returns the current analytics rollup status and generation.
    pub fn analytics_status(
        &self,
        site_timezone: &str,
        week_start: WeekStart,
    ) -> Result<ActivityAnalyticsStatus> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.search_tx
            .send(super::SearchCommand::Analytics(ReadCommand::Status {
                site_timezone: site_timezone.to_owned(),
                week_start,
                reply,
            }))
            .map_err(|_| anyhow::anyhow!("recording catalog search worker stopped"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("recording catalog search worker dropped response"))?
    }

    /// Executes one bounded, authorized activity summary query.
    pub fn query_activity_summary(
        &self,
        query: ActivitySummaryQuery,
    ) -> Result<ActivitySummaryResult> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.search_tx
            .send(super::SearchCommand::Analytics(ReadCommand::Summary {
                query: Box::new(query),
                reply,
            }))
            .map_err(|_| anyhow::anyhow!("recording catalog search worker stopped"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("recording catalog search worker dropped response"))?
    }

    /// Starts a bounded shadow-table rebuild from canonical events.
    pub fn start_analytics_rebuild(&self) -> Result<ActivityAnalyticsStatus> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .send(super::Command::Analytics(WriteCommand::StartRebuild {
                reply,
            }))
            .map_err(|_| anyhow::anyhow!("recording catalog stopped"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("recording catalog dropped response"))?
    }

    /// Advances an active shadow-table rebuild by at most `max_rows` canonical events.
    pub fn step_analytics_rebuild(&self, max_rows: usize) -> Result<bool> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .send(super::Command::Analytics(WriteCommand::StepRebuild {
                max_rows,
                reply,
            }))
            .map_err(|_| anyhow::anyhow!("recording catalog stopped"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("recording catalog dropped response"))?
    }

    /// Runs a full bounded shadow-table rebuild until swap completes.
    pub fn rebuild_analytics_blocking(
        &self,
        site_timezone: &str,
        week_start: WeekStart,
    ) -> Result<ActivityAnalyticsStatus> {
        self.start_analytics_rebuild()?;
        let mut batches = 0usize;
        const MAX_BLOCKING_BATCHES: usize = 50_000;
        loop {
            ensure!(
                batches < MAX_BLOCKING_BATCHES,
                "analytics rebuild exceeded maximum batch limit"
            );
            batches += 1;
            if self.step_analytics_rebuild(MAX_REBUILD_BATCH_ROWS)? {
                break;
            }
        }
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .send(super::Command::Analytics(WriteCommand::Status {
                site_timezone: site_timezone.to_owned(),
                week_start,
                reply,
            }))
            .map_err(|_| anyhow::anyhow!("recording catalog stopped"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("recording catalog dropped response"))?
    }

    /// Deletes one canonical event and subtracts its rollup contribution transactionally.
    pub fn delete_event(&self, event_id: &str, reason: &str) -> Result<bool> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .send(super::Command::Analytics(WriteCommand::DeleteEvent {
                event_id: event_id.to_owned(),
                reason: reason.to_owned(),
                reply,
            }))
            .map_err(|_| anyhow::anyhow!("recording catalog stopped"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("recording catalog dropped response"))?
    }

    /// Deletes a bounded batch of expired events for one camera and records retention evidence.
    pub fn delete_events_before(
        &self,
        camera_id: &str,
        cutoff_ms: i64,
        limit: usize,
        reason: &str,
    ) -> Result<u64> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .send(super::Command::Analytics(
                WriteCommand::DeleteEventsBefore {
                    camera_id: camera_id.to_owned(),
                    cutoff_ms,
                    limit,
                    reason: reason.to_owned(),
                    reply,
                },
            ))
            .map_err(|_| anyhow::anyhow!("recording catalog stopped"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("recording catalog dropped response"))?
    }

    /// Records an observed expected-source health transition for one camera.
    pub fn record_source_health_transition(
        &self,
        camera_id: &str,
        observed_at_ms: i64,
        state: ExpectedSourceState,
    ) -> Result<()> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .send(super::Command::Analytics(
                WriteCommand::RecordSourceHealth {
                    camera_id: camera_id.to_owned(),
                    observed_at_ms,
                    state,
                    reply,
                },
            ))
            .map_err(|_| anyhow::anyhow!("recording catalog stopped"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("recording catalog dropped response"))?
    }

    /// Updates the derived rollup row quota enforced by incremental writes and rebuilds.
    pub fn set_analytics_max_rollup_rows(&self, max_rows: u64) -> Result<()> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .send(super::Command::Analytics(WriteCommand::SetMaxRollupRows {
                max_rows,
                reply,
            }))
            .map_err(|_| anyhow::anyhow!("recording catalog stopped"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("recording catalog dropped response"))?
    }
}
