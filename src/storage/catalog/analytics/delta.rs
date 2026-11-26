//! Atomic incremental rollup delta updates and failure isolation.

use super::{
    StateRow, bump_generation, daily_table, hourly_table, ledger_table, load_state,
    record_deletion_evidence,
};
use crate::storage::{
    analytics::{ConfidenceBand, EventContribution, MAX_REBUILD_DIRTY_ROWS, RollupState},
    catalog::{event_by_id, to_i64, to_u64},
    metadata::{EventSource, TimelineEvent},
};
use anyhow::Result;

#[derive(Debug, Clone, Copy)]
struct RollupCounts {
    event_count: u64,
    interval_event_count: u64,
    open_interval_event_count: u64,
    total_duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RollupKey<'a> {
    bucket_ms: i64,
    camera_id: &'a str,
    origin: &'a str,
    event_type: &'a str,
    zone_key: &'a str,
    confidence_band: i64,
    has_image: i64,
}

pub(super) enum DeltaOutcome {
    Applied { row_count_delta: i64 },
    Degraded(&'static str),
}

/// Applies the incremental rollup delta for one upserted or updated canonical event.
///
/// Rollup quota or integrity failures transition analytics to `degraded` without
/// failing the canonical event write transaction.
pub(in crate::storage::catalog) async fn sync_event_upsert(
    connection: &turso::Connection,
    event: &TimelineEvent,
) -> Result<()> {
    let state = load_state(connection).await?;
    if state.state == RollupState::Rebuilding {
        record_dirty_event(connection, &event.id, false).await?;
    }
    if matches!(
        state.state,
        RollupState::Degraded | RollupState::Unavailable
    ) {
        bump_generation(connection).await?;
        return Ok(());
    }
    let contribution = EventContribution::from_event(event)?;
    apply_active_delta(connection, &state, &event.id, Some(&contribution)).await
}

/// Re-reads one canonical event by ID inside an active write transaction and syncs rollups.
pub(in crate::storage::catalog) async fn sync_event_by_id(
    connection: &turso::Connection,
    event_id: &str,
) -> Result<()> {
    let Some(event) = event_by_id(connection, event_id).await? else {
        return Ok(());
    };
    sync_event_upsert(connection, &event).await
}

/// Deletes one canonical event and updates active rollups and retention deletion evidence.
pub(super) async fn delete_canonical_event(
    connection: &turso::Connection,
    event_id: &str,
    reason: &str,
) -> Result<bool> {
    let Some(event) = event_by_id(connection, event_id).await? else {
        return Ok(false);
    };
    let state = load_state(connection).await?;
    if state.state == RollupState::Rebuilding {
        record_dirty_event(connection, event_id, true).await?;
    }
    if matches!(state.state, RollupState::Ready | RollupState::Rebuilding) {
        apply_active_delta(connection, &state, event_id, None).await?;
    } else {
        let active_ledger = ledger_table(state.active_generation);
        connection
            .execute(
                format!("DELETE FROM {active_ledger} WHERE event_id = ?1"),
                turso::params![event_id],
            )
            .await?;
        bump_generation(connection).await?;
    }
    let end_ms = event
        .end_time_ms
        .unwrap_or_else(|| event.start_time_ms.saturating_add(1));
    record_deletion_evidence(
        connection,
        &event.camera_id,
        event.start_time_ms,
        end_ms,
        1,
        reason,
    )
    .await?;
    connection
        .execute(
            "DELETE FROM recording_events WHERE id = ?1",
            turso::params![event_id],
        )
        .await?;
    Ok(true)
}

async fn apply_active_delta(
    connection: &turso::Connection,
    state: &StateRow,
    event_id: &str,
    next: Option<&EventContribution>,
) -> Result<()> {
    let previous = read_ledger(connection, state.active_generation, event_id).await?;
    if let (Some(prev), Some(curr)) = (&previous, next)
        && curr.revision <= prev.revision
    {
        return Ok(());
    }
    match apply_generation_delta(
        connection,
        state.active_generation,
        state.rollup_row_count,
        state.max_rollup_rows,
        previous.as_ref(),
        next,
    )
    .await?
    {
        DeltaOutcome::Applied { row_count_delta } => {
            write_ledger(connection, state.active_generation, event_id, next).await?;
            let updated_rows = apply_signed_delta(state.rollup_row_count, row_count_delta)?;
            connection
                .execute(
                    "UPDATE analytics_state
                     SET rollup_row_count = ?1,
                         analytics_generation = analytics_generation + 1
                     WHERE id = 1",
                    turso::params![to_i64(updated_rows, "rollup row count")?],
                )
                .await?;
        }
        DeltaOutcome::Degraded(reason) => {
            if next.is_none() {
                write_ledger(connection, state.active_generation, event_id, None).await?;
            }
            // Preserve an active shadow rebuild; the dirty queue replays this event into the new generation.
            if state.state == RollupState::Rebuilding {
                bump_generation(connection).await?;
            } else {
                mark_degraded(connection, reason).await?;
            }
        }
    }
    Ok(())
}

/// Applies a validated old-to-new contribution delta onto one rollup generation.
pub(super) async fn apply_generation_delta(
    connection: &turso::Connection,
    generation: u64,
    current_rows: u64,
    max_rows: u64,
    previous: Option<&EventContribution>,
    next: Option<&EventContribution>,
) -> Result<DeltaOutcome> {
    if previous.is_none() && next.is_none() {
        return Ok(DeltaOutcome::Applied { row_count_delta: 0 });
    }
    let hourly = hourly_table(generation);
    let daily = daily_table(generation);
    if let Some(reason) = verify_delta_preconditions(
        connection,
        hourly,
        daily,
        current_rows,
        max_rows,
        previous,
        next,
    )
    .await?
    {
        return Ok(DeltaOutcome::Degraded(reason));
    }
    let mut actual_delta = 0i64;
    if let Some(prev) = previous {
        actual_delta +=
            subtract_row(connection, hourly, "hour_utc_ms", &hourly_key(prev), prev).await?;
        actual_delta +=
            subtract_row(connection, daily, "day_utc_ms", &daily_key(prev), prev).await?;
    }
    if let Some(curr) = next {
        actual_delta += add_row(connection, hourly, "hour_utc_ms", &hourly_key(curr), curr).await?;
        actual_delta += add_row(connection, daily, "day_utc_ms", &daily_key(curr), curr).await?;
    }
    Ok(DeltaOutcome::Applied {
        row_count_delta: actual_delta,
    })
}

async fn verify_delta_preconditions(
    connection: &turso::Connection,
    hourly: &str,
    daily: &str,
    current_rows: u64,
    max_rows: u64,
    previous: Option<&EventContribution>,
    next: Option<&EventContribution>,
) -> Result<Option<&'static str>> {
    let mut net_new_rows = 0i64;
    let mut prev_hourly_count = 0u64;
    let mut prev_daily_count = 0u64;
    if let Some(prev) = previous {
        let Some(hc) =
            read_rollup_row(connection, hourly, "hour_utc_ms", &hourly_key(prev)).await?
        else {
            return Ok(Some("rollup integrity mismatch: missing hourly row"));
        };
        let Some(dc) = read_rollup_row(connection, daily, "day_utc_ms", &daily_key(prev)).await?
        else {
            return Ok(Some("rollup integrity mismatch: missing daily row"));
        };
        if checked_sub_counts(hc, prev).is_none() || checked_sub_counts(dc, prev).is_none() {
            return Ok(Some("rollup integrity mismatch: counter underflow"));
        }
        prev_hourly_count = hc.event_count;
        prev_daily_count = dc.event_count;
        net_new_rows -= i64::from(prev_hourly_count == 1) + i64::from(prev_daily_count == 1);
    }
    if let Some(curr) = next {
        let same_hourly = previous.is_some_and(|prev| hourly_key(prev) == hourly_key(curr));
        let same_daily = previous.is_some_and(|prev| daily_key(prev) == daily_key(curr));
        let hourly_exists = (same_hourly && prev_hourly_count > 1)
            || (!same_hourly
                && read_rollup_row(connection, hourly, "hour_utc_ms", &hourly_key(curr))
                    .await?
                    .is_some());
        let daily_exists = (same_daily && prev_daily_count > 1)
            || (!same_daily
                && read_rollup_row(connection, daily, "day_utc_ms", &daily_key(curr))
                    .await?
                    .is_some());
        net_new_rows += i64::from(!hourly_exists) + i64::from(!daily_exists);
    }
    if apply_signed_delta(current_rows, net_new_rows)? > max_rows {
        return Ok(Some(
            "rollup row quota exceeded; rebuild or raise max_rollup_rows",
        ));
    }
    Ok(None)
}

pub(super) async fn read_ledger(
    connection: &turso::Connection,
    generation: u64,
    event_id: &str,
) -> Result<Option<EventContribution>> {
    let table = ledger_table(generation);
    let mut rows = connection
        .query(
            format!(
                "SELECT event_id, revision, hour_utc_ms, day_utc_ms, camera_id, origin,
                        event_type, zone_key, confidence_band, has_image, start_time_ms,
                        end_time_ms, is_interval, is_open_interval, duration_ms
                 FROM {table} WHERE event_id = ?1"
            ),
            turso::params![event_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let origin_str: String = row.get(5)?;
    let origin = match origin_str.as_str() {
        "camera" => EventSource::Camera,
        "keeppeek" => EventSource::KeepPeek,
        other => anyhow::bail!("invalid analytics ledger event origin '{other}'"),
    };
    let confidence_band = ConfidenceBand::from_i64(row.get(8)?)
        .ok_or_else(|| anyhow::anyhow!("invalid analytics ledger confidence band"))?;
    Ok(Some(EventContribution {
        event_id: row.get(0)?,
        revision: to_u64(row.get(1)?, "ledger revision")?,
        hour_utc_ms: row.get(2)?,
        day_utc_ms: row.get(3)?,
        camera_id: row.get(4)?,
        origin,
        event_type: row.get(6)?,
        zone_key: row.get(7)?,
        confidence_band,
        has_image: row.get::<i64>(9)? != 0,
        start_time_ms: row.get(10)?,
        end_time_ms: row.get(11)?,
        is_interval: row.get::<i64>(12)? != 0,
        is_open_interval: row.get::<i64>(13)? != 0,
        duration_ms: to_u64(row.get(14)?, "ledger duration")?,
    }))
}

pub(super) async fn write_ledger(
    connection: &turso::Connection,
    generation: u64,
    event_id: &str,
    contribution: Option<&EventContribution>,
) -> Result<()> {
    let table = ledger_table(generation);
    let Some(c) = contribution else {
        connection
            .execute(
                format!("DELETE FROM {table} WHERE event_id = ?1"),
                turso::params![event_id],
            )
            .await?;
        return Ok(());
    };
    connection
        .execute(
            format!(
                "INSERT INTO {table} (
                     event_id, revision, hour_utc_ms, day_utc_ms, camera_id, origin,
                     event_type, zone_key, confidence_band, has_image, start_time_ms,
                     end_time_ms, is_interval, is_open_interval, duration_ms
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                 ON CONFLICT(event_id) DO UPDATE SET
                     revision=excluded.revision, hour_utc_ms=excluded.hour_utc_ms,
                     day_utc_ms=excluded.day_utc_ms, camera_id=excluded.camera_id,
                     origin=excluded.origin, event_type=excluded.event_type,
                     zone_key=excluded.zone_key, confidence_band=excluded.confidence_band,
                     has_image=excluded.has_image, start_time_ms=excluded.start_time_ms,
                     end_time_ms=excluded.end_time_ms, is_interval=excluded.is_interval,
                     is_open_interval=excluded.is_open_interval, duration_ms=excluded.duration_ms"
            ),
            turso::params![
                c.event_id.as_str(),
                to_i64(c.revision, "ledger revision")?,
                c.hour_utc_ms,
                c.day_utc_ms,
                c.camera_id.as_str(),
                c.origin.as_str(),
                c.event_type.as_str(),
                c.zone_key.as_str(),
                c.confidence_band.as_i64(),
                i64::from(c.has_image),
                c.start_time_ms,
                c.end_time_ms,
                i64::from(c.is_interval),
                i64::from(c.is_open_interval),
                to_i64(c.duration_ms, "ledger duration")?,
            ],
        )
        .await?;
    Ok(())
}

async fn read_rollup_row(
    connection: &turso::Connection,
    table: &str,
    bucket_col: &str,
    key: &RollupKey<'_>,
) -> Result<Option<RollupCounts>> {
    let mut rows = connection
        .query(
            format!(
                "SELECT event_count, interval_event_count, open_interval_event_count, total_duration_ms
                 FROM {table}
                 WHERE {bucket_col} = ?1 AND camera_id = ?2 AND origin = ?3 AND event_type = ?4
                   AND zone_key = ?5 AND confidence_band = ?6 AND has_image = ?7"
            ),
            turso::params![
                key.bucket_ms,
                key.camera_id,
                key.origin,
                key.event_type,
                key.zone_key,
                key.confidence_band,
                key.has_image,
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(RollupCounts {
        event_count: to_u64(row.get(0)?, "rollup event_count")?,
        interval_event_count: to_u64(row.get(1)?, "rollup interval_event_count")?,
        open_interval_event_count: to_u64(row.get(2)?, "rollup open_interval_event_count")?,
        total_duration_ms: to_u64(row.get(3)?, "rollup total_duration_ms")?,
    }))
}

async fn subtract_row(
    connection: &turso::Connection,
    table: &str,
    bucket_col: &str,
    key: &RollupKey<'_>,
    prev: &EventContribution,
) -> Result<i64> {
    let existing = read_rollup_row(connection, table, bucket_col, key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("verified rollup row disappeared"))?;
    let updated = checked_sub_counts(existing, prev)
        .ok_or_else(|| anyhow::anyhow!("verified rollup counter underflowed"))?;
    if updated.event_count == 0 {
        connection
            .execute(
                format!(
                    "DELETE FROM {table}
                     WHERE {bucket_col} = ?1 AND camera_id = ?2 AND origin = ?3 AND event_type = ?4
                       AND zone_key = ?5 AND confidence_band = ?6 AND has_image = ?7"
                ),
                turso::params![
                    key.bucket_ms,
                    key.camera_id,
                    key.origin,
                    key.event_type,
                    key.zone_key,
                    key.confidence_band,
                    key.has_image,
                ],
            )
            .await?;
        Ok(-1)
    } else {
        connection
            .execute(
                format!(
                    "UPDATE {table}
                     SET event_count = ?8, interval_event_count = ?9,
                         open_interval_event_count = ?10, total_duration_ms = ?11
                     WHERE {bucket_col} = ?1 AND camera_id = ?2 AND origin = ?3 AND event_type = ?4
                       AND zone_key = ?5 AND confidence_band = ?6 AND has_image = ?7"
                ),
                turso::params![
                    key.bucket_ms,
                    key.camera_id,
                    key.origin,
                    key.event_type,
                    key.zone_key,
                    key.confidence_band,
                    key.has_image,
                    to_i64(updated.event_count, "event_count")?,
                    to_i64(updated.interval_event_count, "interval_event_count")?,
                    to_i64(
                        updated.open_interval_event_count,
                        "open_interval_event_count"
                    )?,
                    to_i64(updated.total_duration_ms, "total_duration_ms")?,
                ],
            )
            .await?;
        Ok(0)
    }
}

async fn add_row(
    connection: &turso::Connection,
    table: &str,
    bucket_col: &str,
    key: &RollupKey<'_>,
    curr: &EventContribution,
) -> Result<i64> {
    let existing = read_rollup_row(connection, table, bucket_col, key).await?;
    let is_new = existing.is_none();
    let base = existing.unwrap_or(RollupCounts {
        event_count: 0,
        interval_event_count: 0,
        open_interval_event_count: 0,
        total_duration_ms: 0,
    });
    let updated = checked_add_counts(base, curr)?;
    connection
        .execute(
            format!(
                "INSERT INTO {table} (
                     {bucket_col}, camera_id, origin, event_type, zone_key,
                     confidence_band, has_image, event_count, interval_event_count,
                     open_interval_event_count, total_duration_ms
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                 ON CONFLICT({bucket_col}, camera_id, origin, event_type, zone_key, confidence_band, has_image)
                 DO UPDATE SET
                     event_count = excluded.event_count,
                     interval_event_count = excluded.interval_event_count,
                     open_interval_event_count = excluded.open_interval_event_count,
                     total_duration_ms = excluded.total_duration_ms"
            ),
            turso::params![
                key.bucket_ms,
                key.camera_id,
                key.origin,
                key.event_type,
                key.zone_key,
                key.confidence_band,
                key.has_image,
                to_i64(updated.event_count, "event_count")?,
                to_i64(updated.interval_event_count, "interval_event_count")?,
                to_i64(updated.open_interval_event_count, "open_interval_event_count")?,
                to_i64(updated.total_duration_ms, "total_duration_ms")?,
            ],
        )
        .await?;
    Ok(i64::from(is_new))
}

fn hourly_key(contribution: &EventContribution) -> RollupKey<'_> {
    RollupKey {
        bucket_ms: contribution.hour_utc_ms,
        camera_id: &contribution.camera_id,
        origin: contribution.origin.as_str(),
        event_type: &contribution.event_type,
        zone_key: &contribution.zone_key,
        confidence_band: contribution.confidence_band.as_i64(),
        has_image: i64::from(contribution.has_image),
    }
}

fn daily_key(contribution: &EventContribution) -> RollupKey<'_> {
    RollupKey {
        bucket_ms: contribution.day_utc_ms,
        camera_id: &contribution.camera_id,
        origin: contribution.origin.as_str(),
        event_type: &contribution.event_type,
        zone_key: &contribution.zone_key,
        confidence_band: contribution.confidence_band.as_i64(),
        has_image: i64::from(contribution.has_image),
    }
}

fn checked_sub_counts(base: RollupCounts, prev: &EventContribution) -> Option<RollupCounts> {
    Some(RollupCounts {
        event_count: base.event_count.checked_sub(1)?,
        interval_event_count: base
            .interval_event_count
            .checked_sub(u64::from(prev.is_interval))?,
        open_interval_event_count: base
            .open_interval_event_count
            .checked_sub(u64::from(prev.is_open_interval))?,
        total_duration_ms: base.total_duration_ms.checked_sub(prev.duration_ms)?,
    })
}

fn checked_add_counts(base: RollupCounts, curr: &EventContribution) -> Result<RollupCounts> {
    Ok(RollupCounts {
        event_count: base
            .event_count
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("rollup event_count overflowed"))?,
        interval_event_count: base
            .interval_event_count
            .checked_add(u64::from(curr.is_interval))
            .ok_or_else(|| anyhow::anyhow!("rollup interval_event_count overflowed"))?,
        open_interval_event_count: base
            .open_interval_event_count
            .checked_add(u64::from(curr.is_open_interval))
            .ok_or_else(|| anyhow::anyhow!("rollup open_interval_event_count overflowed"))?,
        total_duration_ms: base
            .total_duration_ms
            .checked_add(curr.duration_ms)
            .ok_or_else(|| anyhow::anyhow!("rollup total_duration_ms overflowed"))?,
    })
}

async fn record_dirty_event(
    connection: &turso::Connection,
    event_id: &str,
    deleted: bool,
) -> Result<()> {
    let mut count_rows = connection
        .query("SELECT COUNT(*) FROM analytics_rebuild_dirty", ())
        .await?;
    let count = count_rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?
        .unwrap_or(0);
    drop(count_rows);
    if u64::try_from(count).unwrap_or(u64::MAX) >= MAX_REBUILD_DIRTY_ROWS {
        connection
            .execute(
                "UPDATE analytics_state
                 SET rebuild_cursor = NULL,
                     status_detail = 'rebuild dirty queue exceeded limit; restart required'
                 WHERE id = 1",
                (),
            )
            .await?;
        return Ok(());
    }
    connection
        .execute(
            "INSERT INTO analytics_rebuild_dirty (event_id, deleted) VALUES (?1, ?2)
             ON CONFLICT(event_id) DO UPDATE SET deleted = excluded.deleted",
            turso::params![event_id, i64::from(deleted)],
        )
        .await?;
    Ok(())
}

pub(super) async fn mark_degraded(connection: &turso::Connection, reason: &str) -> Result<()> {
    connection
        .execute(
            "UPDATE analytics_state
             SET state = 'degraded',
                 analytics_generation = analytics_generation + 1,
                 status_detail = ?1
             WHERE id = 1",
            turso::params![reason],
        )
        .await?;
    Ok(())
}

pub(super) fn apply_signed_delta(current: u64, delta: i64) -> Result<u64> {
    if delta >= 0 {
        current
            .checked_add(delta.cast_unsigned())
            .ok_or_else(|| anyhow::anyhow!("rollup row count overflowed"))
    } else {
        current
            .checked_sub(delta.unsigned_abs())
            .ok_or_else(|| anyhow::anyhow!("rollup row count underflowed"))
    }
}
