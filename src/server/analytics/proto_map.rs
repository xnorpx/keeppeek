//! Protobuf translation helpers for activity analytics RPC messages.

use super::super::{ControlCommandError, millis_timestamp, timestamp_ms};
use crate::{
    api::proto,
    storage::{
        analytics::{
            ActivityAnalyticsStatus, ActivityBreakdownRow, ActivityCameraSourceHealth,
            ActivityComparisonSummary, ActivityDimension, ActivityHeatmapCell, ActivityMeasures,
            ActivityRetentionEvidence, ActivitySeries, ActivitySourceHealthSummary,
            ActivitySummaryFilter, ActivitySummaryResult, ActivityTimeBucket, BucketGranularity,
            CameraHealthStatus, ComparisonMode, CompletenessState, ConfidenceBand, RollupState,
            WeekStart,
        },
        metadata::EventSource,
        search::EventImageFilter,
    },
};

pub(super) fn parse_granularity(raw: i32) -> Result<BucketGranularity, ControlCommandError> {
    match proto::ActivityBucketGranularity::try_from(raw) {
        Ok(proto::ActivityBucketGranularity::Hour) => Ok(BucketGranularity::Hour),
        Ok(proto::ActivityBucketGranularity::Day) => Ok(BucketGranularity::Day),
        Ok(proto::ActivityBucketGranularity::Week) => Ok(BucketGranularity::Week),
        Ok(proto::ActivityBucketGranularity::Month) => Ok(BucketGranularity::Month),
        Ok(proto::ActivityBucketGranularity::HourOfDay) => Ok(BucketGranularity::HourOfDay),
        Ok(proto::ActivityBucketGranularity::WeekdayWeekend) => {
            Ok(BucketGranularity::WeekdayWeekend)
        }
        _ => Err(invalid("activity bucket granularity is invalid")),
    }
}

pub(super) fn parse_week_start(
    raw: i32,
    default: WeekStart,
) -> Result<WeekStart, ControlCommandError> {
    match proto::ActivityWeekStart::try_from(raw) {
        Ok(proto::ActivityWeekStart::Unspecified) => Ok(default),
        Ok(proto::ActivityWeekStart::Monday) => Ok(WeekStart::Monday),
        Ok(proto::ActivityWeekStart::Sunday) => Ok(WeekStart::Sunday),
        Err(_) => Err(invalid("activity week start is invalid")),
    }
}

pub(super) fn parse_comparison(raw: i32) -> Result<ComparisonMode, ControlCommandError> {
    match proto::ActivityComparisonMode::try_from(raw) {
        Ok(proto::ActivityComparisonMode::Unspecified | proto::ActivityComparisonMode::None) => {
            Ok(ComparisonMode::None)
        }
        Ok(proto::ActivityComparisonMode::PreviousDuration) => Ok(ComparisonMode::PreviousDuration),
        Ok(proto::ActivityComparisonMode::PreviousCalendar) => Ok(ComparisonMode::PreviousCalendar),
        Err(_) => Err(invalid("activity comparison mode is invalid")),
    }
}

pub(super) fn parse_dimension(raw: i32) -> Result<ActivityDimension, ControlCommandError> {
    match proto::ActivityDimension::try_from(raw) {
        Ok(proto::ActivityDimension::Camera) => Ok(ActivityDimension::Camera),
        Ok(proto::ActivityDimension::Group) => Ok(ActivityDimension::Group),
        Ok(proto::ActivityDimension::EventType) => Ok(ActivityDimension::EventType),
        Ok(proto::ActivityDimension::Origin) => Ok(ActivityDimension::Origin),
        Ok(proto::ActivityDimension::Zone) => Ok(ActivityDimension::Zone),
        Ok(proto::ActivityDimension::ConfidenceBand) => Ok(ActivityDimension::ConfidenceBand),
        Ok(proto::ActivityDimension::Image) => Ok(ActivityDimension::Image),
        Ok(proto::ActivityDimension::Workflow) => Ok(ActivityDimension::Workflow),
        _ => Err(invalid("activity dimension is invalid")),
    }
}

pub fn parse_confidence_band(raw: i32) -> Result<ConfidenceBand, ControlCommandError> {
    match proto::EventConfidenceBand::try_from(raw) {
        Ok(proto::EventConfidenceBand::Unknown) => Ok(ConfidenceBand::Unknown),
        Ok(proto::EventConfidenceBand::Low) => Ok(ConfidenceBand::Low),
        Ok(proto::EventConfidenceBand::Medium) => Ok(ConfidenceBand::Medium),
        Ok(proto::EventConfidenceBand::High) => Ok(ConfidenceBand::High),
        _ => Err(invalid("event confidence band is invalid")),
    }
}

pub(super) fn parse_filter(
    raw: Option<proto::ActivitySummaryFilter>,
    principal: &super::super::ApiPrincipal,
) -> Result<ActivitySummaryFilter, ControlCommandError> {
    let Some(raw) = raw else {
        return Ok(ActivitySummaryFilter::default());
    };
    let origins = raw
        .origins
        .into_iter()
        .map(|origin| match proto::EventOrigin::try_from(origin) {
            Ok(proto::EventOrigin::Camera) => Ok(EventSource::Camera),
            Ok(proto::EventOrigin::Keeppeek) => Ok(EventSource::KeepPeek),
            _ => Err(invalid("activity event origin is invalid")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let confidence_bands = raw
        .confidence_bands
        .into_iter()
        .map(parse_confidence_band)
        .collect::<Result<Vec<_>, _>>()?;
    let image = match proto::EventImageFilter::try_from(raw.image) {
        Ok(proto::EventImageFilter::Any) => EventImageFilter::Any,
        Ok(proto::EventImageFilter::WithImage) => EventImageFilter::WithImage,
        Ok(proto::EventImageFilter::WithoutImage) => EventImageFilter::WithoutImage,
        Err(_) => return Err(invalid("activity image filter is invalid")),
    };
    let workflow = raw
        .workflow
        .as_ref()
        .map(|ctx| super::super::event_workflow::query_filter(principal, ctx))
        .transpose()?;
    Ok(ActivitySummaryFilter {
        source_ids: raw.source_ids,
        group_ids: raw.group_ids,
        event_types: raw.event_types,
        origins,
        zones: raw.zones,
        include_missing_zone: raw.include_missing_zone,
        confidence_bands,
        image,
        workflow,
    })
}

pub(super) fn parse_bounds(
    start: Option<prost_types::Timestamp>,
    end: Option<prost_types::Timestamp>,
) -> Result<(i64, i64), ControlCommandError> {
    let start_ms = start
        .as_ref()
        .and_then(timestamp_ms)
        .ok_or_else(|| invalid("activity summary start_time is required and must be valid"))?;
    let end_ms = end
        .as_ref()
        .and_then(timestamp_ms)
        .ok_or_else(|| invalid("activity summary end_time is required and must be valid"))?;
    Ok((start_ms, end_ms))
}

pub(super) fn proto_status(status: ActivityAnalyticsStatus) -> proto::ActivityAnalyticsStatus {
    proto::ActivityAnalyticsStatus {
        state: proto_rollup_state(status.state) as i32,
        analytics_generation: status.analytics_generation,
        schema_version: status.schema_version,
        site_timezone: status.site_timezone,
        week_start: proto_week_start(status.week_start) as i32,
        tzdb_version: status.tzdb_version,
        rollup_row_count: status.rollup_row_count,
        max_rollup_rows: status.max_rollup_rows,
        oldest_retained_event_time: status.oldest_retained_event_ms.map(millis_timestamp),
        last_rebuild_completed_at: status.last_rebuild_completed_at_ms.map(millis_timestamp),
        status_detail: status.status_detail,
    }
}

pub(super) fn proto_summary(
    query_id: String,
    result: ActivitySummaryResult,
) -> proto::ActivitySummaryResponse {
    proto::ActivitySummaryResponse {
        query_id,
        generated_at: Some(millis_timestamp(result.generated_at_ms)),
        start_time: Some(millis_timestamp(result.start_time_ms)),
        end_time: Some(millis_timestamp(result.end_time_ms)),
        timezone: result.timezone,
        tzdb_version: result.tzdb_version,
        granularity: proto_granularity(result.granularity) as i32,
        week_start: proto_week_start(result.week_start) as i32,
        analytics_generation: result.analytics_generation,
        group_policy_revision: result.group_policy_revision,
        data_source: result.data_source.as_str().to_owned(),
        rollup_state: proto_rollup_state(result.rollup_state) as i32,
        totals: Some(proto_measures(result.totals)),
        comparison: result.comparison.map(proto_comparison),
        buckets: result.buckets.into_iter().map(proto_bucket).collect(),
        series: result.series.into_iter().map(proto_series).collect(),
        series_truncated: result.series_truncated,
        heatmap: result.heatmap.into_iter().map(proto_heatmap_cell).collect(),
        by_camera: result.by_camera.into_iter().map(proto_breakdown).collect(),
        by_event_type: result
            .by_event_type
            .into_iter()
            .map(proto_breakdown)
            .collect(),
        by_zone: result.by_zone.into_iter().map(proto_breakdown).collect(),
        by_origin: result.by_origin.into_iter().map(proto_breakdown).collect(),
        by_confidence_band: result
            .by_confidence_band
            .into_iter()
            .map(proto_breakdown)
            .collect(),
        by_group: result.by_group.into_iter().map(proto_breakdown).collect(),
        retention: Some(proto_retention(result.retention)),
        source_health: Some(proto_source_health(result.source_health)),
        provenance_notice: result.provenance_notice,
        exact: result.exact,
    }
}

const fn proto_measures(measures: ActivityMeasures) -> proto::ActivityMeasures {
    proto::ActivityMeasures {
        event_count: measures.event_count,
        distinct_event_count: measures.distinct_event_count,
        interval_event_count: measures.interval_event_count,
        open_interval_event_count: measures.open_interval_event_count,
        total_duration_ms: measures.total_duration_ms,
        average_duration_ms: measures.average_duration_ms,
        with_image_count: measures.with_image_count,
        with_confidence_count: measures.with_confidence_count,
        with_zone_count: measures.with_zone_count,
        percent_with_image: measures.percent_with_image,
        percent_with_confidence: measures.percent_with_confidence,
        percent_with_zone: measures.percent_with_zone,
    }
}

fn proto_bucket(bucket: ActivityTimeBucket) -> proto::ActivityTimeBucket {
    proto::ActivityTimeBucket {
        start_time: Some(millis_timestamp(bucket.start_time_ms)),
        end_time: Some(millis_timestamp(bucket.end_time_ms)),
        label: bucket.label,
        utc_offset_seconds: bucket.utc_offset_seconds,
        dst_fold: bucket.dst_fold,
        partial_current: bucket.partial_current,
        completeness: proto_completeness(bucket.completeness) as i32,
        retention_partial: bucket.retention_partial,
        source_health_gap: bucket.source_health_gap,
        measures: Some(proto_measures(bucket.measures)),
    }
}

fn proto_series(series: ActivitySeries) -> proto::ActivitySeries {
    proto::ActivitySeries {
        dimensions: series
            .dimension_values
            .into_iter()
            .map(|(dim, value)| proto::ActivityDimensionEntry {
                dimension: proto_dimension(dim) as i32,
                value,
            })
            .collect(),
        label: series.label,
        totals: Some(proto_measures(series.totals)),
        bucket_counts: series.bucket_counts,
    }
}

fn proto_heatmap_cell(cell: ActivityHeatmapCell) -> proto::ActivityHeatmapCell {
    proto::ActivityHeatmapCell {
        weekday: u32::from(cell.weekday),
        local_hour: u32::from(cell.local_hour),
        event_count: cell.event_count,
        distinct_event_count: cell.distinct_event_count,
        completeness: proto_completeness(cell.completeness) as i32,
    }
}

fn proto_breakdown(row: ActivityBreakdownRow) -> proto::ActivityBreakdownRow {
    proto::ActivityBreakdownRow {
        key: row.key,
        label: row.label,
        is_unknown: row.is_unknown,
        measures: Some(proto_measures(row.measures)),
        share_percent: row.share_percent,
    }
}

fn proto_comparison(comp: ActivityComparisonSummary) -> proto::ActivityComparisonSummary {
    proto::ActivityComparisonSummary {
        mode: proto_comparison_mode(comp.mode) as i32,
        start_time: Some(millis_timestamp(comp.start_time_ms)),
        end_time: Some(millis_timestamp(comp.end_time_ms)),
        totals: Some(proto_measures(comp.totals)),
        event_count_delta: comp.event_count_delta,
        event_count_delta_percent: comp.event_count_delta_percent,
        completeness: proto_completeness(comp.completeness) as i32,
        retention_partial: comp.retention_partial,
        source_health_gap: comp.source_health_gap,
    }
}

fn proto_retention(retention: ActivityRetentionEvidence) -> proto::ActivityRetentionEvidence {
    proto::ActivityRetentionEvidence {
        partial_retention: retention.partial_retention,
        oldest_retained_event_time: retention.oldest_retained_event_ms.map(millis_timestamp),
        deleted_event_count_in_range: retention.deleted_event_count_in_range,
        deleted_intervals: retention
            .deleted_intervals
            .into_iter()
            .map(|(start, end)| proto::ActivityTimeInterval {
                start_time: Some(millis_timestamp(start)),
                end_time: Some(millis_timestamp(end)),
            })
            .collect(),
    }
}

fn proto_source_health(health: ActivitySourceHealthSummary) -> proto::ActivitySourceHealthSummary {
    proto::ActivitySourceHealthSummary {
        authorized_cameras: health.authorized_cameras,
        reporting_cameras: health.reporting_cameras,
        healthy_no_reports_cameras: health.healthy_no_reports_cameras,
        missing_health_cameras: health.missing_health_cameras,
        no_expected_source_cameras: health.no_expected_source_cameras,
        unknown_history_cameras: health.unknown_history_cameras,
        cameras: health
            .cameras
            .into_iter()
            .map(proto_camera_health)
            .collect(),
    }
}

fn proto_camera_health(camera: ActivityCameraSourceHealth) -> proto::ActivityCameraSourceHealth {
    proto::ActivityCameraSourceHealth {
        camera_id: camera.camera_id,
        display_name: camera.display_name,
        status: proto_camera_health_status(camera.status) as i32,
        reported_events: camera.reported_events,
        missing_health_ms: camera.missing_health_ms,
        healthy_observed_ms: camera.healthy_observed_ms,
    }
}

const fn proto_granularity(value: BucketGranularity) -> proto::ActivityBucketGranularity {
    match value {
        BucketGranularity::Hour => proto::ActivityBucketGranularity::Hour,
        BucketGranularity::Day => proto::ActivityBucketGranularity::Day,
        BucketGranularity::Week => proto::ActivityBucketGranularity::Week,
        BucketGranularity::Month => proto::ActivityBucketGranularity::Month,
        BucketGranularity::HourOfDay => proto::ActivityBucketGranularity::HourOfDay,
        BucketGranularity::WeekdayWeekend => proto::ActivityBucketGranularity::WeekdayWeekend,
    }
}

const fn proto_week_start(value: WeekStart) -> proto::ActivityWeekStart {
    match value {
        WeekStart::Monday => proto::ActivityWeekStart::Monday,
        WeekStart::Sunday => proto::ActivityWeekStart::Sunday,
    }
}

const fn proto_dimension(value: ActivityDimension) -> proto::ActivityDimension {
    match value {
        ActivityDimension::Camera => proto::ActivityDimension::Camera,
        ActivityDimension::Group => proto::ActivityDimension::Group,
        ActivityDimension::EventType => proto::ActivityDimension::EventType,
        ActivityDimension::Origin => proto::ActivityDimension::Origin,
        ActivityDimension::Zone => proto::ActivityDimension::Zone,
        ActivityDimension::ConfidenceBand => proto::ActivityDimension::ConfidenceBand,
        ActivityDimension::Image => proto::ActivityDimension::Image,
        ActivityDimension::Workflow => proto::ActivityDimension::Workflow,
    }
}

const fn proto_comparison_mode(value: ComparisonMode) -> proto::ActivityComparisonMode {
    match value {
        ComparisonMode::None => proto::ActivityComparisonMode::None,
        ComparisonMode::PreviousDuration => proto::ActivityComparisonMode::PreviousDuration,
        ComparisonMode::PreviousCalendar => proto::ActivityComparisonMode::PreviousCalendar,
    }
}

const fn proto_rollup_state(value: RollupState) -> proto::ActivityRollupState {
    match value {
        RollupState::Ready => proto::ActivityRollupState::Ready,
        RollupState::Rebuilding => proto::ActivityRollupState::Rebuilding,
        RollupState::Degraded => proto::ActivityRollupState::Degraded,
        RollupState::Unavailable => proto::ActivityRollupState::Unavailable,
    }
}

const fn proto_completeness(value: CompletenessState) -> proto::ActivityCompletenessState {
    match value {
        CompletenessState::Complete => proto::ActivityCompletenessState::Complete,
        CompletenessState::PartialCurrent => proto::ActivityCompletenessState::PartialCurrent,
        CompletenessState::PartialRetention => proto::ActivityCompletenessState::PartialRetention,
        CompletenessState::SourceGap => proto::ActivityCompletenessState::SourceGap,
        CompletenessState::Unknown => proto::ActivityCompletenessState::Unknown,
    }
}

const fn proto_camera_health_status(
    value: CameraHealthStatus,
) -> proto::ActivityCameraHealthStatus {
    match value {
        CameraHealthStatus::Reporting => proto::ActivityCameraHealthStatus::Reporting,
        CameraHealthStatus::HealthyNoReports => proto::ActivityCameraHealthStatus::HealthyNoReports,
        CameraHealthStatus::MissingHealth => proto::ActivityCameraHealthStatus::MissingHealth,
        CameraHealthStatus::NoExpectedSource => proto::ActivityCameraHealthStatus::NoExpectedSource,
        CameraHealthStatus::UnknownHistory => proto::ActivityCameraHealthStatus::UnknownHistory,
    }
}

fn invalid(message: impl Into<String>) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}
