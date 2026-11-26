//! Bounded activity summaries and temporal analytics over canonical stored events.

use crate::storage::{
    catalog::workflow,
    metadata::{EventSource, TimelineEvent},
    search::{EventImageFilter, normalize_search_text},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub mod time;

/// Current derived analytics schema version.
pub const ANALYTICS_SCHEMA_VERSION: u32 = 1;
/// Maximum time buckets returned by one summary query.
pub const MAX_BUCKETS: usize = 366;
/// Maximum grouped series returned by one summary query.
pub const MAX_SERIES: usize = 64;
/// Maximum ranked breakdown rows returned per dimension table.
pub const MAX_RANKED_ROWS: usize = 64;
/// Maximum authorized cameras evaluated in one summary query.
pub const MAX_CAMERAS: usize = 127;
/// Maximum grouped dimensions selected from the fixed allowlist.
pub const MAX_GROUP_BY_DIMENSIONS: usize = 8;
/// Maximum filter values per list predicate.
pub const MAX_FILTER_VALUES: usize = 64;
/// Maximum UTC query range span (10 years).
pub const MAX_RANGE_MS: i64 = 10 * 366 * 86_400_000;
/// Short-range threshold that may scan canonical events directly.
pub const MAX_EXACT_FALLBACK_RANGE_MS: i64 = 7 * 86_400_000;
/// Maximum serialized response budget in bytes (1 MiB).
pub const MAX_RESPONSE_BYTES: usize = 1_048_576;
/// Maximum exported rows across aggregate CSV or JSON exports.
pub const MAX_EXPORT_ROWS: usize = 25_000;
/// Default derived rollup row quota before analytics enters degraded state.
pub const DEFAULT_MAX_ROLLUP_ROWS: u64 = 500_000;
/// Minimum configurable rollup row quota.
pub const MIN_MAX_ROLLUP_ROWS: u64 = 1_000;
/// Maximum configurable rollup row quota.
pub const MAX_CONFIGURABLE_ROLLUP_ROWS: u64 = 5_000_000;
/// Maximum canonical events scanned per resumable rebuild batch.
pub const MAX_REBUILD_BATCH_ROWS: usize = 512;
/// Maximum pending dirty event IDs queued during an active shadow rebuild.
pub const MAX_REBUILD_DIRTY_ROWS: u64 = 65_536;
/// Maximum canonical events scanned for exact workflow-filtered queries.
pub const MAX_WORKFLOW_SCAN_EVENTS: u64 = 250_000;
/// Maximum deletion evidence rows retained in the analytics deletion ledger.
pub const MAX_DELETION_EVIDENCE_ROWS: i64 = 10_000;
/// Maximum source-health interval rows retained for analytics completeness.
pub const MAX_SOURCE_HEALTH_ROWS: i64 = 50_000;
/// Maximum string length for validated filter tokens.
pub const MAX_FILTER_TOKEN_BYTES: usize = 256;
/// Explicit provenance label returned with every activity summary.
pub const PROVENANCE_NOTICE: &str = "Counts reflect producer-reported event metadata within current retention and authorization scope, not authoritative real-world identity, occupancy, or threat totals.";

/// Fixed confidence bands used by analytics rollups and exact Events drill-down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceBand {
    Unknown,
    Low,
    Medium,
    High,
}

impl ConfidenceBand {
    /// Classifies an optional producer confidence score into a fixed band.
    ///
    /// Non-finite or out-of-range values map to `Unknown`.
    pub fn from_option(value: Option<f64>) -> Self {
        match value {
            Some(score) if score.is_finite() && (0.0..=1.0).contains(&score) => {
                if score < 0.5 {
                    Self::Low
                } else if score < 0.8 {
                    Self::Medium
                } else {
                    Self::High
                }
            }
            _ => Self::Unknown,
        }
    }

    pub const fn as_i64(self) -> i64 {
        match self {
            Self::Unknown => 1,
            Self::Low => 2,
            Self::Medium => 3,
            Self::High => 4,
        }
    }

    pub const fn from_i64(value: i64) -> Option<Self> {
        match value {
            1 => Some(Self::Unknown),
            2 => Some(Self::Low),
            3 => Some(Self::Medium),
            4 => Some(Self::High),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown confidence",
            Self::Low => "Low (0.00-0.49)",
            Self::Medium => "Medium (0.50-0.79)",
            Self::High => "High (0.80-1.00)",
        }
    }
}

/// Time bucket granularity for activity summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketGranularity {
    Hour,
    Day,
    Week,
    Month,
    HourOfDay,
    WeekdayWeekend,
}

impl BucketGranularity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::HourOfDay => "hour_of_day",
            Self::WeekdayWeekend => "weekday_weekend",
        }
    }
}

/// First day of the calendar week for weekly buckets and comparisons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeekStart {
    #[default]
    Monday,
    Sunday,
}

impl WeekStart {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Monday => "monday",
            Self::Sunday => "sunday",
        }
    }
}

/// Fixed allowlist of dimensions supported for grouped summary series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityDimension {
    Camera,
    Group,
    EventType,
    Origin,
    Zone,
    ConfidenceBand,
    Image,
    Workflow,
}

impl ActivityDimension {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::Group => "group",
            Self::EventType => "event_type",
            Self::Origin => "origin",
            Self::Zone => "zone",
            Self::ConfidenceBand => "confidence_band",
            Self::Image => "image",
            Self::Workflow => "workflow",
        }
    }
}

/// Previous-interval comparison mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonMode {
    #[default]
    None,
    PreviousDuration,
    PreviousCalendar,
}

/// Lifecycle state of derived analytics rollups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollupState {
    Ready,
    Rebuilding,
    Degraded,
    Unavailable,
}

impl RollupState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Rebuilding => "rebuilding",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ready" => Some(Self::Ready),
            "rebuilding" => Some(Self::Rebuilding),
            "degraded" => Some(Self::Degraded),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Completeness classification for one time bucket or summary window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletenessState {
    Complete,
    PartialCurrent,
    PartialRetention,
    SourceGap,
    Unknown,
}

impl CompletenessState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::PartialCurrent => "partial_current",
            Self::PartialRetention => "partial_retention",
            Self::SourceGap => "source_gap",
            Self::Unknown => "unknown",
        }
    }
}

/// Historical event-source health state recorded from real health transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedSourceState {
    NoExpectedSource,
    Healthy,
    MissingHealth,
}

impl ExpectedSourceState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoExpectedSource => "no_expected_source",
            Self::Healthy => "healthy",
            Self::MissingHealth => "missing_health",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "no_expected_source" => Some(Self::NoExpectedSource),
            "healthy" => Some(Self::Healthy),
            "missing_health" => Some(Self::MissingHealth),
            _ => None,
        }
    }
}

/// Per-camera reporting and expected-source health classification for one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraHealthStatus {
    Reporting,
    HealthyNoReports,
    MissingHealth,
    NoExpectedSource,
    UnknownHistory,
}

/// Data source selected by the analytics query planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryDataSource {
    Rollups,
    CanonicalExact,
    HybridRollupsAndExactEdges,
}

impl QueryDataSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rollups => "rollups",
            Self::CanonicalExact => "canonical_exact",
            Self::HybridRollupsAndExactEdges => "hybrid_rollups_and_exact_edges",
        }
    }
}

/// Authorized camera display metadata supplied by the server before aggregation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedCamera {
    pub camera_id: String,
    pub display_name: String,
    pub group_ids: Vec<String>,
}

/// Authorized camera group display metadata supplied by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedGroup {
    pub group_id: String,
    pub display_name: String,
    pub camera_ids: BTreeSet<String>,
}

/// Shared filter predicates for activity summaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivitySummaryFilter {
    pub source_ids: Vec<String>,
    pub group_ids: Vec<String>,
    pub event_types: Vec<String>,
    pub origins: Vec<EventSource>,
    pub zones: Vec<String>,
    pub include_missing_zone: bool,
    pub confidence_bands: Vec<ConfidenceBand>,
    pub image: EventImageFilter,
    pub workflow: Option<workflow::Query>,
}

/// Validated input for one bounded activity summary query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySummaryQuery {
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub timezone: String,
    pub granularity: BucketGranularity,
    pub week_start: WeekStart,
    pub filter: ActivitySummaryFilter,
    pub group_by: Vec<ActivityDimension>,
    pub comparison: ComparisonMode,
    pub dense_buckets: bool,
    pub max_buckets: usize,
    pub max_series: usize,
    pub max_ranked_rows: usize,
    pub include_heatmap: bool,
    pub now_ms: i64,
    pub authorized_cameras: BTreeMap<String, AuthorizedCamera>,
    pub authorized_groups: BTreeMap<String, AuthorizedGroup>,
    pub group_policy_revision: u64,
}

impl ActivitySummaryQuery {
    /// Creates a query with default bounds over the supplied authorized camera scope.
    pub fn new(
        start_time_ms: i64,
        end_time_ms: i64,
        timezone: impl Into<String>,
        granularity: BucketGranularity,
        now_ms: i64,
        authorized_cameras: BTreeMap<String, AuthorizedCamera>,
    ) -> Self {
        Self {
            start_time_ms,
            end_time_ms,
            timezone: timezone.into(),
            granularity,
            week_start: WeekStart::Monday,
            filter: ActivitySummaryFilter::default(),
            group_by: Vec::new(),
            comparison: ComparisonMode::None,
            dense_buckets: true,
            max_buckets: MAX_BUCKETS,
            max_series: MAX_SERIES,
            max_ranked_rows: MAX_RANKED_ROWS,
            include_heatmap: false,
            now_ms,
            authorized_cameras,
            authorized_groups: BTreeMap::new(),
            group_policy_revision: 0,
        }
    }

    /// Validates and normalizes all bounds, timezone, filters, and dimensions.
    pub fn validate(&mut self) -> anyhow::Result<chrono_tz::Tz> {
        anyhow::ensure!(
            self.start_time_ms < self.end_time_ms,
            "activity summary start time must precede end time"
        );
        let span = self
            .end_time_ms
            .checked_sub(self.start_time_ms)
            .ok_or_else(|| anyhow::anyhow!("activity summary time range overflowed"))?;
        anyhow::ensure!(
            span <= MAX_RANGE_MS,
            "activity summary time range exceeds 10 years"
        );
        anyhow::ensure!(
            self.authorized_cameras.len() <= MAX_CAMERAS,
            "authorized camera count exceeds {MAX_CAMERAS}"
        );
        let tz = time::parse_timezone(&self.timezone)?;
        self.timezone = tz.name().to_owned();
        if self.max_buckets == 0 || self.max_buckets > MAX_BUCKETS {
            anyhow::bail!("activity summary max_buckets must be between 1 and {MAX_BUCKETS}");
        }
        if self.max_series == 0 || self.max_series > MAX_SERIES {
            anyhow::bail!("activity summary max_series must be between 1 and {MAX_SERIES}");
        }
        if self.max_ranked_rows == 0 || self.max_ranked_rows > MAX_RANKED_ROWS {
            anyhow::bail!(
                "activity summary max_ranked_rows must be between 1 and {MAX_RANKED_ROWS}"
            );
        }
        if self.group_by.len() > MAX_GROUP_BY_DIMENSIONS {
            anyhow::bail!(
                "activity summary group_by dimension count exceeds {MAX_GROUP_BY_DIMENSIONS}"
            );
        }
        let mut seen_dimensions = BTreeSet::new();
        self.group_by.retain(|dim| seen_dimensions.insert(*dim));
        normalize_filter(&mut self.filter)?;
        Ok(tz)
    }

    /// Resolves the exact authorized camera IDs permitted after source and group filters.
    pub fn effective_camera_ids(&self) -> Vec<String> {
        let mut allowed: BTreeSet<&str> =
            self.authorized_cameras.keys().map(String::as_str).collect();
        if !self.filter.source_ids.is_empty() {
            let requested: BTreeSet<&str> =
                self.filter.source_ids.iter().map(String::as_str).collect();
            allowed.retain(|camera_id| requested.contains(camera_id));
        }
        if !self.filter.group_ids.is_empty() {
            let mut in_groups = BTreeSet::new();
            for group_id in &self.filter.group_ids {
                if let Some(group) = self.authorized_groups.get(group_id) {
                    for camera_id in &group.camera_ids {
                        in_groups.insert(camera_id.as_str());
                    }
                }
            }
            allowed.retain(|camera_id| in_groups.contains(camera_id));
        }
        allowed.into_iter().map(str::to_owned).collect()
    }
}

fn normalize_filter(filter: &mut ActivitySummaryFilter) -> anyhow::Result<()> {
    normalize_id_list(&mut filter.source_ids, "camera", MAX_CAMERAS)?;
    normalize_id_list(&mut filter.group_ids, "group", MAX_FILTER_VALUES)?;
    normalize_text_list(&mut filter.event_types, "event type")?;
    normalize_text_list(&mut filter.zones, "zone")?;
    anyhow::ensure!(
        filter.origins.len() <= MAX_FILTER_VALUES,
        "origin filter count exceeds {MAX_FILTER_VALUES}"
    );
    filter
        .origins
        .sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
    filter.origins.dedup();
    anyhow::ensure!(
        filter.confidence_bands.len() <= 4,
        "confidence band filter count exceeds 4"
    );
    filter.confidence_bands.sort_unstable();
    filter.confidence_bands.dedup();
    Ok(())
}

fn normalize_id_list(values: &mut Vec<String>, label: &str, max_len: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        values.len() <= max_len,
        "{label} filter count exceeds {max_len}"
    );
    for value in &mut *values {
        *value = value.trim().to_owned();
        anyhow::ensure!(
            !value.is_empty() && value.len() <= MAX_FILTER_TOKEN_BYTES,
            "{label} filter value must contain 1 to {MAX_FILTER_TOKEN_BYTES} UTF-8 bytes"
        );
    }
    values.sort_unstable();
    values.dedup();
    Ok(())
}

fn normalize_text_list(values: &mut Vec<String>, label: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        values.len() <= MAX_FILTER_VALUES,
        "{label} filter count exceeds {MAX_FILTER_VALUES}"
    );
    for value in &mut *values {
        *value = normalize_search_text(value)?;
    }
    values.sort_unstable();
    values.dedup();
    Ok(())
}

/// Aggregated measures for one bucket, series, breakdown row, or total window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivityMeasures {
    pub event_count: u64,
    pub distinct_event_count: u64,
    pub interval_event_count: u64,
    pub open_interval_event_count: u64,
    pub total_duration_ms: u64,
    pub average_duration_ms: Option<f64>,
    pub with_image_count: u64,
    pub with_confidence_count: u64,
    pub with_zone_count: u64,
    pub percent_with_image: Option<f64>,
    pub percent_with_confidence: Option<f64>,
    pub percent_with_zone: Option<f64>,
}

impl ActivityMeasures {
    /// Adds one event contribution into these measures with checked arithmetic.
    pub fn add_contribution(
        &mut self,
        contribution: &EventContribution,
        clipped_duration_ms: u64,
    ) -> anyhow::Result<()> {
        self.event_count = self
            .event_count
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("event count overflowed"))?;
        self.distinct_event_count = self.event_count;
        if contribution.is_interval {
            self.interval_event_count = self
                .interval_event_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("interval event count overflowed"))?;
            self.total_duration_ms = self
                .total_duration_ms
                .checked_add(clipped_duration_ms)
                .ok_or_else(|| anyhow::anyhow!("total duration overflowed"))?;
        }
        if contribution.is_open_interval {
            self.open_interval_event_count = self
                .open_interval_event_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("open interval count overflowed"))?;
        }
        if contribution.has_image {
            self.with_image_count = self
                .with_image_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("image count overflowed"))?;
        }
        if contribution.confidence_band != ConfidenceBand::Unknown {
            self.with_confidence_count = self
                .with_confidence_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("confidence count overflowed"))?;
        }
        if !contribution.zone_key.is_empty() {
            self.with_zone_count = self
                .with_zone_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("zone count overflowed"))?;
        }
        Ok(())
    }

    /// Merges pre-aggregated counts from a rollup row into these measures.
    #[allow(clippy::too_many_arguments)]
    pub fn add_rollup_counts(
        &mut self,
        event_count: u64,
        interval_event_count: u64,
        open_interval_event_count: u64,
        total_duration_ms: u64,
        has_image: bool,
        has_confidence: bool,
        has_zone: bool,
    ) -> anyhow::Result<()> {
        self.event_count = self
            .event_count
            .checked_add(event_count)
            .ok_or_else(|| anyhow::anyhow!("event count overflowed"))?;
        self.distinct_event_count = self.event_count;
        self.interval_event_count = self
            .interval_event_count
            .checked_add(interval_event_count)
            .ok_or_else(|| anyhow::anyhow!("interval event count overflowed"))?;
        self.open_interval_event_count = self
            .open_interval_event_count
            .checked_add(open_interval_event_count)
            .ok_or_else(|| anyhow::anyhow!("open interval count overflowed"))?;
        self.total_duration_ms = self
            .total_duration_ms
            .checked_add(total_duration_ms)
            .ok_or_else(|| anyhow::anyhow!("total duration overflowed"))?;
        if has_image {
            self.with_image_count = self
                .with_image_count
                .checked_add(event_count)
                .ok_or_else(|| anyhow::anyhow!("image count overflowed"))?;
        }
        if has_confidence {
            self.with_confidence_count = self
                .with_confidence_count
                .checked_add(event_count)
                .ok_or_else(|| anyhow::anyhow!("confidence count overflowed"))?;
        }
        if has_zone {
            self.with_zone_count = self
                .with_zone_count
                .checked_add(event_count)
                .ok_or_else(|| anyhow::anyhow!("zone count overflowed"))?;
        }
        Ok(())
    }

    /// Computes derived average duration and metadata percentages.
    pub fn finalize(&mut self) {
        self.distinct_event_count = self.event_count;
        self.average_duration_ms = (self.interval_event_count > 0)
            .then(|| self.total_duration_ms as f64 / self.interval_event_count as f64);
        if self.event_count > 0 {
            let denom = self.event_count as f64;
            self.percent_with_image = Some((self.with_image_count as f64 * 100.0) / denom);
            self.percent_with_confidence =
                Some((self.with_confidence_count as f64 * 100.0) / denom);
            self.percent_with_zone = Some((self.with_zone_count as f64 * 100.0) / denom);
        } else {
            self.percent_with_image = None;
            self.percent_with_confidence = None;
            self.percent_with_zone = None;
        }
    }
}

/// One normalized current-revision event contribution stored in the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventContribution {
    pub event_id: String,
    pub revision: u64,
    pub hour_utc_ms: i64,
    pub day_utc_ms: i64,
    pub camera_id: String,
    pub origin: EventSource,
    pub event_type: String,
    pub zone_key: String,
    pub confidence_band: ConfidenceBand,
    pub has_image: bool,
    pub start_time_ms: i64,
    pub end_time_ms: Option<i64>,
    pub is_interval: bool,
    pub is_open_interval: bool,
    pub duration_ms: u64,
}

impl EventContribution {
    /// Derives the normalized contribution for one canonical event revision.
    pub fn from_event(event: &TimelineEvent) -> anyhow::Result<Self> {
        let hour_utc_ms = floor_utc_bucket(event.start_time_ms, 3_600_000)?;
        let day_utc_ms = floor_utc_bucket(event.start_time_ms, 86_400_000)?;
        let event_type = normalize_dimension_key(&event.kind);
        let zone_key = event
            .zone
            .as_deref()
            .map(normalize_dimension_key)
            .unwrap_or_default();
        let confidence_band = ConfidenceBand::from_option(event.confidence);
        let has_image =
            event.thumbnail_filename.is_some() || event.canonical_attachment_id.is_some();
        let (is_interval, is_open_interval, duration_ms) = match event.end_time_ms {
            None => (true, true, 0),
            Some(end) if end > event.start_time_ms => {
                let delta = end
                    .checked_sub(event.start_time_ms)
                    .ok_or_else(|| anyhow::anyhow!("event duration overflowed"))?;
                (true, false, u64::try_from(delta)?)
            }
            Some(end) if end == event.start_time_ms => (false, false, 0),
            Some(_) => anyhow::bail!("event end precedes start"),
        };
        Ok(Self {
            event_id: event.id.clone(),
            revision: event.revision,
            hour_utc_ms,
            day_utc_ms,
            camera_id: event.camera_id.clone(),
            origin: event.source,
            event_type,
            zone_key,
            confidence_band,
            has_image,
            start_time_ms: event.start_time_ms,
            end_time_ms: event.end_time_ms,
            is_interval,
            is_open_interval,
            duration_ms,
        })
    }

    /// Computes clipped interval overlap in milliseconds against `[window_start, window_end)`.
    pub fn clipped_duration_ms(
        &self,
        window_start_ms: i64,
        window_end_ms: i64,
        now_ms: i64,
    ) -> u64 {
        if !self.is_interval {
            return 0;
        }
        let raw_end = self
            .end_time_ms
            .unwrap_or_else(|| now_ms.max(self.start_time_ms));
        let overlap_start = self.start_time_ms.max(window_start_ms);
        let overlap_end = raw_end.min(window_end_ms);
        if overlap_end <= overlap_start {
            0
        } else {
            u64::try_from(overlap_end.saturating_sub(overlap_start)).unwrap_or(u64::MAX)
        }
    }
}

/// Floors a Unix millisecond timestamp to a positive UTC step boundary.
pub fn floor_utc_bucket(timestamp_ms: i64, step_ms: i64) -> anyhow::Result<i64> {
    anyhow::ensure!(step_ms > 0, "UTC bucket step must be positive");
    timestamp_ms
        .div_euclid(step_ms)
        .checked_mul(step_ms)
        .ok_or_else(|| anyhow::anyhow!("UTC bucket timestamp overflowed"))
}

/// Normalizes a producer string dimension key for rollup grouping.
pub fn normalize_dimension_key(raw: &str) -> String {
    let normalized = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if normalized.len() <= MAX_FILTER_TOKEN_BYTES {
        normalized
    } else {
        normalized
            .char_indices()
            .take_while(|(idx, ch)| idx + ch.len_utf8() <= MAX_FILTER_TOKEN_BYTES)
            .map(|(_, ch)| ch)
            .collect()
    }
}

/// One returned time bucket with UTC bounds, civil-time provenance, and completeness flags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityTimeBucket {
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub label: String,
    pub utc_offset_seconds: i32,
    pub dst_fold: u32,
    pub partial_current: bool,
    pub completeness: CompletenessState,
    pub retention_partial: bool,
    pub source_health_gap: bool,
    pub measures: ActivityMeasures,
}

/// One grouped series over the requested buckets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivitySeries {
    pub dimension_values: BTreeMap<ActivityDimension, String>,
    pub label: String,
    pub totals: ActivityMeasures,
    pub bucket_counts: Vec<u64>,
}

/// One cell in the bounded 7x24 local weekday-by-hour heatmap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityHeatmapCell {
    /// ISO weekday: 1 = Monday through 7 = Sunday.
    pub weekday: u8,
    /// Civil hour of day: 0 through 23.
    pub local_hour: u8,
    pub event_count: u64,
    pub distinct_event_count: u64,
    pub completeness: CompletenessState,
}

/// One ranked breakdown row for camera, group, event type, zone, origin, or confidence band.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityBreakdownRow {
    pub key: String,
    pub label: String,
    pub is_unknown: bool,
    pub measures: ActivityMeasures,
    pub share_percent: f64,
}

/// Previous-interval comparison totals and deltas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityComparisonSummary {
    pub mode: ComparisonMode,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub totals: ActivityMeasures,
    pub event_count_delta: i64,
    pub event_count_delta_percent: Option<f64>,
    pub completeness: CompletenessState,
    pub retention_partial: bool,
    pub source_health_gap: bool,
}

/// Retention and deletion completeness evidence for one queried window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityRetentionEvidence {
    pub partial_retention: bool,
    pub oldest_retained_event_ms: Option<i64>,
    pub deleted_event_count_in_range: u64,
    pub deleted_intervals: Vec<(i64, i64)>,
}

/// Per-camera reporting and expected-source health summary row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityCameraSourceHealth {
    pub camera_id: String,
    pub display_name: String,
    pub status: CameraHealthStatus,
    pub reported_events: u64,
    pub missing_health_ms: u64,
    pub healthy_observed_ms: u64,
}

/// Aggregate source-health evidence across authorized cameras in the queried scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivitySourceHealthSummary {
    pub authorized_cameras: u64,
    pub reporting_cameras: u64,
    pub healthy_no_reports_cameras: u64,
    pub missing_health_cameras: u64,
    pub no_expected_source_cameras: u64,
    pub unknown_history_cameras: u64,
    pub cameras: Vec<ActivityCameraSourceHealth>,
}

/// Operational status snapshot for analytics rollups and site time defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityAnalyticsStatus {
    pub state: RollupState,
    pub analytics_generation: u64,
    pub schema_version: u32,
    pub site_timezone: String,
    pub week_start: WeekStart,
    pub tzdb_version: String,
    pub rollup_row_count: u64,
    pub max_rollup_rows: u64,
    pub oldest_retained_event_ms: Option<i64>,
    pub last_rebuild_completed_at_ms: Option<i64>,
    pub status_detail: Option<String>,
}

/// Complete bounded summary response produced by the catalog query planner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivitySummaryResult {
    pub generated_at_ms: i64,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub timezone: String,
    pub tzdb_version: String,
    pub granularity: BucketGranularity,
    pub week_start: WeekStart,
    pub analytics_generation: u64,
    pub group_policy_revision: u64,
    pub data_source: QueryDataSource,
    pub rollup_state: RollupState,
    pub totals: ActivityMeasures,
    pub comparison: Option<ActivityComparisonSummary>,
    pub buckets: Vec<ActivityTimeBucket>,
    pub series: Vec<ActivitySeries>,
    pub series_truncated: bool,
    pub heatmap: Vec<ActivityHeatmapCell>,
    pub by_camera: Vec<ActivityBreakdownRow>,
    pub by_event_type: Vec<ActivityBreakdownRow>,
    pub by_zone: Vec<ActivityBreakdownRow>,
    pub by_origin: Vec<ActivityBreakdownRow>,
    pub by_confidence_band: Vec<ActivityBreakdownRow>,
    pub by_group: Vec<ActivityBreakdownRow>,
    pub retention: ActivityRetentionEvidence,
    pub source_health: ActivitySourceHealthSummary,
    pub provenance_notice: String,
    pub exact: bool,
}
