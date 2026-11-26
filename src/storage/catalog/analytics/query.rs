//! Authorized bounded summary query planner and SQL executor.

use super::{
    StateRow, daily_table, hourly_table, load_state,
    query_agg::{
        DeletionInterval, HealthInterval, RollupRowInput, SummaryAccumulator, build_heatmap,
        build_retention_evidence, build_source_health_summary, classify_completeness,
        finalize_buckets, rank_breakdowns, rank_confidence_breakdowns,
    },
};
use crate::storage::{
    analytics::{
        ANALYTICS_SCHEMA_VERSION, ActivityComparisonSummary, ActivityDimension, ActivityMeasures,
        ActivitySummaryFilter, ActivitySummaryQuery, ActivitySummaryResult, BucketGranularity,
        ComparisonMode, ConfidenceBand, EventContribution, ExpectedSourceState,
        MAX_EXACT_FALLBACK_RANGE_MS, MAX_RESPONSE_BYTES, MAX_WORKFLOW_SCAN_EVENTS,
        PROVENANCE_NOTICE, QueryDataSource, RollupState, floor_utc_bucket,
        time::{self, BucketSpec},
    },
    catalog::{EVENT_DETAIL_COLUMNS, event_from_row, to_u64, workflow},
    metadata::{EventSource, TimelineEvent},
    search::EventImageFilter,
};
use anyhow::{Result, ensure};
use chrono_tz::Tz;
use std::collections::BTreeSet;

const HOUR_MS: i64 = 3_600_000;

struct PlannedWindow {
    data_source: QueryDataSource,
    totals: ActivityMeasures,
    buckets: Vec<crate::storage::analytics::ActivityTimeBucket>,
    series: Vec<crate::storage::analytics::ActivitySeries>,
    series_truncated: bool,
    heatmap: Vec<crate::storage::analytics::ActivityHeatmapCell>,
    by_camera: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_event_type: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_zone: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_origin: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_confidence_band: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_group: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    retention: crate::storage::analytics::ActivityRetentionEvidence,
    source_health: crate::storage::analytics::ActivitySourceHealthSummary,
    window_completeness: crate::storage::analytics::CompletenessState,
    window_source_gap: bool,
}

/// Executes one bounded, authorized activity summary query against the catalog.
pub(super) async fn execute(
    connection: &turso::Connection,
    mut query: ActivitySummaryQuery,
) -> Result<ActivitySummaryResult> {
    let tz = query.validate()?;
    let state = load_state(connection).await?;
    ensure!(
        state.schema_version == ANALYTICS_SCHEMA_VERSION,
        "analytics schema version {} requires rebuild",
        state.schema_version
    );
    ensure!(
        state.state != RollupState::Unavailable,
        "activity analytics are unavailable"
    );
    let effective_cameras = query.effective_camera_ids();
    let main = execute_window(connection, &state, &query, tz, &effective_cameras).await?;
    let comparison = run_optional_comparison(
        connection,
        &state,
        &query,
        tz,
        &effective_cameras,
        main.totals.event_count,
    )
    .await?;
    assemble_result(query, &state, main, comparison)
}

async fn run_optional_comparison(
    connection: &turso::Connection,
    state: &StateRow,
    query: &ActivitySummaryQuery,
    tz: Tz,
    effective_cameras: &[String],
    current_events: u64,
) -> Result<Option<ActivityComparisonSummary>> {
    let Some((prev_start, prev_end)) = time::previous_comparison_window(
        query.start_time_ms,
        query.end_time_ms,
        tz,
        query.granularity,
        query.week_start,
        query.comparison,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(
        execute_comparison(
            connection,
            state,
            query,
            tz,
            effective_cameras,
            prev_start,
            prev_end,
            current_events,
        )
        .await?,
    ))
}

fn assemble_result(
    query: ActivitySummaryQuery,
    state: &StateRow,
    main: PlannedWindow,
    comparison: Option<ActivityComparisonSummary>,
) -> Result<ActivitySummaryResult> {
    let result = ActivitySummaryResult {
        generated_at_ms: query.now_ms,
        start_time_ms: query.start_time_ms,
        end_time_ms: query.end_time_ms,
        timezone: query.timezone,
        tzdb_version: time::tzdb_version().to_owned(),
        granularity: query.granularity,
        week_start: query.week_start,
        analytics_generation: state.analytics_generation,
        group_policy_revision: query.group_policy_revision,
        data_source: main.data_source,
        rollup_state: state.state,
        totals: main.totals,
        comparison,
        buckets: main.buckets,
        series: main.series,
        series_truncated: main.series_truncated,
        heatmap: main.heatmap,
        by_camera: main.by_camera,
        by_event_type: main.by_event_type,
        by_zone: main.by_zone,
        by_origin: main.by_origin,
        by_confidence_band: main.by_confidence_band,
        by_group: main.by_group,
        retention: main.retention,
        source_health: main.source_health,
        provenance_notice: PROVENANCE_NOTICE.to_owned(),
        exact: !main.series_truncated,
    };
    let encoded_size = serde_json::to_vec(&result)?.len();
    ensure!(
        encoded_size <= MAX_RESPONSE_BYTES,
        "activity summary response exceeds {MAX_RESPONSE_BYTES} byte limit"
    );
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn execute_comparison(
    connection: &turso::Connection,
    state: &StateRow,
    query: &ActivitySummaryQuery,
    tz: Tz,
    effective_cameras: &[String],
    prev_start_ms: i64,
    prev_end_ms: i64,
    current_events: u64,
) -> Result<ActivityComparisonSummary> {
    let mut prev_query = query.clone();
    prev_query.start_time_ms = prev_start_ms;
    prev_query.end_time_ms = prev_end_ms;
    prev_query.comparison = ComparisonMode::None;
    prev_query.group_by.clear();
    prev_query.include_heatmap = false;
    let prev = execute_window(connection, state, &prev_query, tz, effective_cameras).await?;
    let prev_events = prev.totals.event_count;
    let delta = i128::from(current_events) - i128::from(prev_events);
    let event_count_delta =
        i64::try_from(delta).unwrap_or(if delta < 0 { i64::MIN } else { i64::MAX });
    let event_count_delta_percent = (prev_events > 0)
        .then(|| ((current_events as f64 - prev_events as f64) * 100.0) / prev_events as f64);
    Ok(ActivityComparisonSummary {
        mode: query.comparison,
        start_time_ms: prev_start_ms,
        end_time_ms: prev_end_ms,
        totals: prev.totals,
        event_count_delta,
        event_count_delta_percent,
        completeness: prev.window_completeness,
        retention_partial: prev.retention.partial_retention,
        source_health_gap: prev.window_source_gap,
    })
}

struct BreakdownTables {
    by_camera: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_event_type: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_zone: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_origin: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_confidence_band: Vec<crate::storage::analytics::ActivityBreakdownRow>,
    by_group: Vec<crate::storage::analytics::ActivityBreakdownRow>,
}

async fn execute_window(
    connection: &turso::Connection,
    state: &StateRow,
    query: &ActivitySummaryQuery,
    tz: Tz,
    effective_cameras: &[String],
) -> Result<PlannedWindow> {
    let specs = time::build_time_buckets(
        query.start_time_ms,
        query.end_time_ms,
        tz,
        query.granularity,
        query.week_start,
        query.now_ms,
        query.max_buckets,
    )?;
    let mut acc = SummaryAccumulator::new(specs.len());
    let data_source = if effective_cameras.is_empty() {
        QueryDataSource::Rollups
    } else {
        populate_accumulator(
            connection,
            state,
            query,
            tz,
            &specs,
            effective_cameras,
            &mut acc,
        )
        .await?
    };
    acc.totals.finalize();
    let deletions = load_deletions(
        connection,
        effective_cameras,
        query.start_time_ms,
        query.end_time_ms,
    )
    .await?;
    let health = load_health_intervals(
        connection,
        effective_cameras,
        query.start_time_ms,
        query.end_time_ms,
    )
    .await?;
    let oldest_retained = load_oldest_retained_event(connection, effective_cameras).await?;
    Ok(finalize_window(
        query,
        effective_cameras,
        &specs,
        data_source,
        acc,
        oldest_retained,
        &deletions,
        &health,
    ))
}

#[allow(clippy::too_many_arguments)]
fn finalize_window(
    query: &ActivitySummaryQuery,
    effective_cameras: &[String],
    specs: &[BucketSpec],
    data_source: QueryDataSource,
    mut acc: SummaryAccumulator,
    oldest_retained: Option<i64>,
    deletions: &[DeletionInterval],
    health: &[HealthInterval],
) -> PlannedWindow {
    let retention = build_retention_evidence(oldest_retained, deletions);
    let source_health =
        build_source_health_summary(query, effective_cameras, &acc.by_camera, health);
    let buckets = finalize_buckets(
        query,
        specs,
        acc.bucket_measures.clone(),
        deletions,
        health,
        effective_cameras,
    );
    let window_source_gap = source_health.missing_health_cameras > 0;
    let window_completeness = classify_completeness(
        query.start_time_ms < query.now_ms && query.now_ms < query.end_time_ms,
        retention.partial_retention,
        window_source_gap,
        acc.totals.event_count,
        source_health.unknown_history_cameras < source_health.authorized_cameras
            || effective_cameras.is_empty(),
    );
    let (series, series_truncated) = acc.finalize_series(query);
    let heatmap = if query.include_heatmap {
        build_heatmap(&acc.heatmap_counts, window_completeness)
    } else {
        Vec::new()
    };
    let tables = build_breakdown_tables(query, &acc);
    PlannedWindow {
        data_source,
        totals: acc.totals,
        buckets,
        series,
        series_truncated,
        heatmap,
        by_camera: tables.by_camera,
        by_event_type: tables.by_event_type,
        by_zone: tables.by_zone,
        by_origin: tables.by_origin,
        by_confidence_band: tables.by_confidence_band,
        by_group: tables.by_group,
        retention,
        source_health,
        window_completeness,
        window_source_gap,
    }
}

fn build_breakdown_tables(
    query: &ActivitySummaryQuery,
    acc: &SummaryAccumulator,
) -> BreakdownTables {
    let total = acc.totals.event_count;
    let max = query.max_ranked_rows;
    BreakdownTables {
        by_camera: rank_breakdowns(&acc.by_camera, total, max, |id| {
            let label = query
                .authorized_cameras
                .get(id)
                .map_or_else(|| id.to_owned(), |cam| cam.display_name.clone());
            (label, false)
        }),
        by_event_type: rank_breakdowns(&acc.by_event_type, total, max, |kind| {
            (kind.to_owned(), false)
        }),
        by_zone: rank_breakdowns(&acc.by_zone, total, max, |zone| {
            if zone.is_empty() {
                ("Unknown zone".to_owned(), true)
            } else {
                (zone.to_owned(), false)
            }
        }),
        by_origin: rank_breakdowns(&acc.by_origin, total, max, |origin| {
            (origin.to_owned(), false)
        }),
        by_confidence_band: rank_confidence_breakdowns(&acc.by_confidence, total, max),
        by_group: rank_breakdowns(&acc.by_group, total, max, |id| {
            let label = query
                .authorized_groups
                .get(id)
                .map_or_else(|| id.to_owned(), |group| group.display_name.clone());
            (label, false)
        }),
    }
}

async fn populate_accumulator(
    connection: &turso::Connection,
    state: &StateRow,
    query: &ActivitySummaryQuery,
    tz: Tz,
    specs: &[BucketSpec],
    cameras: &[String],
    acc: &mut SummaryAccumulator,
) -> Result<QueryDataSource> {
    let span_ms = query.end_time_ms.saturating_sub(query.start_time_ms);
    let needs_workflow =
        query.filter.workflow.is_some() || query.group_by.contains(&ActivityDimension::Workflow);
    if needs_workflow || state.state == RollupState::Degraded {
        if state.state == RollupState::Degraded && !needs_workflow {
            ensure!(
                span_ms <= MAX_EXACT_FALLBACK_RANGE_MS,
                "activity analytics rollups are degraded; rebuild required for ranges exceeding 7 days"
            );
        }
        return run_canonical_exact(connection, query, tz, specs, cameras, acc).await;
    }
    let local_hours_aligned =
        time::utc_hours_align_with_local_hours(query.start_time_ms, query.end_time_ms, tz)?;
    if let Some((table, col)) = aligned_rollup_table(state, query, specs, local_hours_aligned) {
        scan_rollup_table(
            connection,
            table,
            col,
            query,
            tz,
            specs,
            cameras,
            query.start_time_ms,
            query.end_time_ms,
            acc,
        )
        .await?;
        adjust_open_or_crossing_intervals(connection, query, tz, specs, cameras, acc).await?;
        return Ok(QueryDataSource::Rollups);
    }
    if span_ms <= MAX_EXACT_FALLBACK_RANGE_MS {
        return run_canonical_exact(connection, query, tz, specs, cameras, acc).await;
    }
    ensure!(
        local_hours_aligned || !requires_local_hour_precision(query),
        "hourly heatmap and hour-of-day or weekday-weekend queries in sub-hour offset timezones exceed the 7-day exact scan limit"
    );
    populate_hybrid_rollups_and_edges(connection, state, query, tz, specs, cameras, acc).await?;
    Ok(QueryDataSource::HybridRollupsAndExactEdges)
}

async fn run_canonical_exact(
    connection: &turso::Connection,
    query: &ActivitySummaryQuery,
    tz: Tz,
    specs: &[BucketSpec],
    cameras: &[String],
    acc: &mut SummaryAccumulator,
) -> Result<QueryDataSource> {
    scan_canonical_range(
        connection,
        query,
        tz,
        specs,
        cameras,
        query.start_time_ms,
        query.end_time_ms,
        acc,
    )
    .await?;
    Ok(QueryDataSource::CanonicalExact)
}

const fn requires_local_hour_precision(query: &ActivitySummaryQuery) -> bool {
    query.include_heatmap
        || matches!(
            query.granularity,
            BucketGranularity::Hour
                | BucketGranularity::HourOfDay
                | BucketGranularity::WeekdayWeekend
        )
}

fn aligned_rollup_table(
    state: &StateRow,
    query: &ActivitySummaryQuery,
    specs: &[BucketSpec],
    local_hours_aligned: bool,
) -> Option<(&'static str, &'static str)> {
    let day_aligned = specs.iter().all(|bucket| bucket.day_aligned_utc)
        && !query.include_heatmap
        && matches!(
            query.granularity,
            BucketGranularity::Day | BucketGranularity::Week | BucketGranularity::Month
        );
    if day_aligned {
        return Some((daily_table(state.active_generation), "day_utc_ms"));
    }
    let heatmap_compatible = !query.include_heatmap || local_hours_aligned;
    if heatmap_compatible && specs.iter().all(|bucket| bucket.hour_aligned_utc) {
        return Some((hourly_table(state.active_generation), "hour_utc_ms"));
    }
    None
}

async fn populate_hybrid_rollups_and_edges(
    connection: &turso::Connection,
    state: &StateRow,
    query: &ActivitySummaryQuery,
    tz: Tz,
    specs: &[BucketSpec],
    cameras: &[String],
    acc: &mut SummaryAccumulator,
) -> Result<()> {
    let mut exact_hours = BTreeSet::new();
    for spec in specs {
        if spec.start_time_ms.rem_euclid(HOUR_MS) != 0 {
            exact_hours.insert(floor_utc_bucket(spec.start_time_ms, HOUR_MS)?);
        }
        if spec.end_time_ms.rem_euclid(HOUR_MS) != 0 {
            exact_hours.insert(floor_utc_bucket(spec.end_time_ms, HOUR_MS)?);
        }
    }
    let table = hourly_table(state.active_generation);
    let min_hour = floor_utc_bucket(query.start_time_ms, HOUR_MS)?;
    let max_hour = floor_utc_bucket(query.end_time_ms - 1, HOUR_MS)?;
    scan_rollup_table_excluding_hours(
        connection,
        table,
        query,
        tz,
        specs,
        cameras,
        min_hour,
        max_hour.saturating_add(HOUR_MS),
        &exact_hours,
        acc,
    )
    .await?;
    adjust_open_or_crossing_intervals_excluding_hours(
        connection,
        query,
        tz,
        specs,
        cameras,
        &exact_hours,
        acc,
    )
    .await?;
    for hour_start in exact_hours {
        let slice_start = hour_start.max(query.start_time_ms);
        let slice_end = hour_start.saturating_add(HOUR_MS).min(query.end_time_ms);
        if slice_start < slice_end {
            scan_canonical_range(
                connection,
                query,
                tz,
                specs,
                cameras,
                slice_start,
                slice_end,
                acc,
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn scan_rollup_table(
    connection: &turso::Connection,
    table: &str,
    bucket_col: &str,
    query: &ActivitySummaryQuery,
    tz: Tz,
    specs: &[BucketSpec],
    cameras: &[String],
    start_ms: i64,
    end_ms: i64,
    acc: &mut SummaryAccumulator,
) -> Result<()> {
    let empty = BTreeSet::new();
    scan_rollup_table_filtered(
        connection, table, bucket_col, query, tz, specs, cameras, start_ms, end_ms, &empty, acc,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn scan_rollup_table_excluding_hours(
    connection: &turso::Connection,
    table: &str,
    query: &ActivitySummaryQuery,
    tz: Tz,
    specs: &[BucketSpec],
    cameras: &[String],
    start_ms: i64,
    end_ms: i64,
    excluded_hours: &BTreeSet<i64>,
    acc: &mut SummaryAccumulator,
) -> Result<()> {
    scan_rollup_table_filtered(
        connection,
        table,
        "hour_utc_ms",
        query,
        tz,
        specs,
        cameras,
        start_ms,
        end_ms,
        excluded_hours,
        acc,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn scan_rollup_table_filtered(
    connection: &turso::Connection,
    table: &str,
    bucket_col: &str,
    query: &ActivitySummaryQuery,
    tz: Tz,
    specs: &[BucketSpec],
    cameras: &[String],
    start_ms: i64,
    end_ms: i64,
    excluded_buckets: &BTreeSet<i64>,
    acc: &mut SummaryAccumulator,
) -> Result<()> {
    let mut params = vec![
        turso::Value::Integer(start_ms),
        turso::Value::Integer(end_ms),
    ];
    let mut sql = format!(
        "SELECT {bucket_col}, camera_id, origin, event_type, zone_key, confidence_band,
                has_image, event_count, interval_event_count, open_interval_event_count,
                total_duration_ms
         FROM {table}
         WHERE {bucket_col} >= ?1 AND {bucket_col} < ?2"
    );
    append_in_clause(&mut sql, &mut params, "camera_id", cameras);
    append_rollup_filters(&mut sql, &mut params, &query.filter);
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    while let Some(row) = rows.next().await? {
        let bucket_ms: i64 = row.get(0)?;
        if excluded_buckets.contains(&bucket_ms) {
            continue;
        }
        let origin_str: String = row.get(2)?;
        let origin = match origin_str.as_str() {
            "camera" => EventSource::Camera,
            "keeppeek" => EventSource::KeepPeek,
            other => anyhow::bail!("invalid rollup event origin '{other}'"),
        };
        let confidence_band = ConfidenceBand::from_i64(row.get(5)?)
            .ok_or_else(|| anyhow::anyhow!("invalid rollup confidence band"))?;
        let input = RollupRowInput {
            bucket_ms,
            camera_id: row.get(1)?,
            origin,
            event_type: row.get(3)?,
            zone_key: row.get(4)?,
            confidence_band,
            has_image: row.get::<i64>(6)? != 0,
            event_count: to_u64(row.get(7)?, "rollup event_count")?,
            interval_event_count: to_u64(row.get(8)?, "rollup interval_event_count")?,
            open_interval_event_count: to_u64(row.get(9)?, "rollup open_interval_event_count")?,
            total_duration_ms: to_u64(row.get(10)?, "rollup total_duration_ms")?,
        };
        acc.add_rollup_row(query, tz, specs, &input)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn scan_canonical_range(
    connection: &turso::Connection,
    query: &ActivitySummaryQuery,
    tz: Tz,
    specs: &[BucketSpec],
    cameras: &[String],
    start_ms: i64,
    end_ms: i64,
    acc: &mut SummaryAccumulator,
) -> Result<()> {
    let mut params = vec![
        turso::Value::Integer(start_ms),
        turso::Value::Integer(end_ms),
    ];
    let mut sql = format!(
        "SELECT {EVENT_DETAIL_COLUMNS}
         FROM recording_events AS e
         WHERE e.start_time_ms >= ?1 AND e.start_time_ms < ?2"
    );
    append_in_clause(&mut sql, &mut params, "e.camera_id", cameras);
    if let Some(wf) = &query.filter.workflow {
        workflow::append_filter(Some(wf), &mut sql, &mut params)?;
    }
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    let mut scanned = 0u64;
    let mut events = Vec::new();
    while let Some(row) = rows.next().await? {
        scanned += 1;
        ensure!(
            scanned <= MAX_WORKFLOW_SCAN_EVENTS,
            "exact event scan exceeded {MAX_WORKFLOW_SCAN_EVENTS} event limit"
        );
        let event = event_from_row(&row, true)?;
        let contribution = EventContribution::from_event(&event)?;
        if matches_filter(&contribution, &query.filter) {
            events.push((event, contribution));
        }
    }
    drop(rows);
    let actor = query
        .filter
        .workflow
        .as_ref()
        .map(|wf| wf.actor_id.as_str())
        .unwrap_or("");
    let needs_wf_label = query.group_by.contains(&ActivityDimension::Workflow);
    for (event, contribution) in &events {
        let label = if needs_wf_label {
            Some(load_event_workflow_label(connection, actor, &event.camera_id, &event.id).await?)
        } else {
            None
        };
        acc.add_event(query, tz, specs, contribution, label.as_deref())?;
    }
    Ok(())
}

async fn adjust_open_or_crossing_intervals(
    connection: &turso::Connection,
    query: &ActivitySummaryQuery,
    tz: Tz,
    specs: &[BucketSpec],
    cameras: &[String],
    acc: &mut SummaryAccumulator,
) -> Result<()> {
    let empty = BTreeSet::new();
    adjust_open_or_crossing_intervals_excluding_hours(
        connection, query, tz, specs, cameras, &empty, acc,
    )
    .await
}

async fn adjust_open_or_crossing_intervals_excluding_hours(
    connection: &turso::Connection,
    query: &ActivitySummaryQuery,
    tz: Tz,
    specs: &[BucketSpec],
    cameras: &[String],
    excluded_hours: &BTreeSet<i64>,
    acc: &mut SummaryAccumulator,
) -> Result<()> {
    let mut params = vec![
        turso::Value::Integer(query.start_time_ms),
        turso::Value::Integer(query.end_time_ms),
    ];
    let mut sql = format!(
        "SELECT {EVENT_DETAIL_COLUMNS}
         FROM recording_events AS e
         WHERE e.start_time_ms >= ?1 AND e.start_time_ms < ?2
           AND (e.end_time_ms IS NULL OR e.end_time_ms > ?2 OR (e.end_time_ms - e.start_time_ms) > 0)"
    );
    append_in_clause(&mut sql, &mut params, "e.camera_id", cameras);
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    while let Some(row) = rows.next().await? {
        let event: TimelineEvent = event_from_row(&row, true)?;
        let contribution = EventContribution::from_event(&event)?;
        if excluded_hours.contains(&contribution.hour_utc_ms)
            || !matches_filter(&contribution, &query.filter)
        {
            continue;
        }
        let needs_clip = contribution.is_open_interval
            || event.end_time_ms.is_some_and(|end| {
                end > query.end_time_ms || crosses_bucket(specs, event.start_time_ms, end)
            });
        if needs_clip {
            acc.adjust_interval_duration(query, tz, specs, &contribution)?;
        }
    }
    Ok(())
}

fn crosses_bucket(specs: &[BucketSpec], start_ms: i64, end_ms: i64) -> bool {
    let idx = specs.partition_point(|bucket| bucket.end_time_ms <= start_ms);
    idx < specs.len() && end_ms > specs[idx].end_time_ms
}

async fn load_event_workflow_label(
    connection: &turso::Connection,
    actor: &str,
    camera_id: &str,
    event_id: &str,
) -> Result<String> {
    if !actor.is_empty() {
        let mut rev = connection
            .query(
                "SELECT reviewed, dismissed FROM event_reviews
                 WHERE source_id = ?1 AND event_id = ?2 AND principal_id = ?3",
                turso::params![camera_id, event_id, actor],
            )
            .await?;
        if let Some(row) = rev.next().await? {
            if row.get::<i64>(1)? != 0 {
                return Ok("dismissed".to_owned());
            }
            if row.get::<i64>(0)? != 0 {
                return Ok("reviewed".to_owned());
            }
        }
    }
    let mut bm = connection
        .query(
            "SELECT 1 FROM event_bookmarks WHERE source_id = ?1 AND event_id = ?2 AND active = 1",
            turso::params![camera_id, event_id],
        )
        .await?;
    if bm.next().await?.is_some() {
        return Ok("bookmarked".to_owned());
    }
    Ok("unreviewed".to_owned())
}

async fn load_deletions(
    connection: &turso::Connection,
    cameras: &[String],
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<DeletionInterval>> {
    if cameras.is_empty() {
        return Ok(Vec::new());
    }
    let mut params = vec![
        turso::Value::Integer(end_ms),
        turso::Value::Integer(start_ms),
    ];
    let mut sql = String::from(
        "SELECT start_time_ms, end_time_ms, deleted_count
         FROM analytics_event_deletions
         WHERE start_time_ms < ?1 AND end_time_ms > ?2",
    );
    append_in_clause(&mut sql, &mut params, "camera_id", cameras);
    sql.push_str(" ORDER BY start_time_ms ASC LIMIT 256");
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(DeletionInterval {
            start_time_ms: row.get(0)?,
            end_time_ms: row.get(1)?,
            deleted_count: to_u64(row.get(2)?, "deleted_count")?,
        });
    }
    Ok(out)
}

async fn load_health_intervals(
    connection: &turso::Connection,
    cameras: &[String],
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<HealthInterval>> {
    if cameras.is_empty() {
        return Ok(Vec::new());
    }
    let mut params = vec![
        turso::Value::Integer(end_ms),
        turso::Value::Integer(start_ms),
    ];
    let mut sql = String::from(
        "SELECT camera_id, start_time_ms, end_time_ms, state
         FROM analytics_source_health_intervals
         WHERE start_time_ms < ?1 AND (end_time_ms IS NULL OR end_time_ms > ?2)",
    );
    append_in_clause(&mut sql, &mut params, "camera_id", cameras);
    sql.push_str(" ORDER BY start_time_ms ASC LIMIT 4096");
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let state_str: String = row.get(3)?;
        if let Some(state) = ExpectedSourceState::parse(&state_str) {
            out.push(HealthInterval {
                camera_id: row.get(0)?,
                start_time_ms: row.get(1)?,
                end_time_ms: row.get(2)?,
                state,
            });
        }
    }
    Ok(out)
}

async fn load_oldest_retained_event(
    connection: &turso::Connection,
    cameras: &[String],
) -> Result<Option<i64>> {
    if cameras.is_empty() {
        return Ok(None);
    }
    let mut params = Vec::new();
    let mut sql = String::from("SELECT MIN(start_time_ms) FROM recording_events WHERE 1 = 1");
    append_in_clause(&mut sql, &mut params, "camera_id", cameras);
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    Ok(rows
        .next()
        .await?
        .map(|row| row.get::<Option<i64>>(0))
        .transpose()?
        .flatten())
}

fn append_in_clause(
    sql: &mut String,
    params: &mut Vec<turso::Value>,
    column: &str,
    values: &[String],
) {
    if values.is_empty() {
        return;
    }
    let placeholders: Vec<String> = values
        .iter()
        .map(|value| {
            params.push(turso::Value::Text(value.clone()));
            format!("?{}", params.len())
        })
        .collect();
    sql.push_str(&format!(" AND {column} IN ({})", placeholders.join(",")));
}

fn append_rollup_filters(
    sql: &mut String,
    params: &mut Vec<turso::Value>,
    filter: &ActivitySummaryFilter,
) {
    append_in_clause(sql, params, "event_type", &filter.event_types);
    if !filter.origins.is_empty() {
        let origins: Vec<String> = filter
            .origins
            .iter()
            .map(|origin| origin.as_str().to_owned())
            .collect();
        append_in_clause(sql, params, "origin", &origins);
    }
    if !filter.zones.is_empty() || filter.include_missing_zone {
        let mut zones = filter.zones.clone();
        if filter.include_missing_zone {
            zones.push(String::new());
        }
        append_in_clause(sql, params, "zone_key", &zones);
    }
    if !filter.confidence_bands.is_empty() {
        let placeholders: Vec<String> = filter
            .confidence_bands
            .iter()
            .map(|band| {
                params.push(turso::Value::Integer(band.as_i64()));
                format!("?{}", params.len())
            })
            .collect();
        sql.push_str(&format!(
            " AND confidence_band IN ({})",
            placeholders.join(",")
        ));
    }
    match filter.image {
        EventImageFilter::Any => {}
        EventImageFilter::WithImage => sql.push_str(" AND has_image = 1"),
        EventImageFilter::WithoutImage => sql.push_str(" AND has_image = 0"),
    }
}

fn matches_filter(contribution: &EventContribution, filter: &ActivitySummaryFilter) -> bool {
    if !filter.event_types.is_empty() && !filter.event_types.contains(&contribution.event_type) {
        return false;
    }
    if !filter.origins.is_empty() && !filter.origins.contains(&contribution.origin) {
        return false;
    }
    if !filter.zones.is_empty() || filter.include_missing_zone {
        let matches_zone = if contribution.zone_key.is_empty() {
            filter.include_missing_zone
        } else {
            filter.zones.contains(&contribution.zone_key)
        };
        if !matches_zone {
            return false;
        }
    }
    if !filter.confidence_bands.is_empty()
        && !filter
            .confidence_bands
            .contains(&contribution.confidence_band)
    {
        return false;
    }
    match filter.image {
        EventImageFilter::Any => true,
        EventImageFilter::WithImage => contribution.has_image,
        EventImageFilter::WithoutImage => !contribution.has_image,
    }
}
