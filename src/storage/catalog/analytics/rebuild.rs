//! Bounded, resumable shadow-table rebuild and atomic generation swap.

use super::{
    daily_table,
    delta::{DeltaOutcome, apply_generation_delta, apply_signed_delta, read_ledger, write_ledger},
    hourly_table, ledger_table, load_state, read_status,
};
use crate::storage::{
    analytics::{
        ANALYTICS_SCHEMA_VERSION, ActivityAnalyticsStatus, EventContribution,
        MAX_REBUILD_BATCH_ROWS, RollupState, WeekStart,
    },
    catalog::{
        EVENT_DETAIL_COLUMNS, current_unix_time_ms, event_by_id, event_from_row, to_i64, to_u64,
    },
};
use anyhow::{Result, ensure};

/// Initializes a shadow-table rebuild into the inactive generation without touching canonical events.
pub(super) async fn start(connection: &turso::Connection) -> Result<ActivityAnalyticsStatus> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let state = load_state(connection).await?;
        let shadow_gen = state.active_generation.wrapping_add(1);
        clear_generation_tables(connection, shadow_gen).await?;
        connection
            .execute("DELETE FROM analytics_rebuild_dirty", ())
            .await?;
        connection
            .execute(
                "UPDATE analytics_state
                 SET state = 'rebuilding',
                     rebuild_cursor = '',
                     rebuild_started_generation = analytics_generation,
                     status_detail = 'rebuilding derived analytics rollups in shadow tables'
                 WHERE id = 1",
                (),
            )
            .await?;
        read_status(connection, "UTC", WeekStart::Monday).await
    }
    .await;
    match result {
        Ok(status) => {
            connection.execute_batch("COMMIT").await?;
            Ok(status)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

/// Processes at most `max_rows` canonical events or dirty catch-up IDs in one transaction.
///
/// Returns `true` when the shadow tables are complete and atomically swapped to active.
pub(super) async fn step(connection: &turso::Connection, max_rows: usize) -> Result<bool> {
    ensure!(
        (1..=MAX_REBUILD_BATCH_ROWS).contains(&max_rows),
        "analytics rebuild batch must be between 1 and {MAX_REBUILD_BATCH_ROWS}"
    );
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = step_in_transaction(connection, max_rows).await;
    match result {
        Ok(done) => {
            connection.execute_batch("COMMIT").await?;
            Ok(done)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn step_in_transaction(connection: &turso::Connection, max_rows: usize) -> Result<bool> {
    let state = load_state(connection).await?;
    match state.state {
        RollupState::Rebuilding => {}
        RollupState::Ready => return Ok(true),
        other => anyhow::bail!(
            "analytics rebuild is not active (state: {})",
            other.as_str()
        ),
    }
    let shadow_gen = state.active_generation.wrapping_add(1);
    let Some(cursor) = state.rebuild_cursor.as_deref() else {
        restart_shadow_rebuild(connection, shadow_gen).await?;
        return Ok(false);
    };
    let shadow_rows = count_generation_rows(connection, shadow_gen).await?;
    if cursor != "\u{10ffff}"
        && advance_canonical_scan(
            connection,
            shadow_gen,
            cursor,
            shadow_rows,
            state.max_rollup_rows,
            max_rows,
        )
        .await?
    {
        return Ok(false);
    }
    let refreshed_rows = count_generation_rows(connection, shadow_gen).await?;
    catch_up_and_swap(
        connection,
        shadow_gen,
        state.active_generation,
        refreshed_rows,
        state.max_rollup_rows,
        max_rows,
    )
    .await
}

async fn restart_shadow_rebuild(connection: &turso::Connection, shadow_gen: u64) -> Result<()> {
    clear_generation_tables(connection, shadow_gen).await?;
    connection
        .execute("DELETE FROM analytics_rebuild_dirty", ())
        .await?;
    connection
        .execute(
            "UPDATE analytics_state
             SET rebuild_cursor = '',
                 rebuild_started_generation = analytics_generation,
                 status_detail = 'restarted shadow rebuild after dirty queue limit'
             WHERE id = 1",
            (),
        )
        .await?;
    Ok(())
}

async fn advance_canonical_scan(
    connection: &turso::Connection,
    shadow_gen: u64,
    cursor: &str,
    mut shadow_rows: u64,
    max_rows_quota: u64,
    batch_limit: usize,
) -> Result<bool> {
    let events = fetch_canonical_batch(connection, cursor, batch_limit).await?;
    if events.is_empty() {
        connection
            .execute(
                "UPDATE analytics_state SET rebuild_cursor = ?1 WHERE id = 1",
                turso::params!["\u{10ffff}"],
            )
            .await?;
        return Ok(false);
    }
    let last_id = events
        .last()
        .map(|event| event.id.clone())
        .unwrap_or_default();
    for event in &events {
        let contribution = EventContribution::from_event(event)?;
        let previous = read_ledger(connection, shadow_gen, &event.id).await?;
        match apply_generation_delta(
            connection,
            shadow_gen,
            shadow_rows,
            max_rows_quota,
            previous.as_ref(),
            Some(&contribution),
        )
        .await?
        {
            DeltaOutcome::Applied { row_count_delta } => {
                write_ledger(connection, shadow_gen, &event.id, Some(&contribution)).await?;
                shadow_rows = apply_signed_delta(shadow_rows, row_count_delta)?;
            }
            DeltaOutcome::Degraded(reason) => {
                abort_rebuild_degraded(connection, shadow_gen, reason).await?;
                return Ok(true);
            }
        }
    }
    let next_cursor = if events.len() < batch_limit {
        "\u{10ffff}".to_owned()
    } else {
        last_id
    };
    connection
        .execute(
            "UPDATE analytics_state SET rebuild_cursor = ?1 WHERE id = 1",
            turso::params![next_cursor],
        )
        .await?;
    Ok(true)
}

async fn catch_up_and_swap(
    connection: &turso::Connection,
    shadow_gen: u64,
    old_gen: u64,
    mut shadow_rows: u64,
    max_rows_quota: u64,
    batch_limit: usize,
) -> Result<bool> {
    let dirty_ids = fetch_dirty_batch(connection, batch_limit).await?;
    if !dirty_ids.is_empty() {
        for event_id in &dirty_ids {
            let previous = read_ledger(connection, shadow_gen, event_id).await?;
            let current = event_by_id(connection, event_id)
                .await?
                .map(|event| EventContribution::from_event(&event))
                .transpose()?;
            match apply_generation_delta(
                connection,
                shadow_gen,
                shadow_rows,
                max_rows_quota,
                previous.as_ref(),
                current.as_ref(),
            )
            .await?
            {
                DeltaOutcome::Applied { row_count_delta } => {
                    write_ledger(connection, shadow_gen, event_id, current.as_ref()).await?;
                    shadow_rows = apply_signed_delta(shadow_rows, row_count_delta)?;
                }
                DeltaOutcome::Degraded(reason) => {
                    return abort_rebuild_degraded(connection, shadow_gen, reason).await;
                }
            }
            connection
                .execute(
                    "DELETE FROM analytics_rebuild_dirty WHERE event_id = ?1",
                    turso::params![event_id.as_str()],
                )
                .await?;
        }
        return Ok(false);
    }
    // Atomic O(1) generation swap; clear the retired generation tables in the same transaction.
    connection
        .execute(
            "UPDATE analytics_state
             SET schema_version = ?1,
                 state = 'ready',
                 active_generation = ?2,
                 analytics_generation = analytics_generation + 1,
                 rollup_row_count = ?3,
                 rebuild_cursor = NULL,
                 rebuild_started_generation = NULL,
                 last_rebuild_completed_at_ms = ?4,
                 status_detail = NULL
             WHERE id = 1",
            turso::params![
                i64::from(ANALYTICS_SCHEMA_VERSION),
                to_i64(shadow_gen, "shadow generation")?,
                to_i64(shadow_rows, "shadow rollup row count")?,
                current_unix_time_ms(),
            ],
        )
        .await?;
    clear_generation_tables(connection, old_gen).await?;
    Ok(true)
}

async fn abort_rebuild_degraded(
    connection: &turso::Connection,
    shadow_gen: u64,
    reason: &str,
) -> Result<bool> {
    clear_generation_tables(connection, shadow_gen).await?;
    connection
        .execute("DELETE FROM analytics_rebuild_dirty", ())
        .await?;
    connection
        .execute(
            "UPDATE analytics_state
             SET state = 'degraded',
                 analytics_generation = analytics_generation + 1,
                 rebuild_cursor = NULL,
                 rebuild_started_generation = NULL,
                 status_detail = ?1
             WHERE id = 1",
            turso::params![reason],
        )
        .await?;
    Ok(true)
}

async fn clear_generation_tables(connection: &turso::Connection, generation: u64) -> Result<()> {
    let ledger = ledger_table(generation);
    let hourly = hourly_table(generation);
    let daily = daily_table(generation);
    connection
        .execute_batch(&format!(
            "DELETE FROM {ledger}; DELETE FROM {hourly}; DELETE FROM {daily};"
        ))
        .await?;
    Ok(())
}

async fn count_generation_rows(connection: &turso::Connection, generation: u64) -> Result<u64> {
    let hourly = hourly_table(generation);
    let daily = daily_table(generation);
    let mut rows = connection
        .query(
            format!("SELECT (SELECT COUNT(*) FROM {hourly}) + (SELECT COUNT(*) FROM {daily})"),
            (),
        )
        .await?;
    let count = rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?
        .unwrap_or(0);
    to_u64(count, "shadow rollup row count")
}

async fn fetch_canonical_batch(
    connection: &turso::Connection,
    cursor: &str,
    limit: usize,
) -> Result<Vec<crate::storage::metadata::TimelineEvent>> {
    let sql = if cursor.is_empty() {
        format!("SELECT {EVENT_DETAIL_COLUMNS} FROM recording_events ORDER BY id ASC LIMIT ?1")
    } else {
        format!(
            "SELECT {EVENT_DETAIL_COLUMNS} FROM recording_events WHERE id > ?1 ORDER BY id ASC LIMIT ?2"
        )
    };
    let mut rows = if cursor.is_empty() {
        connection
            .query(sql, turso::params![i64::try_from(limit)?])
            .await?
    } else {
        connection
            .query(sql, turso::params![cursor, i64::try_from(limit)?])
            .await?
    };
    let mut events = Vec::with_capacity(limit);
    while let Some(row) = rows.next().await? {
        events.push(event_from_row(&row, true)?);
    }
    Ok(events)
}

async fn fetch_dirty_batch(connection: &turso::Connection, limit: usize) -> Result<Vec<String>> {
    let mut rows = connection
        .query(
            "SELECT event_id FROM analytics_rebuild_dirty ORDER BY event_id ASC LIMIT ?1",
            turso::params![i64::try_from(limit)?],
        )
        .await?;
    let mut ids = Vec::with_capacity(limit);
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    Ok(ids)
}
