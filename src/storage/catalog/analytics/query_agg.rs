//! Pure bounded accumulator, breakdown ranking, heatmap, and completeness evaluation.

use crate::storage::{
    analytics::{
        ActivityBreakdownRow, ActivityCameraSourceHealth, ActivityDimension, ActivityHeatmapCell,
        ActivityMeasures, ActivityRetentionEvidence, ActivitySeries, ActivitySourceHealthSummary,
        ActivitySummaryQuery, ActivityTimeBucket, BucketGranularity, CameraHealthStatus,
        CompletenessState, ConfidenceBand, EventContribution, ExpectedSourceState,
        time::{BucketSpec, local_weekday_and_hour},
    },
    metadata::EventSource,
};
use anyhow::Result;
use chrono_tz::Tz;
use std::collections::{BTreeMap, BTreeSet};

/// One persisted source-health interval loaded for an authorized camera.
#[derive(Debug, Clone)]
pub(super) struct HealthInterval {
    pub camera_id: String,
    pub start_time_ms: i64,
    pub end_time_ms: Option<i64>,
    pub state: ExpectedSourceState,
}

/// One persisted deletion interval loaded for an authorized camera.
#[derive(Debug, Clone)]
pub(super) struct DeletionInterval {
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub deleted_count: u64,
}

/// One pre-aggregated rollup row fed into the accumulator.
#[derive(Debug, Clone)]
pub(super) struct RollupRowInput {
    pub bucket_ms: i64,
    pub camera_id: String,
    pub origin: EventSource,
    pub event_type: String,
    pub zone_key: String,
    pub confidence_band: ConfidenceBand,
    pub has_image: bool,
    pub event_count: u64,
    pub interval_event_count: u64,
    pub open_interval_event_count: u64,
    pub total_duration_ms: u64,
}

pub(super) struct SummaryAccumulator {
    pub totals: ActivityMeasures,
    pub bucket_measures: Vec<ActivityMeasures>,
    pub heatmap_counts: [[u64; 24]; 7],
    pub by_camera: BTreeMap<String, ActivityMeasures>,
    pub by_event_type: BTreeMap<String, ActivityMeasures>,
    pub by_zone: BTreeMap<String, ActivityMeasures>,
    pub by_origin: BTreeMap<String, ActivityMeasures>,
    pub by_confidence: BTreeMap<ConfidenceBand, ActivityMeasures>,
    pub by_group: BTreeMap<String, ActivityMeasures>,
    series: BTreeMap<BTreeMap<ActivityDimension, String>, (ActivityMeasures, Vec<u64>)>,
    pub series_truncated: bool,
}

impl SummaryAccumulator {
    pub(super) fn new(bucket_count: usize) -> Self {
        Self {
            totals: ActivityMeasures::default(),
            bucket_measures: vec![ActivityMeasures::default(); bucket_count],
            heatmap_counts: [[0; 24]; 7],
            by_camera: BTreeMap::new(),
            by_event_type: BTreeMap::new(),
            by_zone: BTreeMap::new(),
            by_origin: BTreeMap::new(),
            by_confidence: BTreeMap::new(),
            by_group: BTreeMap::new(),
            series: BTreeMap::new(),
            series_truncated: false,
        }
    }

    /// Adds one canonical event contribution into totals, bucket, heatmap, breakdowns, and series.
    pub(super) fn add_event(
        &mut self,
        query: &ActivitySummaryQuery,
        tz: Tz,
        buckets: &[BucketSpec],
        contribution: &EventContribution,
        workflow_label: Option<&str>,
    ) -> Result<()> {
        let total_dur =
            contribution.clipped_duration_ms(query.start_time_ms, query.end_time_ms, query.now_ms);
        self.totals.add_contribution(contribution, total_dur)?;
        let bucket_idx = locate_bucket(query.granularity, tz, buckets, contribution.start_time_ms)?;
        if let Some(idx) = bucket_idx {
            let bucket_dur = match query.granularity {
                BucketGranularity::HourOfDay | BucketGranularity::WeekdayWeekend => total_dur,
                _ => contribution.clipped_duration_ms(
                    buckets[idx].start_time_ms,
                    buckets[idx].end_time_ms,
                    query.now_ms,
                ),
            };
            self.bucket_measures[idx].add_contribution(contribution, bucket_dur)?;
        }
        if query.include_heatmap {
            let (weekday, hour) = local_weekday_and_hour(contribution.start_time_ms, tz)?;
            let cell = &mut self.heatmap_counts[usize::from(weekday - 1)][usize::from(hour)];
            *cell = cell
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("heatmap cell overflowed"))?;
        }
        self.add_breakdowns_from_event(query, contribution, total_dur)?;
        if !query.group_by.is_empty() {
            self.add_series_from_event(
                query,
                buckets.len(),
                bucket_idx,
                contribution,
                total_dur,
                workflow_label,
            )?;
        }
        Ok(())
    }

    /// Adds one UTC-hour or UTC-day rollup row into totals, bucket, heatmap, breakdowns, and series.
    pub(super) fn add_rollup_row(
        &mut self,
        query: &ActivitySummaryQuery,
        tz: Tz,
        buckets: &[BucketSpec],
        row: &RollupRowInput,
    ) -> Result<()> {
        let has_conf = row.confidence_band != ConfidenceBand::Unknown;
        let has_zone = !row.zone_key.is_empty();
        self.totals.add_rollup_counts(
            row.event_count,
            row.interval_event_count,
            row.open_interval_event_count,
            row.total_duration_ms,
            row.has_image,
            has_conf,
            has_zone,
        )?;
        let bucket_idx = locate_bucket(query.granularity, tz, buckets, row.bucket_ms)?;
        if let Some(idx) = bucket_idx {
            self.bucket_measures[idx].add_rollup_counts(
                row.event_count,
                row.interval_event_count,
                row.open_interval_event_count,
                row.total_duration_ms,
                row.has_image,
                has_conf,
                has_zone,
            )?;
        }
        if query.include_heatmap {
            let (weekday, hour) = local_weekday_and_hour(row.bucket_ms, tz)?;
            let cell = &mut self.heatmap_counts[usize::from(weekday - 1)][usize::from(hour)];
            *cell = cell
                .checked_add(row.event_count)
                .ok_or_else(|| anyhow::anyhow!("heatmap cell overflowed"))?;
        }
        self.add_breakdowns_from_rollup(query, row, has_conf, has_zone)?;
        if !query.group_by.is_empty() {
            self.add_series_from_rollup(query, buckets.len(), bucket_idx, row, has_conf, has_zone)?;
        }
        Ok(())
    }

    /// Adjusts total and bucket durations for open or cross-bucket interval events.
    pub(super) fn adjust_interval_duration(
        &mut self,
        query: &ActivitySummaryQuery,
        tz: Tz,
        buckets: &[BucketSpec],
        contribution: &EventContribution,
    ) -> Result<()> {
        let stored = contribution.duration_ms;
        let clipped_total =
            contribution.clipped_duration_ms(query.start_time_ms, query.end_time_ms, query.now_ms);
        replace_duration(&mut self.totals, stored, clipped_total);
        if let Some(idx) =
            locate_bucket(query.granularity, tz, buckets, contribution.start_time_ms)?
        {
            let clipped_bucket = match query.granularity {
                BucketGranularity::HourOfDay | BucketGranularity::WeekdayWeekend => clipped_total,
                _ => contribution.clipped_duration_ms(
                    buckets[idx].start_time_ms,
                    buckets[idx].end_time_ms,
                    query.now_ms,
                ),
            };
            replace_duration(&mut self.bucket_measures[idx], stored, clipped_bucket);
        }
        if let Some(entry) = self.by_camera.get_mut(&contribution.camera_id) {
            replace_duration(entry, stored, clipped_total);
        }
        if let Some(entry) = self.by_event_type.get_mut(&contribution.event_type) {
            replace_duration(entry, stored, clipped_total);
        }
        if let Some(entry) = self.by_zone.get_mut(&contribution.zone_key) {
            replace_duration(entry, stored, clipped_total);
        }
        if let Some(entry) = self.by_origin.get_mut(contribution.origin.as_str()) {
            replace_duration(entry, stored, clipped_total);
        }
        if let Some(entry) = self.by_confidence.get_mut(&contribution.confidence_band) {
            replace_duration(entry, stored, clipped_total);
        }
        for group_id in camera_groups(query, &contribution.camera_id) {
            if let Some(entry) = self.by_group.get_mut(&group_id) {
                replace_duration(entry, stored, clipped_total);
            }
        }
        Ok(())
    }

    fn add_breakdowns_from_event(
        &mut self,
        query: &ActivitySummaryQuery,
        contribution: &EventContribution,
        duration_ms: u64,
    ) -> Result<()> {
        self.by_camera
            .entry(contribution.camera_id.clone())
            .or_default()
            .add_contribution(contribution, duration_ms)?;
        self.by_event_type
            .entry(contribution.event_type.clone())
            .or_default()
            .add_contribution(contribution, duration_ms)?;
        self.by_zone
            .entry(contribution.zone_key.clone())
            .or_default()
            .add_contribution(contribution, duration_ms)?;
        self.by_origin
            .entry(contribution.origin.as_str().to_owned())
            .or_default()
            .add_contribution(contribution, duration_ms)?;
        self.by_confidence
            .entry(contribution.confidence_band)
            .or_default()
            .add_contribution(contribution, duration_ms)?;
        for group_id in camera_groups(query, &contribution.camera_id) {
            self.by_group
                .entry(group_id)
                .or_default()
                .add_contribution(contribution, duration_ms)?;
        }
        Ok(())
    }

    fn add_breakdowns_from_rollup(
        &mut self,
        query: &ActivitySummaryQuery,
        row: &RollupRowInput,
        has_conf: bool,
        has_zone: bool,
    ) -> Result<()> {
        let apply = |target: &mut ActivityMeasures| {
            target.add_rollup_counts(
                row.event_count,
                row.interval_event_count,
                row.open_interval_event_count,
                row.total_duration_ms,
                row.has_image,
                has_conf,
                has_zone,
            )
        };
        apply(self.by_camera.entry(row.camera_id.clone()).or_default())?;
        apply(
            self.by_event_type
                .entry(row.event_type.clone())
                .or_default(),
        )?;
        apply(self.by_zone.entry(row.zone_key.clone()).or_default())?;
        apply(
            self.by_origin
                .entry(row.origin.as_str().to_owned())
                .or_default(),
        )?;
        apply(self.by_confidence.entry(row.confidence_band).or_default())?;
        for group_id in camera_groups(query, &row.camera_id) {
            apply(self.by_group.entry(group_id).or_default())?;
        }
        Ok(())
    }

    fn add_series_from_event(
        &mut self,
        query: &ActivitySummaryQuery,
        bucket_len: usize,
        bucket_idx: Option<usize>,
        contribution: &EventContribution,
        duration_ms: u64,
        workflow_label: Option<&str>,
    ) -> Result<()> {
        let keys = build_series_keys(
            query,
            &contribution.camera_id,
            contribution.origin,
            &contribution.event_type,
            &contribution.zone_key,
            contribution.confidence_band,
            contribution.has_image,
            workflow_label,
        );
        for key in keys {
            let entry = self
                .series
                .entry(key)
                .or_insert_with(|| (ActivityMeasures::default(), vec![0; bucket_len]));
            entry.0.add_contribution(contribution, duration_ms)?;
            if let Some(idx) = bucket_idx {
                entry.1[idx] = entry.1[idx]
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("series bucket count overflowed"))?;
            }
        }
        Ok(())
    }

    fn add_series_from_rollup(
        &mut self,
        query: &ActivitySummaryQuery,
        bucket_len: usize,
        bucket_idx: Option<usize>,
        row: &RollupRowInput,
        has_conf: bool,
        has_zone: bool,
    ) -> Result<()> {
        let keys = build_series_keys(
            query,
            &row.camera_id,
            row.origin,
            &row.event_type,
            &row.zone_key,
            row.confidence_band,
            row.has_image,
            None,
        );
        for key in keys {
            let entry = self
                .series
                .entry(key)
                .or_insert_with(|| (ActivityMeasures::default(), vec![0; bucket_len]));
            entry.0.add_rollup_counts(
                row.event_count,
                row.interval_event_count,
                row.open_interval_event_count,
                row.total_duration_ms,
                row.has_image,
                has_conf,
                has_zone,
            )?;
            if let Some(idx) = bucket_idx {
                entry.1[idx] = entry.1[idx]
                    .checked_add(row.event_count)
                    .ok_or_else(|| anyhow::anyhow!("series bucket count overflowed"))?;
            }
        }
        Ok(())
    }

    /// Finalizes grouped series, enforcing `query.max_series` with deterministic ranking.
    pub(super) fn finalize_series(
        &mut self,
        query: &ActivitySummaryQuery,
    ) -> (Vec<ActivitySeries>, bool) {
        let mut all: Vec<ActivitySeries> = self
            .series
            .iter()
            .map(|(dims, (measures, counts))| {
                let mut finalized = measures.clone();
                finalized.finalize();
                let label = dims
                    .iter()
                    .map(|(dim, value)| format!("{}: {}", dim.as_str(), value))
                    .collect::<Vec<_>>()
                    .join(" / ");
                ActivitySeries {
                    dimension_values: dims.clone(),
                    label,
                    totals: finalized,
                    bucket_counts: counts.clone(),
                }
            })
            .collect();
        all.sort_by(|left, right| {
            right
                .totals
                .event_count
                .cmp(&left.totals.event_count)
                .then_with(|| left.label.cmp(&right.label))
        });
        let truncated = all.len() > query.max_series;
        all.truncate(query.max_series);
        self.series_truncated = truncated;
        (all, truncated)
    }
}

pub(super) fn finalize_buckets(
    query: &ActivitySummaryQuery,
    specs: &[BucketSpec],
    measures: Vec<ActivityMeasures>,
    deletions: &[DeletionInterval],
    health: &[HealthInterval],
    effective_cameras: &[String],
) -> Vec<ActivityTimeBucket> {
    let mut result = Vec::with_capacity(specs.len());
    for (spec, mut m) in specs.iter().zip(measures) {
        m.finalize();
        let retention_partial = deletions.iter().any(|del| {
            del.start_time_ms < spec.end_time_ms && del.end_time_ms > spec.start_time_ms
        });
        let (source_health_gap, has_any_health_evidence) = bucket_health_flags(
            spec.start_time_ms,
            spec.end_time_ms,
            health,
            effective_cameras,
        );
        let completeness = classify_completeness(
            spec.partial_current,
            retention_partial,
            source_health_gap,
            m.event_count,
            has_any_health_evidence,
        );
        if !query.dense_buckets && m.event_count == 0 && completeness == CompletenessState::Complete
        {
            continue;
        }
        result.push(ActivityTimeBucket {
            start_time_ms: spec.start_time_ms,
            end_time_ms: spec.end_time_ms,
            label: spec.label.clone(),
            utc_offset_seconds: spec.utc_offset_seconds,
            dst_fold: spec.dst_fold,
            partial_current: spec.partial_current,
            completeness,
            retention_partial,
            source_health_gap,
            measures: m,
        });
    }
    result
}

pub(super) fn build_heatmap(
    counts: &[[u64; 24]; 7],
    window_completeness: CompletenessState,
) -> Vec<ActivityHeatmapCell> {
    let mut cells = Vec::with_capacity(168);
    for weekday in 1..=7u8 {
        for local_hour in 0..24u8 {
            let count = counts[usize::from(weekday - 1)][usize::from(local_hour)];
            cells.push(ActivityHeatmapCell {
                weekday,
                local_hour,
                event_count: count,
                distinct_event_count: count,
                completeness: window_completeness,
            });
        }
    }
    cells
}

pub(super) fn rank_breakdowns<F>(
    map: &BTreeMap<String, ActivityMeasures>,
    total_events: u64,
    max_rows: usize,
    label_fn: F,
) -> Vec<ActivityBreakdownRow>
where
    F: Fn(&str) -> (String, bool),
{
    let mut rows: Vec<ActivityBreakdownRow> = map
        .iter()
        .map(|(key, measures)| {
            let mut finalized = measures.clone();
            finalized.finalize();
            let (label, is_unknown) = label_fn(key);
            let share_percent = if total_events > 0 {
                (finalized.event_count as f64 * 100.0) / total_events as f64
            } else {
                0.0
            };
            ActivityBreakdownRow {
                key: key.clone(),
                label,
                is_unknown,
                measures: finalized,
                share_percent,
            }
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .measures
            .event_count
            .cmp(&left.measures.event_count)
            .then_with(|| left.key.cmp(&right.key))
    });
    rows.truncate(max_rows);
    rows
}

pub(super) fn rank_confidence_breakdowns(
    map: &BTreeMap<ConfidenceBand, ActivityMeasures>,
    total_events: u64,
    max_rows: usize,
) -> Vec<ActivityBreakdownRow> {
    let mut rows: Vec<ActivityBreakdownRow> = map
        .iter()
        .map(|(band, measures)| {
            let mut finalized = measures.clone();
            finalized.finalize();
            let share_percent = if total_events > 0 {
                (finalized.event_count as f64 * 100.0) / total_events as f64
            } else {
                0.0
            };
            ActivityBreakdownRow {
                key: band.as_str().to_owned(),
                label: band.display_label().to_owned(),
                is_unknown: *band == ConfidenceBand::Unknown,
                measures: finalized,
                share_percent,
            }
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .measures
            .event_count
            .cmp(&left.measures.event_count)
            .then_with(|| left.key.cmp(&right.key))
    });
    rows.truncate(max_rows);
    rows
}

pub(super) fn build_retention_evidence(
    oldest_retained_event_ms: Option<i64>,
    deletions: &[DeletionInterval],
) -> ActivityRetentionEvidence {
    let deleted_event_count_in_range: u64 = deletions
        .iter()
        .fold(0u64, |acc, item| acc.saturating_add(item.deleted_count));
    let deleted_intervals: Vec<(i64, i64)> = deletions
        .iter()
        .take(64)
        .map(|item| (item.start_time_ms, item.end_time_ms))
        .collect();
    ActivityRetentionEvidence {
        partial_retention: deleted_event_count_in_range > 0,
        oldest_retained_event_ms,
        deleted_event_count_in_range,
        deleted_intervals,
    }
}

pub(super) fn build_source_health_summary(
    query: &ActivitySummaryQuery,
    effective_cameras: &[String],
    by_camera: &BTreeMap<String, ActivityMeasures>,
    intervals: &[HealthInterval],
) -> ActivitySourceHealthSummary {
    let mut summary = ActivitySourceHealthSummary {
        authorized_cameras: effective_cameras.len() as u64,
        ..Default::default()
    };
    let effective_end = query.end_time_ms.min(query.now_ms.max(query.start_time_ms));
    for camera_id in effective_cameras {
        let display_name = query
            .authorized_cameras
            .get(camera_id)
            .map_or_else(|| camera_id.clone(), |cam| cam.display_name.clone());
        let reported_events = by_camera.get(camera_id).map_or(0, |m| m.event_count);
        let (healthy_ms, missing_ms, no_expected_ms) =
            camera_health_durations(camera_id, query.start_time_ms, effective_end, intervals);
        let status = if missing_ms > 0 {
            summary.missing_health_cameras += 1;
            CameraHealthStatus::MissingHealth
        } else if reported_events > 0 {
            summary.reporting_cameras += 1;
            CameraHealthStatus::Reporting
        } else if healthy_ms > 0 {
            summary.healthy_no_reports_cameras += 1;
            CameraHealthStatus::HealthyNoReports
        } else if no_expected_ms > 0 {
            summary.no_expected_source_cameras += 1;
            CameraHealthStatus::NoExpectedSource
        } else {
            summary.unknown_history_cameras += 1;
            CameraHealthStatus::UnknownHistory
        };
        summary.cameras.push(ActivityCameraSourceHealth {
            camera_id: camera_id.clone(),
            display_name,
            status,
            reported_events,
            missing_health_ms: missing_ms,
            healthy_observed_ms: healthy_ms,
        });
    }
    summary
}

pub(super) const fn classify_completeness(
    partial_current: bool,
    retention_partial: bool,
    source_health_gap: bool,
    event_count: u64,
    has_health_evidence: bool,
) -> CompletenessState {
    if retention_partial {
        CompletenessState::PartialRetention
    } else if source_health_gap {
        CompletenessState::SourceGap
    } else if partial_current {
        CompletenessState::PartialCurrent
    } else if event_count == 0 && !has_health_evidence {
        CompletenessState::Unknown
    } else {
        CompletenessState::Complete
    }
}

fn bucket_health_flags(
    start_ms: i64,
    end_ms: i64,
    intervals: &[HealthInterval],
    effective_cameras: &[String],
) -> (bool, bool) {
    if effective_cameras.is_empty() {
        return (false, true);
    }
    let camera_set: BTreeSet<&str> = effective_cameras.iter().map(String::as_str).collect();
    let mut has_gap = false;
    let mut has_evidence = false;
    for interval in intervals {
        if !camera_set.contains(interval.camera_id.as_str()) {
            continue;
        }
        let interval_end = interval.end_time_ms.unwrap_or(i64::MAX);
        if interval.start_time_ms < end_ms && interval_end > start_ms {
            has_evidence = true;
            if interval.state == ExpectedSourceState::MissingHealth {
                has_gap = true;
            }
        }
    }
    (has_gap, has_evidence)
}

fn camera_health_durations(
    camera_id: &str,
    start_ms: i64,
    end_ms: i64,
    intervals: &[HealthInterval],
) -> (u64, u64, u64) {
    let mut healthy_ms = 0u64;
    let mut missing_ms = 0u64;
    let mut no_expected_ms = 0u64;
    for interval in intervals.iter().filter(|item| item.camera_id == camera_id) {
        let interval_end = interval.end_time_ms.unwrap_or(end_ms);
        let overlap_start = interval.start_time_ms.max(start_ms);
        let overlap_end = interval_end.min(end_ms);
        if overlap_end <= overlap_start {
            continue;
        }
        let dur = u64::try_from(overlap_end.saturating_sub(overlap_start)).unwrap_or(u64::MAX);
        match interval.state {
            ExpectedSourceState::Healthy => healthy_ms = healthy_ms.saturating_add(dur),
            ExpectedSourceState::MissingHealth => missing_ms = missing_ms.saturating_add(dur),
            ExpectedSourceState::NoExpectedSource => {
                no_expected_ms = no_expected_ms.saturating_add(dur);
            }
        }
    }
    (healthy_ms, missing_ms, no_expected_ms)
}

fn locate_bucket(
    granularity: BucketGranularity,
    tz: Tz,
    buckets: &[BucketSpec],
    timestamp_ms: i64,
) -> Result<Option<usize>> {
    match granularity {
        BucketGranularity::HourOfDay => {
            let (_, hour) = local_weekday_and_hour(timestamp_ms, tz)?;
            Ok(Some(usize::from(hour)))
        }
        BucketGranularity::WeekdayWeekend => {
            let (weekday, _) = local_weekday_and_hour(timestamp_ms, tz)?;
            Ok(Some(usize::from(weekday >= 6)))
        }
        _ => {
            let idx = buckets.partition_point(|bucket| bucket.end_time_ms <= timestamp_ms);
            Ok((idx < buckets.len()
                && timestamp_ms >= buckets[idx].start_time_ms
                && timestamp_ms < buckets[idx].end_time_ms)
                .then_some(idx))
        }
    }
}

fn camera_groups(query: &ActivitySummaryQuery, camera_id: &str) -> Vec<String> {
    let mut groups = Vec::new();
    for (group_id, group) in &query.authorized_groups {
        if group.camera_ids.contains(camera_id) {
            groups.push(group_id.clone());
        }
    }
    groups
}

#[allow(clippy::too_many_arguments)]
fn build_series_keys(
    query: &ActivitySummaryQuery,
    camera_id: &str,
    origin: EventSource,
    event_type: &str,
    zone_key: &str,
    confidence_band: ConfidenceBand,
    has_image: bool,
    workflow_label: Option<&str>,
) -> Vec<BTreeMap<ActivityDimension, String>> {
    let mut base = BTreeMap::new();
    let mut include_group = false;
    for dim in &query.group_by {
        match dim {
            ActivityDimension::Camera => {
                base.insert(*dim, camera_id.to_owned());
            }
            ActivityDimension::Group => {
                include_group = true;
            }
            ActivityDimension::EventType => {
                base.insert(*dim, event_type.to_owned());
            }
            ActivityDimension::Origin => {
                base.insert(*dim, origin.as_str().to_owned());
            }
            ActivityDimension::Zone => {
                let label = if zone_key.is_empty() {
                    "unknown".to_owned()
                } else {
                    zone_key.to_owned()
                };
                base.insert(*dim, label);
            }
            ActivityDimension::ConfidenceBand => {
                base.insert(*dim, confidence_band.as_str().to_owned());
            }
            ActivityDimension::Image => {
                base.insert(
                    *dim,
                    if has_image {
                        "with_image"
                    } else {
                        "without_image"
                    }
                    .to_owned(),
                );
            }
            ActivityDimension::Workflow => {
                base.insert(*dim, workflow_label.unwrap_or("unreviewed").to_owned());
            }
        }
    }
    if !include_group {
        return vec![base];
    }
    let groups = camera_groups(query, camera_id);
    if groups.is_empty() {
        base.insert(ActivityDimension::Group, "ungrouped".to_owned());
        return vec![base];
    }
    groups
        .into_iter()
        .map(|group_id| {
            let mut map = base.clone();
            map.insert(ActivityDimension::Group, group_id);
            map
        })
        .collect()
}

const fn replace_duration(measures: &mut ActivityMeasures, stored: u64, clipped: u64) {
    measures.total_duration_ms = measures
        .total_duration_ms
        .saturating_sub(stored)
        .saturating_add(clipped);
}
