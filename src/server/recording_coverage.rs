use super::{
    CameraEntry, ServerState, normalized_video_stream_id, recording_freshness_threshold_ms,
    recording_mode_includes_stream, recording_stream_id, server_health, service_error,
    unix_time_ms,
};
use crate::{
    health::{CameraHealth, ServerHealthResponse, StreamHealth},
    operational_events::{OperationalEvent, OperationalEventKind},
    runtime::{FacadeSender, RouterMessage},
    storage::{
        RecordingStreamHealthSnapshot,
        catalog::{
            CatalogCoverageSnapshot, CatalogDeletionRange, CatalogDeletionReason,
            CatalogStreamCoverage,
        },
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rouille::{Request, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DAY_MS: u64 = 86_400_000;
const DEFAULT_WINDOW_MS: u64 = DAY_MS;
const MAX_WINDOW_MS: u64 = 31 * DAY_MS;
const DEFAULT_PAGE_SIZE: usize = 25;
const MAX_PAGE_SIZE: usize = 50;
const DEFAULT_MINIMUM_GAP_MS: u64 = 5_000;
const PAGE_TOKEN_TTL_MS: u64 = 15 * 60 * 1_000;
const MAX_PAGE_TOKEN_BYTES: usize = 4_096;
const MAX_SEARCH_CHARS: usize = 128;

#[derive(Debug, Serialize)]
pub struct RecordingCoverageResponse {
    pub generated_at_ms: u64,
    pub catalog_available: bool,
    pub catalog_revision: u64,
    pub catalog_updated_at_ms: Option<i64>,
    pub window: CoverageWindow,
    pub totals: RecordingCoverageTotals,
    pub storage: RecordingCoverageStorage,
    pub groups: Vec<String>,
    pub cameras: Vec<CameraRecordingCoverage>,
    pub findings: Vec<RecordingFinding>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CoverageWindow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub minimum_gap_ms: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct RecordingCoverageTotals {
    pub cameras: usize,
    pub healthy: usize,
    pub degraded: usize,
    pub paused_by_policy: usize,
    pub not_configured: usize,
    pub unknown: usize,
    pub recording_bytes: u64,
    pub estimated_bytes_per_day: u64,
}

#[derive(Debug, Serialize)]
pub struct RecordingCoverageStorage {
    pub pressure: String,
    pub recording_state: String,
    pub available_bytes: Option<u64>,
    pub effective_limit_bytes: Option<u64>,
    pub recording_bytes: u64,
    pub estimated_bytes_per_day: u64,
    pub projected_retention_days: Option<f64>,
    pub projection_assumption: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingCoverageState {
    Healthy,
    Degraded,
    PausedByPolicy,
    NotConfigured,
    Unknown,
}

impl RecordingCoverageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::PausedByPolicy => "paused_by_policy",
            Self::NotConfigured => "not_configured",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CameraRecordingCoverage {
    pub camera_id: String,
    pub camera_name: String,
    pub groups: Vec<String>,
    pub state: RecordingCoverageState,
    pub recording_requested: bool,
    pub policy: String,
    pub streams: Vec<StreamRecordingCoverage>,
    pub health_href: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriterState {
    Progressing,
    Stalled,
    Failed,
    Pending,
    PolicyDisabled,
    Unknown,
}

impl WriterState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Progressing => "progressing",
            Self::Stalled => "stalled",
            Self::Failed => "failed",
            Self::Pending => "pending",
            Self::PolicyDisabled => "policy_disabled",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StreamRecordingCoverage {
    pub stream_id: String,
    pub recording_stream_id: String,
    pub recording_requested: bool,
    pub writer_state: WriterState,
    pub last_frame_at_ms: Option<u64>,
    pub last_write_at_ms: Option<u64>,
    pub last_finalize_at_ms: Option<i64>,
    pub last_catalog_commit_at_ms: Option<i64>,
    pub oldest_retained_at_ms: Option<i64>,
    pub newest_retained_at_ms: Option<i64>,
    pub effective_retention_ms: Option<u64>,
    pub recording_bytes: u64,
    pub estimated_bytes_per_day: u64,
    pub selected_coverage_ms: u64,
    pub coverage_percent: f64,
    pub gap_count: u64,
    pub largest_gap_ms: u64,
    pub playable_fragments: u64,
    pub ranges: Vec<CoverageRange>,
    pub range_count: u64,
    pub bucket_ms: u64,
    pub buckets: Vec<CoverageBucket>,
    pub detail_truncated: bool,
    pub gaps: Vec<RecordingGap>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CoverageRange {
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CoverageBucket {
    pub start_ms: i64,
    pub end_ms: i64,
    pub coverage_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingGapCause {
    SourceSilence,
    TransportOutage,
    StaleFrames,
    DecodeFailure,
    WriterFailure,
    DiskPressure,
    RetentionDeletion,
    Migration,
    CatalogMismatch,
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct RecordingGap {
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub observed_end_ms: i64,
    pub duration_ms: u64,
    pub cause: RecordingGapCause,
    pub explanation: String,
    pub evidence_source: String,
    pub operational_event_id: Option<String>,
    pub before_href: Option<String>,
    pub after_href: Option<String>,
    pub health_href: String,
    pub logs_href: String,
}

#[derive(Debug, Serialize)]
pub struct RecordingFinding {
    pub severity: String,
    pub camera_id: String,
    pub camera_name: String,
    pub stream_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub started_at_ms: Option<i64>,
    pub health_href: String,
    pub playback_href: Option<String>,
    pub logs_href: String,
}

#[derive(Debug)]
pub struct RecordingCoverageMetricSnapshot {
    pub catalog_available: bool,
    pub catalog_revision: u64,
    pub window: CoverageWindow,
    pub cameras: Vec<CameraRecordingCoverage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CoverageQuery {
    start_ms: i64,
    end_ms: i64,
    minimum_gap_ms: u64,
    minimum_camera_gap_ms: u64,
    page_size: usize,
    search: String,
    state: Option<String>,
    stream: Option<String>,
    group: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PageToken {
    catalog_revision: u64,
    expires_at_ms: u64,
    after_camera_id: String,
    query: CoverageQuery,
}

struct ProjectionContext<'a> {
    window: CoverageWindow,
    generated_at_ms: u64,
    catalog: &'a HashMap<String, CatalogStreamCoverage>,
    writers: &'a HashMap<String, RecordingStreamHealthSnapshot>,
    health: &'a [CameraHealth],
    storage_paused: bool,
    recording_threshold_ms: u64,
}

struct GapBuildContext<'a> {
    camera_id: &'a str,
    stream_id: &'a str,
    window: CoverageWindow,
    generated_at_ms: u64,
    recording_requested: bool,
    detail_truncated: bool,
    deletions: &'a [CatalogDeletionRange],
}

pub(super) fn get(
    request: &Request,
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> Response {
    let generated_at_ms = unix_time_ms();
    let (query, cursor) = match parse_query(request, generated_at_ms) {
        Ok(parsed) => parsed,
        Err((status, message)) => return service_error(status, &message),
    };
    match build_response(router_tx, state, query, cursor, generated_at_ms) {
        Ok(response) => Response::json(&response).with_no_cache(),
        Err((status, message)) => service_error(status, &message),
    }
}

fn parse_query(
    request: &Request,
    generated_at_ms: u64,
) -> Result<(CoverageQuery, Option<PageToken>), (u16, String)> {
    if let Some(encoded) = request.get_param("page_token") {
        if encoded.len() > MAX_PAGE_TOKEN_BYTES {
            return Err((400, "recording coverage page token is too large".to_owned()));
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| (400, "recording coverage page token is invalid".to_owned()))?;
        let token = serde_json::from_slice::<PageToken>(&bytes)
            .map_err(|_| (400, "recording coverage page token is invalid".to_owned()))?;
        if generated_at_ms > token.expires_at_ms {
            return Err((409, "recording coverage page token expired".to_owned()));
        }
        validate_query(&token.query, generated_at_ms)?;
        return Ok((token.query.clone(), Some(token)));
    }

    let end_ms = query_u64(request, "end_ms")?.unwrap_or(generated_at_ms);
    let start_ms =
        query_u64(request, "start_ms")?.unwrap_or_else(|| end_ms.saturating_sub(DEFAULT_WINDOW_MS));
    if start_ms >= end_ms {
        return Err((400, "recording coverage start must precede end".to_owned()));
    }
    if end_ms.saturating_sub(start_ms) > MAX_WINDOW_MS {
        return Err((
            400,
            format!("recording coverage window cannot exceed {MAX_WINDOW_MS} milliseconds"),
        ));
    }
    if end_ms > generated_at_ms.saturating_add(60_000) {
        return Err((
            400,
            "recording coverage end cannot be more than one minute in the future".to_owned(),
        ));
    }
    let page_size = query_u64(request, "page_size")?
        .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
        .unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
        return Err((
            400,
            format!("recording coverage page size must be between 1 and {MAX_PAGE_SIZE}"),
        ));
    }
    let minimum_gap_ms = query_u64(request, "minimum_gap_ms")?
        .unwrap_or(DEFAULT_MINIMUM_GAP_MS)
        .min(end_ms - start_ms);
    let minimum_camera_gap_ms = query_u64(request, "minimum_camera_gap_ms")?
        .unwrap_or(0)
        .min(end_ms - start_ms);
    let search = request.get_param("search").unwrap_or_default();
    if search.chars().count() > MAX_SEARCH_CHARS {
        return Err((
            400,
            format!("recording coverage search cannot exceed {MAX_SEARCH_CHARS} characters"),
        ));
    }
    let state = request.get_param("state").filter(|value| !value.is_empty());
    if state.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "healthy" | "degraded" | "paused_by_policy" | "not_configured" | "unknown"
        )
    }) {
        return Err((400, "recording coverage state filter is invalid".to_owned()));
    }
    let stream = request
        .get_param("stream")
        .filter(|value| !value.is_empty());
    if stream
        .as_deref()
        .is_some_and(|value| !matches!(value, "main" | "sub"))
    {
        return Err((
            400,
            "recording coverage stream filter is invalid".to_owned(),
        ));
    }
    let group = request.get_param("group").filter(|value| !value.is_empty());
    if group
        .as_deref()
        .is_some_and(|value| value.chars().count() > MAX_SEARCH_CHARS)
    {
        return Err((
            400,
            "recording coverage group filter is too long".to_owned(),
        ));
    }
    let start_ms = i64::try_from(start_ms)
        .map_err(|_| (400, "recording coverage start is too large".to_owned()))?;
    let end_ms = i64::try_from(end_ms)
        .map_err(|_| (400, "recording coverage end is too large".to_owned()))?;
    Ok((
        CoverageQuery {
            start_ms,
            end_ms,
            minimum_gap_ms,
            minimum_camera_gap_ms,
            page_size,
            search: search.trim().to_lowercase(),
            state,
            stream,
            group,
        },
        None,
    ))
}

fn validate_query(query: &CoverageQuery, generated_at_ms: u64) -> Result<(), (u16, String)> {
    if query.start_ms >= query.end_ms
        || query.end_ms.saturating_sub(query.start_ms)
            > i64::try_from(MAX_WINDOW_MS).unwrap_or(i64::MAX)
        || query.page_size == 0
        || query.page_size > MAX_PAGE_SIZE
        || query.minimum_camera_gap_ms
            > u64::try_from(query.end_ms.saturating_sub(query.start_ms)).unwrap_or(u64::MAX)
        || query.search.chars().count() > MAX_SEARCH_CHARS
        || query
            .group
            .as_deref()
            .is_some_and(|value| value.chars().count() > MAX_SEARCH_CHARS)
    {
        return Err((
            400,
            "recording coverage page token is out of bounds".to_owned(),
        ));
    }
    let maximum_end_ms = i64::try_from(generated_at_ms.saturating_add(60_000)).unwrap_or(i64::MAX);
    if query.end_ms > maximum_end_ms {
        return Err((
            400,
            "recording coverage page token ends in the future".to_owned(),
        ));
    }
    if query.state.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "healthy" | "degraded" | "paused_by_policy" | "not_configured" | "unknown"
        )
    }) || query
        .stream
        .as_deref()
        .is_some_and(|value| !matches!(value, "main" | "sub"))
    {
        return Err((
            400,
            "recording coverage page token has invalid filters".to_owned(),
        ));
    }
    Ok(())
}

fn query_u64(request: &Request, name: &str) -> Result<Option<u64>, (u16, String)> {
    request.get_param(name).map_or(Ok(None), |value| {
        value.parse::<u64>().map(Some).map_err(|_| {
            (
                400,
                format!("recording coverage {name} must be an unsigned integer"),
            )
        })
    })
}

fn build_response(
    router_tx: &FacadeSender<RouterMessage>,
    state: &ServerState,
    query: CoverageQuery,
    cursor: Option<PageToken>,
    generated_at_ms: u64,
) -> Result<RecordingCoverageResponse, (u16, String)> {
    let health = server_health(router_tx, state);
    let catalog_snapshot = state.catalog.as_ref().map_or_else(
        || {
            Ok(CatalogCoverageSnapshot {
                revision: 0,
                updated_at_ms: 0,
                streams: Vec::new(),
            })
        },
        |catalog| {
            catalog
                .coverage(query.start_ms..query.end_ms)
                .map_err(|error| (503, format!("recording coverage is unavailable: {error}")))
        },
    )?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.catalog_revision != catalog_snapshot.revision)
    {
        return Err((
            409,
            "recording coverage changed; restart pagination".to_owned(),
        ));
    }

    let catalog_available = state.catalog.is_some();
    let catalog_revision = catalog_snapshot.revision;
    let catalog_updated_at_ms = catalog_available.then_some(catalog_snapshot.updated_at_ms);
    let catalog = catalog_snapshot
        .streams
        .into_iter()
        .map(|stream| (stream.stream_id.clone(), stream))
        .collect::<HashMap<_, _>>();
    let writers = state
        .recording_health
        .snapshot()
        .streams
        .into_iter()
        .map(|stream| (stream.stream_id.clone(), stream))
        .collect::<HashMap<_, _>>();
    let context = ProjectionContext {
        window: CoverageWindow {
            start_ms: query.start_ms,
            end_ms: query.end_ms,
            minimum_gap_ms: query.minimum_gap_ms,
        },
        generated_at_ms,
        catalog: &catalog,
        writers: &writers,
        health: &health.cameras,
        storage_paused: health.storage.safety.recording_state.as_str() == "paused",
        recording_threshold_ms: recording_freshness_threshold_ms(&state.storage_config),
    };
    let mut cameras = state
        .camera_entries()
        .iter()
        .map(|camera| project_camera(camera, &context))
        .collect::<Vec<_>>();
    let mut groups = cameras
        .iter()
        .flat_map(|camera| camera.groups.iter().cloned())
        .collect::<Vec<_>>();
    groups.sort_unstable();
    groups.dedup();
    cameras.retain(|camera| query_matches(camera, &query));
    cameras.sort_unstable_by(|left, right| left.camera_id.cmp(&right.camera_id));
    let totals = coverage_totals(&cameras);
    let findings = recording_findings(&cameras);
    let after_camera_id = cursor
        .as_ref()
        .map(|cursor| cursor.after_camera_id.as_str())
        .unwrap_or_default();
    let page_start = cameras.partition_point(|camera| camera.camera_id.as_str() <= after_camera_id);
    let page_end = page_start
        .saturating_add(query.page_size)
        .min(cameras.len());
    let mut page = cameras.drain(page_start..page_end).collect::<Vec<_>>();
    attach_gap_evidence(state, &mut page, &context);
    let next_page_token = (page_end < cameras.len().saturating_add(page.len())).then(|| {
        let after_camera_id = page
            .last()
            .map(|camera| camera.camera_id.clone())
            .unwrap_or_default();
        encode_page_token(PageToken {
            catalog_revision,
            expires_at_ms: generated_at_ms.saturating_add(PAGE_TOKEN_TTL_MS),
            after_camera_id,
            query: query.clone(),
        })
    });
    let estimated_bytes_per_day = totals.estimated_bytes_per_day;
    let recording_bytes = totals.recording_bytes;
    let effective_limit_bytes = health.storage.safety.effective_limit_bytes;
    let projected_retention_days = (estimated_bytes_per_day > 0).then(|| {
        effective_limit_bytes
            .unwrap_or(recording_bytes)
            .max(recording_bytes) as f64
            / estimated_bytes_per_day as f64
    });
    let storage = RecordingCoverageStorage {
        pressure: health.storage.safety.pressure.as_str().to_owned(),
        recording_state: health.storage.safety.recording_state.as_str().to_owned(),
        available_bytes: health.storage.safety.available_bytes,
        effective_limit_bytes,
        recording_bytes,
        estimated_bytes_per_day,
        projected_retention_days,
        projection_assumption: format!(
            "Selected finalized playable fragment bytes scaled from {} hours; future bitrate and cleanup behavior may differ",
            (query.end_ms - query.start_ms) as f64 / 3_600_000.0
        ),
    };

    Ok(RecordingCoverageResponse {
        generated_at_ms,
        catalog_available,
        catalog_revision,
        catalog_updated_at_ms,
        window: context.window,
        totals,
        storage,
        groups,
        cameras: page,
        findings,
        next_page_token: next_page_token.transpose().map_err(|error| (500, error))?,
    })
}

pub fn metric_snapshot(
    state: &ServerState,
    health: &ServerHealthResponse,
    generated_at_ms: u64,
) -> Result<RecordingCoverageMetricSnapshot, String> {
    let end_ms = i64::try_from(generated_at_ms).unwrap_or(i64::MAX);
    let start_ms = end_ms.saturating_sub(i64::try_from(DEFAULT_WINDOW_MS).unwrap_or(i64::MAX));
    let window = CoverageWindow {
        start_ms,
        end_ms,
        minimum_gap_ms: DEFAULT_MINIMUM_GAP_MS,
    };
    let catalog_snapshot = state.catalog.as_ref().map_or_else(
        || {
            Ok(CatalogCoverageSnapshot {
                revision: 0,
                updated_at_ms: 0,
                streams: Vec::new(),
            })
        },
        |catalog| {
            catalog
                .coverage(start_ms..end_ms)
                .map_err(|error| error.to_string())
        },
    )?;
    let catalog_available = state.catalog.is_some();
    let catalog_revision = catalog_snapshot.revision;
    let catalog = catalog_snapshot
        .streams
        .into_iter()
        .map(|stream| (stream.stream_id.clone(), stream))
        .collect::<HashMap<_, _>>();
    let writers = state
        .recording_health
        .snapshot()
        .streams
        .into_iter()
        .map(|stream| (stream.stream_id.clone(), stream))
        .collect::<HashMap<_, _>>();
    let context = ProjectionContext {
        window,
        generated_at_ms,
        catalog: &catalog,
        writers: &writers,
        health: &health.cameras,
        storage_paused: health.storage.safety.recording_state.as_str() == "paused",
        recording_threshold_ms: recording_freshness_threshold_ms(&state.storage_config),
    };
    let cameras = state
        .camera_entries()
        .iter()
        .map(|camera| project_camera(camera, &context))
        .collect();
    Ok(RecordingCoverageMetricSnapshot {
        catalog_available,
        catalog_revision,
        window,
        cameras,
    })
}

fn project_camera(
    camera: &CameraEntry,
    context: &ProjectionContext<'_>,
) -> CameraRecordingCoverage {
    let health = context
        .health
        .iter()
        .find(|health| health.id == camera.info.id);
    let mut stream_ids = camera
        .info
        .profiles
        .iter()
        .filter_map(|profile| normalized_video_stream_id(&profile.stream))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(health) = health {
        stream_ids.extend(
            health
                .dimensions
                .configured_video_stream_ids
                .iter()
                .cloned(),
        );
    }
    stream_ids.sort_unstable();
    stream_ids.dedup();
    let streams = stream_ids
        .iter()
        .map(|stream_id| project_stream(camera, stream_id, health, context))
        .collect::<Vec<_>>();
    let recording_requested = streams.iter().any(|stream| stream.recording_requested);
    let state = if streams.is_empty() {
        RecordingCoverageState::NotConfigured
    } else if !recording_requested {
        RecordingCoverageState::PausedByPolicy
    } else if streams
        .iter()
        .filter(|stream| stream.recording_requested)
        .all(|stream| stream.writer_state == WriterState::Progressing)
    {
        RecordingCoverageState::Healthy
    } else if streams.iter().any(|stream| {
        stream.recording_requested
            && matches!(
                stream.writer_state,
                WriterState::Stalled | WriterState::Failed
            )
    }) {
        RecordingCoverageState::Degraded
    } else {
        RecordingCoverageState::Unknown
    };
    CameraRecordingCoverage {
        camera_id: camera.info.id.clone(),
        camera_name: camera
            .info
            .name
            .clone()
            .unwrap_or_else(|| camera.info.ip.clone()),
        groups: camera.groups.clone(),
        state,
        recording_requested,
        policy: recording_policy_name(camera.configuration.recording_mode).to_owned(),
        streams,
        health_href: health_href(&camera.info.id),
    }
}

fn project_stream(
    camera: &CameraEntry,
    stream_id: &str,
    camera_health: Option<&CameraHealth>,
    context: &ProjectionContext<'_>,
) -> StreamRecordingCoverage {
    let recording_key = recording_stream_id(camera, stream_id);
    let catalog = context.catalog.get(&recording_key);
    let writer = context.writers.get(&recording_key);
    let stream_health = camera_health.and_then(|health| {
        health.streams.iter().find(|stream| {
            normalized_video_stream_id(&stream.ingress.report.kind) == Some(stream_id)
        })
    });
    let recording_requested =
        recording_mode_includes_stream(camera.configuration.recording_mode, stream_id);
    let writer_state = project_writer_state(
        recording_requested,
        context.storage_paused,
        writer,
        stream_health,
        context.recording_threshold_ms,
    );
    let window_duration_ms =
        u64::try_from(context.window.end_ms - context.window.start_ms).unwrap_or(u64::MAX);
    let selected_coverage_ms = catalog.map_or(0, |catalog| catalog.selected_coverage_ms);
    let selected_fragment_bytes = catalog.map_or(0, |catalog| catalog.selected_fragment_bytes);
    let estimated_bytes_per_day = u128::from(selected_fragment_bytes)
        .saturating_mul(u128::from(DAY_MS))
        .checked_div(u128::from(window_duration_ms))
        .unwrap_or(0)
        .try_into()
        .unwrap_or(u64::MAX);
    let coverage_percent = if window_duration_ms == 0 {
        0.0
    } else {
        selected_coverage_ms as f64 * 100.0 / window_duration_ms as f64
    };
    let ranges = catalog.map_or_else(Vec::new, |catalog| {
        catalog
            .ranges
            .iter()
            .map(|&(start_ms, end_ms)| CoverageRange { start_ms, end_ms })
            .collect()
    });
    let range_count = catalog.map_or(0, |catalog| catalog.range_count);
    let bucket_ms = catalog.map_or(0, |catalog| catalog.bucket_ms);
    let buckets = catalog.map_or_else(Vec::new, |catalog| {
        catalog
            .buckets
            .iter()
            .map(|bucket| CoverageBucket {
                start_ms: bucket.start_ms,
                end_ms: bucket.end_ms,
                coverage_ms: bucket.coverage_ms,
            })
            .collect()
    });
    let gap_count = selected_gap_count(catalog, context.window);
    let largest_gap_ms = selected_largest_gap(catalog, context.window);
    let detail_truncated = usize::try_from(range_count).map_or(true, |count| count > ranges.len());
    let gaps = build_gaps(
        GapBuildContext {
            camera_id: &camera.info.id,
            stream_id,
            window: context.window,
            generated_at_ms: context.generated_at_ms,
            recording_requested,
            detail_truncated,
            deletions: catalog.map_or(&[], |catalog| catalog.deletions.as_slice()),
        },
        &ranges,
    );
    let oldest_retained_at_ms = catalog.and_then(|catalog| catalog.oldest_recording_at_ms);
    let newest_retained_at_ms = catalog.and_then(|catalog| catalog.newest_recording_at_ms);
    StreamRecordingCoverage {
        stream_id: stream_id.to_owned(),
        recording_stream_id: recording_key,
        recording_requested,
        writer_state,
        last_frame_at_ms: stream_health.and_then(|stream| stream.ingress.frame_updated_at_ms),
        last_write_at_ms: writer.and_then(|writer| writer.last_progress_at_ms),
        last_finalize_at_ms: catalog.and_then(|catalog| catalog.last_finalized_at_ms),
        last_catalog_commit_at_ms: catalog.and_then(|catalog| catalog.last_catalog_commit_at_ms),
        oldest_retained_at_ms,
        newest_retained_at_ms,
        effective_retention_ms: catalog.map(|catalog| catalog.retained_coverage_ms),
        recording_bytes: catalog.map_or(0, |catalog| catalog.recording_bytes),
        estimated_bytes_per_day,
        selected_coverage_ms,
        coverage_percent,
        gap_count,
        largest_gap_ms,
        playable_fragments: catalog.map_or(0, |catalog| catalog.playable_fragments),
        ranges,
        range_count,
        bucket_ms,
        buckets,
        detail_truncated,
        gaps,
    }
}

fn selected_gap_count(coverage: Option<&CatalogStreamCoverage>, window: CoverageWindow) -> u64 {
    let Some(coverage) = coverage else {
        return 1;
    };
    if coverage.range_count == 0 {
        return 1;
    }
    coverage
        .range_count
        .saturating_sub(1)
        .saturating_add(u64::from(
            coverage
                .selected_first_start_ms
                .is_some_and(|start_ms| start_ms > window.start_ms),
        ))
        .saturating_add(u64::from(
            coverage
                .selected_last_end_ms
                .is_some_and(|end_ms| end_ms < window.end_ms),
        ))
}

fn selected_largest_gap(coverage: Option<&CatalogStreamCoverage>, window: CoverageWindow) -> u64 {
    let Some(coverage) = coverage else {
        return u64::try_from(window.end_ms.saturating_sub(window.start_ms)).unwrap_or(u64::MAX);
    };
    let leading = coverage
        .selected_first_start_ms
        .unwrap_or(window.end_ms)
        .saturating_sub(window.start_ms);
    let trailing = window
        .end_ms
        .saturating_sub(coverage.selected_last_end_ms.unwrap_or(window.start_ms));
    coverage
        .largest_gap_ms
        .max(u64::try_from(leading).unwrap_or(u64::MAX))
        .max(u64::try_from(trailing).unwrap_or(u64::MAX))
}

fn project_writer_state(
    recording_requested: bool,
    storage_paused: bool,
    writer: Option<&RecordingStreamHealthSnapshot>,
    stream: Option<&StreamHealth>,
    threshold_ms: u64,
) -> WriterState {
    if !recording_requested {
        return WriterState::PolicyDisabled;
    }
    if storage_paused {
        return WriterState::Stalled;
    }
    if writer
        .and_then(|writer| writer.last_error.as_ref())
        .is_some()
    {
        return WriterState::Failed;
    }
    if let Some(progressing) = stream.and_then(|stream| stream.dimensions.recording_progressing) {
        return if progressing {
            WriterState::Progressing
        } else {
            WriterState::Stalled
        };
    }
    if writer
        .and_then(|writer| writer.progress_age_ms)
        .is_some_and(|age_ms| age_ms <= threshold_ms)
    {
        return WriterState::Progressing;
    }
    if writer
        .and_then(|writer| writer.attempt_age_ms)
        .is_some_and(|age_ms| age_ms > threshold_ms)
    {
        return WriterState::Stalled;
    }
    if writer.is_some() {
        WriterState::Pending
    } else {
        WriterState::Unknown
    }
}

fn build_gaps(context: GapBuildContext<'_>, ranges: &[CoverageRange]) -> Vec<RecordingGap> {
    if !context.recording_requested {
        return Vec::new();
    }
    let detail_start_ms = if context.detail_truncated {
        ranges
            .first()
            .map_or(context.window.end_ms, |range| range.start_ms)
    } else {
        context.window.start_ms
    };
    let mut gaps = Vec::with_capacity(ranges.len().saturating_add(1));
    let mut cursor_ms = detail_start_ms;
    for range in ranges {
        if range.start_ms > cursor_ms {
            push_gap(
                &mut gaps,
                context.camera_id,
                context.stream_id,
                cursor_ms,
                range.start_ms,
                context.window,
                context.generated_at_ms,
            );
        }
        cursor_ms = cursor_ms.max(range.end_ms);
    }
    if cursor_ms < context.window.end_ms {
        push_gap(
            &mut gaps,
            context.camera_id,
            context.stream_id,
            cursor_ms,
            context.window.end_ms,
            context.window,
            context.generated_at_ms,
        );
    }
    for gap in &mut gaps {
        if let Some(deletion) = context.deletions.iter().find(|deletion| {
            deletion.start_ms < gap.observed_end_ms && deletion.end_ms > gap.start_ms
        }) {
            gap.cause = match deletion.reason {
                CatalogDeletionReason::ArchiveLimit | CatalogDeletionReason::DiskPressure => {
                    RecordingGapCause::RetentionDeletion
                }
                CatalogDeletionReason::Reconciliation => RecordingGapCause::CatalogMismatch,
                CatalogDeletionReason::Migration => RecordingGapCause::Migration,
                CatalogDeletionReason::Unknown => RecordingGapCause::Unknown,
            };
            gap.explanation = deletion_explanation(deletion.reason).to_owned();
            gap.evidence_source = "catalog_deletion_ledger".to_owned();
        }
    }
    gaps
}

const fn deletion_explanation(reason: CatalogDeletionReason) -> &'static str {
    match reason {
        CatalogDeletionReason::ArchiveLimit => {
            "Footage was removed to enforce the configured archive size limit"
        }
        CatalogDeletionReason::DiskPressure => {
            "Footage was removed to restore recording filesystem headroom"
        }
        CatalogDeletionReason::Reconciliation => {
            "Catalog coverage was removed while reconciling unavailable media"
        }
        CatalogDeletionReason::Migration => {
            "Footage was removed from this catalog during storage migration"
        }
        CatalogDeletionReason::Unknown => {
            "Footage was removed, but the deletion reason is unavailable"
        }
    }
}

fn push_gap(
    gaps: &mut Vec<RecordingGap>,
    camera_id: &str,
    stream_id: &str,
    start_ms: i64,
    observed_end_ms: i64,
    window: CoverageWindow,
    generated_at_ms: u64,
) {
    let duration_ms = u64::try_from(observed_end_ms.saturating_sub(start_ms)).unwrap_or(u64::MAX);
    if duration_ms < window.minimum_gap_ms {
        return;
    }
    let now_ms = i64::try_from(generated_at_ms).unwrap_or(i64::MAX);
    let current = observed_end_ms == window.end_ms
        && window.end_ms >= now_ms.saturating_sub(1_000)
        && observed_end_ms >= now_ms.saturating_sub(1_000);
    gaps.push(RecordingGap {
        start_ms,
        end_ms: (!current).then_some(observed_end_ms),
        observed_end_ms,
        duration_ms,
        cause: RecordingGapCause::Unknown,
        explanation: "No bounded cause evidence overlaps this recording gap".to_owned(),
        evidence_source: "catalog_coverage".to_owned(),
        operational_event_id: None,
        before_href: (start_ms > window.start_ms)
            .then(|| keep_href(camera_id, stream_id, start_ms.saturating_sub(1))),
        after_href: (!current && observed_end_ms < window.end_ms)
            .then(|| keep_href(camera_id, stream_id, observed_end_ms)),
        health_href: health_href(camera_id),
        logs_href: "/settings/logs".to_owned(),
    });
}

fn attach_gap_evidence(
    state: &ServerState,
    cameras: &mut [CameraRecordingCoverage],
    context: &ProjectionContext<'_>,
) {
    for camera in cameras {
        let events = state.events.as_ref().map_or_else(Vec::new, |store| {
            store
                .operational_events_in_range(
                    &camera.camera_id,
                    context.window.start_ms,
                    context.window.end_ms,
                )
                .unwrap_or_default()
        });
        let camera_health = context
            .health
            .iter()
            .find(|health| health.id == camera.camera_id);
        for stream in &mut camera.streams {
            let stream_health = camera_health.and_then(|health| {
                health.streams.iter().find(|candidate| {
                    normalized_video_stream_id(&candidate.ingress.report.kind)
                        == Some(stream.stream_id.as_str())
                })
            });
            let writer = context.writers.get(&stream.recording_stream_id);
            for gap in &mut stream.gaps {
                classify_gap(
                    gap,
                    &stream.stream_id,
                    &events,
                    stream_health,
                    writer,
                    context.storage_paused,
                );
            }
        }
    }
}

fn classify_gap(
    gap: &mut RecordingGap,
    stream_id: &str,
    events: &[OperationalEvent],
    stream: Option<&StreamHealth>,
    writer: Option<&RecordingStreamHealthSnapshot>,
    storage_paused: bool,
) {
    if gap.evidence_source == "catalog_deletion_ledger" {
        return;
    }
    let event = events
        .iter()
        .filter(|event| {
            event
                .key
                .stream_id
                .as_deref()
                .is_none_or(|event_stream| event_stream == stream_id)
                && event.start_time_ms < gap.observed_end_ms
                && event.end_time_ms.unwrap_or(i64::MAX) > gap.start_ms
        })
        .max_by_key(|event| gap_cause_priority(event.key.kind));
    if let Some(event) = event {
        gap.cause = event_gap_cause(event);
        gap.explanation = event.evidence.explanation.clone();
        gap.evidence_source = event.evidence.source.clone();
        gap.operational_event_id = Some(event.id.clone());
        return;
    }
    if gap.end_ms.is_some() {
        return;
    }
    if storage_paused {
        gap.cause = RecordingGapCause::DiskPressure;
        gap.explanation = "Recording is paused by the storage safety policy".to_owned();
        gap.evidence_source = "storage_safety".to_owned();
    } else if let Some(error) = writer.and_then(|writer| writer.last_error.as_ref()) {
        gap.cause = if error.to_lowercase().contains("disk") {
            RecordingGapCause::DiskPressure
        } else {
            RecordingGapCause::WriterFailure
        };
        gap.explanation = error.clone();
        gap.evidence_source = "recording_writer".to_owned();
    } else if stream.and_then(|stream| stream.dimensions.transport_connected) == Some(false) {
        gap.cause = RecordingGapCause::TransportOutage;
        gap.explanation = "The camera transport is disconnected".to_owned();
        gap.evidence_source = "canonical_health".to_owned();
    } else if stream.is_some_and(|stream| !stream.dimensions.frames_fresh) {
        gap.cause = RecordingGapCause::StaleFrames;
        gap.explanation = "Frames are not fresh enough to sustain recording".to_owned();
        gap.evidence_source = "canonical_health".to_owned();
    } else if stream.is_some_and(|stream| !stream.dimensions.decodable) {
        gap.cause = RecordingGapCause::DecodeFailure;
        gap.explanation = "Recent keyframes are unavailable for decoding".to_owned();
        gap.evidence_source = "canonical_health".to_owned();
    } else if writer.and_then(|writer| writer.progress_age_ms).is_some() {
        gap.cause = RecordingGapCause::WriterFailure;
        gap.explanation = "The recording writer is not making current progress".to_owned();
        gap.evidence_source = "recording_writer".to_owned();
    }
}

const fn gap_cause_priority(kind: OperationalEventKind) -> u8 {
    match kind {
        OperationalEventKind::RecordingInterrupted => 4,
        OperationalEventKind::DecodeUnavailable => 3,
        OperationalEventKind::CameraOffline => 2,
        OperationalEventKind::StreamStale => 1,
    }
}

fn event_gap_cause(event: &OperationalEvent) -> RecordingGapCause {
    if event.evidence.cause.contains("disk") || event.evidence.cause.contains("storage") {
        return RecordingGapCause::DiskPressure;
    }
    match event.key.kind {
        OperationalEventKind::CameraOffline => RecordingGapCause::TransportOutage,
        OperationalEventKind::StreamStale => {
            if event.evidence.cause.contains("frames_not_arriving") {
                RecordingGapCause::SourceSilence
            } else {
                RecordingGapCause::StaleFrames
            }
        }
        OperationalEventKind::DecodeUnavailable => RecordingGapCause::DecodeFailure,
        OperationalEventKind::RecordingInterrupted => RecordingGapCause::WriterFailure,
    }
}

fn query_matches(camera: &CameraRecordingCoverage, query: &CoverageQuery) -> bool {
    let search_matches = query.search.is_empty()
        || camera.camera_id.to_lowercase().contains(&query.search)
        || camera.camera_name.to_lowercase().contains(&query.search);
    let state_matches = query
        .state
        .as_deref()
        .is_none_or(|state| camera.state.as_str() == state);
    let stream_matches = query.stream.as_deref().is_none_or(|stream| {
        camera
            .streams
            .iter()
            .any(|candidate| candidate.stream_id == stream)
    });
    let group_matches = query.group.as_deref().is_none_or(|group| {
        camera
            .groups
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(group))
    });
    let gap_matches = query.minimum_camera_gap_ms == 0
        || camera.streams.iter().any(|stream| {
            stream.recording_requested && stream.largest_gap_ms >= query.minimum_camera_gap_ms
        });
    search_matches && state_matches && stream_matches && group_matches && gap_matches
}

fn coverage_totals(cameras: &[CameraRecordingCoverage]) -> RecordingCoverageTotals {
    let mut totals = RecordingCoverageTotals {
        cameras: cameras.len(),
        ..RecordingCoverageTotals::default()
    };
    for camera in cameras {
        match camera.state {
            RecordingCoverageState::Healthy => totals.healthy += 1,
            RecordingCoverageState::Degraded => totals.degraded += 1,
            RecordingCoverageState::PausedByPolicy => totals.paused_by_policy += 1,
            RecordingCoverageState::NotConfigured => totals.not_configured += 1,
            RecordingCoverageState::Unknown => totals.unknown += 1,
        }
        for stream in &camera.streams {
            totals.recording_bytes = totals
                .recording_bytes
                .saturating_add(stream.recording_bytes);
            totals.estimated_bytes_per_day = totals
                .estimated_bytes_per_day
                .saturating_add(stream.estimated_bytes_per_day);
        }
    }
    totals
}

fn recording_findings(cameras: &[CameraRecordingCoverage]) -> Vec<RecordingFinding> {
    let mut findings = Vec::new();
    for camera in cameras {
        for stream in &camera.streams {
            if let Some((severity, message)) = match stream.writer_state {
                WriterState::Failed => Some(("critical", "Recording writer failed")),
                WriterState::Stalled => Some(("warning", "Recording writer is not progressing")),
                _ => None,
            } {
                findings.push(RecordingFinding {
                    severity: severity.to_owned(),
                    camera_id: camera.camera_id.clone(),
                    camera_name: camera.camera_name.clone(),
                    stream_id: Some(stream.stream_id.clone()),
                    kind: "writer_state".to_owned(),
                    message: message.to_owned(),
                    started_at_ms: stream
                        .last_write_at_ms
                        .and_then(|value| i64::try_from(value).ok()),
                    health_href: camera.health_href.clone(),
                    playback_href: None,
                    logs_href: "/settings/logs".to_owned(),
                });
            }
            if let Some(gap) = stream.gaps.last() {
                findings.push(RecordingFinding {
                    severity: "warning".to_owned(),
                    camera_id: camera.camera_id.clone(),
                    camera_name: camera.camera_name.clone(),
                    stream_id: Some(stream.stream_id.clone()),
                    kind: "recording_gap".to_owned(),
                    message: if gap.end_ms.is_none() {
                        format!("Open recording gap: {} ms observed", gap.duration_ms)
                    } else {
                        format!("Recording gap: {} ms", gap.duration_ms)
                    },
                    started_at_ms: Some(gap.start_ms),
                    health_href: gap.health_href.clone(),
                    playback_href: gap.after_href.clone().or_else(|| gap.before_href.clone()),
                    logs_href: gap.logs_href.clone(),
                });
            }
        }
    }
    findings.sort_unstable_by(|left, right| {
        finding_priority(&right.severity)
            .cmp(&finding_priority(&left.severity))
            .then(right.started_at_ms.cmp(&left.started_at_ms))
            .then(left.camera_name.cmp(&right.camera_name))
    });
    findings.truncate(100);
    findings
}

const fn finding_priority(severity: &str) -> u8 {
    match severity.as_bytes() {
        b"critical" => 2,
        b"warning" => 1,
        _ => 0,
    }
}

const fn recording_policy_name(mode: crate::cameras::CameraRecordingMode) -> &'static str {
    match mode {
        crate::cameras::CameraRecordingMode::Off => "off",
        crate::cameras::CameraRecordingMode::Sub => "sub",
        crate::cameras::CameraRecordingMode::Main => "main",
        crate::cameras::CameraRecordingMode::Both => "both",
        crate::cameras::CameraRecordingMode::EventBoost => "event-boost",
    }
}

fn encode_page_token(token: PageToken) -> Result<String, String> {
    serde_json::to_vec(&token)
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|error| format!("unable to encode recording coverage page token: {error}"))
}

fn keep_href(camera_id: &str, stream_id: &str, at_ms: i64) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("camera", camera_id);
    query.append_pair("stream", stream_id);
    query.append_pair("at", &at_ms.to_string());
    format!("/keep?{}", query.finish())
}

fn health_href(camera_id: &str) -> String {
    let encoded = url::form_urlencoded::byte_serialize(camera_id.as_bytes()).collect::<String>();
    format!("/system-health/camera/{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operational_event(kind: OperationalEventKind, cause: &str) -> OperationalEvent {
        OperationalEvent {
            id: format!("{}-{cause}", kind.as_str()),
            key: crate::operational_events::OperationalEventKey {
                camera_id: "front".to_owned(),
                stream_id: Some("main".to_owned()),
                kind,
            },
            evidence: crate::operational_events::OperationalEvidence {
                cause: cause.to_owned(),
                explanation: cause.to_owned(),
                affected_streams: vec!["main".to_owned()],
                recording_interrupted: true,
                source: "test".to_owned(),
            },
            severity: crate::operational_events::OperationalSeverity::Warning,
            revision: 1,
            start_time_ms: 1_000,
            end_time_ms: Some(2_000),
            duration_ms: Some(1_000),
        }
    }

    #[test]
    fn gaps_are_half_open_and_current_gap_has_no_claimed_end() {
        let gaps = build_gaps(
            GapBuildContext {
                camera_id: "front",
                stream_id: "main",
                window: CoverageWindow {
                    start_ms: 1_000,
                    end_ms: 2_000,
                    minimum_gap_ms: 100,
                },
                generated_at_ms: 2_000,
                recording_requested: true,
                detail_truncated: false,
                deletions: &[],
            },
            &[
                CoverageRange {
                    start_ms: 1_000,
                    end_ms: 1_400,
                },
                CoverageRange {
                    start_ms: 1_500,
                    end_ms: 1_800,
                },
            ],
        );

        assert_eq!(gaps.len(), 2);
        assert_eq!((gaps[0].start_ms, gaps[0].end_ms), (1_400, Some(1_500)));
        assert_eq!((gaps[1].start_ms, gaps[1].end_ms), (1_800, None));
        assert_eq!(gaps[1].observed_end_ms, 2_000);
    }

    #[test]
    fn policy_disabled_is_not_reported_as_an_unexpected_gap() {
        assert!(
            build_gaps(
                GapBuildContext {
                    camera_id: "front",
                    stream_id: "sub",
                    window: CoverageWindow {
                        start_ms: 1_000,
                        end_ms: 2_000,
                        minimum_gap_ms: 1,
                    },
                    generated_at_ms: 2_000,
                    recording_requested: false,
                    detail_truncated: false,
                    deletions: &[],
                },
                &[],
            )
            .is_empty()
        );
    }

    #[test]
    fn truncated_detail_does_not_invent_an_earlier_gap() {
        let gaps = build_gaps(
            GapBuildContext {
                camera_id: "front",
                stream_id: "main",
                window: CoverageWindow {
                    start_ms: 1_000,
                    end_ms: 3_000,
                    minimum_gap_ms: 1,
                },
                generated_at_ms: 4_000,
                recording_requested: true,
                detail_truncated: true,
                deletions: &[],
            },
            &[CoverageRange {
                start_ms: 2_000,
                end_ms: 2_500,
            }],
        );

        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].start_ms, 2_500);
    }

    #[test]
    fn deletion_ledger_classifies_a_missing_interval() {
        let gaps = build_gaps(
            GapBuildContext {
                camera_id: "front",
                stream_id: "main",
                window: CoverageWindow {
                    start_ms: 1_000,
                    end_ms: 3_000,
                    minimum_gap_ms: 1,
                },
                generated_at_ms: 4_000,
                recording_requested: true,
                detail_truncated: false,
                deletions: &[CatalogDeletionRange {
                    start_ms: 1_500,
                    end_ms: 2_000,
                    deleted_at_ms: 4_000,
                    reason: CatalogDeletionReason::ArchiveLimit,
                }],
            },
            &[
                CoverageRange {
                    start_ms: 1_000,
                    end_ms: 1_500,
                },
                CoverageRange {
                    start_ms: 2_000,
                    end_ms: 3_000,
                },
            ],
        );

        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].cause, RecordingGapCause::RetentionDeletion);
        assert_eq!(gaps[0].evidence_source, "catalog_deletion_ledger");
    }

    #[test]
    fn operational_evidence_projects_every_runtime_gap_cause() {
        for (event, expected) in [
            (
                operational_event(
                    OperationalEventKind::CameraOffline,
                    "transport_disconnected",
                ),
                RecordingGapCause::TransportOutage,
            ),
            (
                operational_event(OperationalEventKind::StreamStale, "frames_not_arriving"),
                RecordingGapCause::SourceSilence,
            ),
            (
                operational_event(OperationalEventKind::StreamStale, "stream_report_stale"),
                RecordingGapCause::StaleFrames,
            ),
            (
                operational_event(OperationalEventKind::DecodeUnavailable, "keyframes_missing"),
                RecordingGapCause::DecodeFailure,
            ),
            (
                operational_event(
                    OperationalEventKind::RecordingInterrupted,
                    "recording_not_progressing",
                ),
                RecordingGapCause::WriterFailure,
            ),
            (
                operational_event(OperationalEventKind::RecordingInterrupted, "disk_pressure"),
                RecordingGapCause::DiskPressure,
            ),
        ] {
            assert_eq!(event_gap_cause(&event), expected);
        }
    }

    #[test]
    fn deletion_and_missing_evidence_project_catalog_migration_and_unknown_causes() {
        for (reason, expected) in [
            (
                CatalogDeletionReason::Reconciliation,
                RecordingGapCause::CatalogMismatch,
            ),
            (
                CatalogDeletionReason::Migration,
                RecordingGapCause::Migration,
            ),
            (CatalogDeletionReason::Unknown, RecordingGapCause::Unknown),
        ] {
            let gaps = build_gaps(
                GapBuildContext {
                    camera_id: "front",
                    stream_id: "main",
                    window: CoverageWindow {
                        start_ms: 1_000,
                        end_ms: 2_000,
                        minimum_gap_ms: 1,
                    },
                    generated_at_ms: 3_000,
                    recording_requested: true,
                    detail_truncated: false,
                    deletions: &[CatalogDeletionRange {
                        start_ms: 1_000,
                        end_ms: 2_000,
                        deleted_at_ms: 3_000,
                        reason,
                    }],
                },
                &[],
            );
            assert_eq!(gaps[0].cause, expected);
        }
    }

    #[test]
    fn group_filter_matches_configured_membership_case_insensitively() {
        let camera = CameraRecordingCoverage {
            camera_id: "front".to_owned(),
            camera_name: "Front Door".to_owned(),
            groups: vec!["Exterior".to_owned()],
            state: RecordingCoverageState::Healthy,
            recording_requested: true,
            policy: "main".to_owned(),
            streams: Vec::new(),
            health_href: "/system-health/camera/front".to_owned(),
        };
        let query = CoverageQuery {
            start_ms: 1_000,
            end_ms: 2_000,
            minimum_gap_ms: 1,
            minimum_camera_gap_ms: 0,
            page_size: 25,
            search: String::new(),
            state: None,
            stream: None,
            group: Some("exterior".to_owned()),
        };

        assert!(query_matches(&camera, &query));
    }

    #[test]
    fn page_token_query_bounds_are_revalidated() {
        let token = encode_page_token(PageToken {
            catalog_revision: 1,
            expires_at_ms: 10_000,
            after_camera_id: "front".to_owned(),
            query: CoverageQuery {
                start_ms: 0,
                end_ms: MAX_WINDOW_MS as i64 + 1,
                minimum_gap_ms: 1,
                minimum_camera_gap_ms: 0,
                page_size: 25,
                search: String::new(),
                state: None,
                stream: None,
                group: None,
            },
        })
        .unwrap();
        let request = Request::fake_http(
            "GET",
            format!("/recording-coverage?page_token={token}"),
            Vec::new(),
            Vec::new(),
        );

        assert!(matches!(parse_query(&request, 1_000), Err((400, _))));
    }
}
