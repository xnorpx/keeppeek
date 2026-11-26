//! Bounded, authorized server dispatch for event activity summaries and rollup maintenance.

use super::{
    ApiPrincipal, ControlCommandError, ServerState, data_channel_target, event_search_catalog,
    unix_time_ms, validate_client_id,
};
use crate::{
    access::CameraAccess,
    api::proto::{self, activity_analytics_command, activity_analytics_result, ok as control_ok},
    config::AnalyticsConfig,
    metrics::AnalyticsMetricsSnapshot,
    storage::analytics::{
        ActivitySummaryQuery, AuthorizedCamera, AuthorizedGroup, MAX_BUCKETS, MAX_RANKED_ROWS,
        MAX_REBUILD_BATCH_ROWS, MAX_RESPONSE_BYTES, MAX_SERIES, RollupState,
    },
    webrtc::{DataChannelTarget, OutboundDataMessage, SessionId},
};
use prost::Message as _;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) mod proto_map;

pub(super) const CAPABILITY_ID: &str = "keeppeek.activity-analytics.v1";
const MAX_ACTIVE_PER_PRINCIPAL: usize = 2;
const MAX_ACTIVE_GLOBAL: usize = 8;
const QUERY_DEADLINE: Duration = Duration::from_secs(5);
const CHUNK_BYTES: usize = 48 * 1_024;

#[derive(Clone)]
pub(super) struct Runtime {
    config: AnalyticsConfig,
    active: Arc<Mutex<HashMap<(SessionId, String), ActiveQuery>>>,
    rebuilding: Arc<AtomicBool>,
    metrics: Arc<MetricsCounters>,
}

struct ActiveQuery {
    principal_id: String,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct MetricsCounters {
    queries_total: AtomicU64,
    query_errors_total: AtomicU64,
    rebuild_runs_total: AtomicU64,
    last_query_duration_ms: AtomicU64,
}

impl Runtime {
    pub(super) fn new(config: AnalyticsConfig) -> Self {
        Self {
            config,
            active: Arc::new(Mutex::new(HashMap::new())),
            rebuilding: Arc::new(AtomicBool::new(false)),
            metrics: Arc::new(MetricsCounters::default()),
        }
    }

    pub(super) const fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub(super) const fn config(&self) -> &AnalyticsConfig {
        &self.config
    }

    pub(super) fn metric_snapshot(&self, state: &ServerState) -> Option<AnalyticsMetricsSnapshot> {
        let catalog = state.catalog.as_ref()?;
        let status = catalog
            .analytics_status(&self.config.timezone, self.config.week_start)
            .ok()?;
        let active_queries = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len() as u64;
        Some(AnalyticsMetricsSnapshot {
            rollup_state: match status.state {
                RollupState::Ready => 0,
                RollupState::Rebuilding => 1,
                RollupState::Degraded => 2,
                RollupState::Unavailable => 3,
            },
            rollup_rows: status.rollup_row_count,
            active_queries,
            queries_total: self.metrics.queries_total.load(Ordering::Relaxed),
            query_errors_total: self.metrics.query_errors_total.load(Ordering::Relaxed),
            rebuild_runs_total: self.metrics.rebuild_runs_total.load(Ordering::Relaxed),
            last_query_duration_ms: self.metrics.last_query_duration_ms.load(Ordering::Relaxed),
        })
    }
}

pub(super) fn dispatch(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    command: proto::ActivityAnalyticsCommand,
) -> Result<(Option<control_ok::Result>, Vec<OutboundDataMessage>), ControlCommandError> {
    if !state.analytics.enabled() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "activity analytics is disabled by server configuration",
        ));
    }
    match command.action {
        Some(activity_analytics_command::Action::Query(request)) => {
            dispatch_query(state, session_id, principal, request)
        }
        Some(activity_analytics_command::Action::CancelQuery(request)) => {
            validate_client_id(&request.query_id, "activity analytics query ID")?;
            cancel_query(state, session_id, &request.query_id);
            Ok((None, Vec::new()))
        }
        Some(activity_analytics_command::Action::GetStatus(_)) => {
            let catalog = event_search_catalog(state)?;
            let cfg = state.analytics.config();
            let status = catalog
                .analytics_status(&cfg.timezone, cfg.week_start)
                .map_err(|error| unavailable("read activity analytics status", error))?;
            Ok((
                Some(control_ok::Result::ActivityAnalyticsResult(
                    proto::ActivityAnalyticsResult {
                        result: Some(activity_analytics_result::Result::Status(
                            proto_map::proto_status(status),
                        )),
                    },
                )),
                Vec::new(),
            ))
        }
        Some(activity_analytics_command::Action::Rebuild(_)) => {
            let status = trigger_rebuild(state)?;
            Ok((
                Some(control_ok::Result::ActivityAnalyticsResult(
                    proto::ActivityAnalyticsResult {
                        result: Some(activity_analytics_result::Result::Status(
                            proto_map::proto_status(status),
                        )),
                    },
                )),
                Vec::new(),
            ))
        }
        None => Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "activity analytics command has no action",
        )),
    }
}

pub(super) fn close_session(state: &ServerState, session_id: SessionId) {
    let active = state
        .analytics
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for ((owner, _), task) in active.iter() {
        if *owner == session_id {
            task.cancelled.store(true, Ordering::Release);
        }
    }
}

fn dispatch_query(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    request: proto::QueryActivitySummary,
) -> Result<(Option<control_ok::Result>, Vec<OutboundDataMessage>), ControlCommandError> {
    validate_client_id(&request.query_id, "activity analytics query ID")?;
    let (_, channel) = data_channel_target(request.channel)?;
    if channel != proto::DataChannelKind::ReliableData {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "activity analytics results require reliable-data",
        ));
    }
    let catalog = event_search_catalog(state)?;
    let access = super::camera_access::for_session(state, session_id)?;
    let query = build_domain_query(state, principal, &access, &request)?;
    let query_id = request.query_id;
    let delivery = proto::ActivityAnalyticsDelivery {
        query_id: query_id.clone(),
        channel: channel as i32,
    };
    let ok = control_ok::Result::ActivityAnalyticsResult(proto::ActivityAnalyticsResult {
        result: Some(activity_analytics_result::Result::Delivery(delivery)),
    });
    if !state.webrtc.has_api_session(session_id) {
        let messages = run_query_sync(state, catalog, &query_id, query)?;
        return Ok((Some(ok), messages));
    }
    let cancelled = reserve_slot(state, session_id, &principal.id(), &query_id)?;
    let worker_state = state.clone();
    let worker_query_id = query_id.clone();
    let worker_catalog = catalog.clone();
    let spawn = std::thread::Builder::new()
        .name("activity-analytics-query".to_owned())
        .spawn(move || {
            run_query_async(
                worker_state,
                session_id,
                worker_query_id,
                query,
                cancelled,
                worker_catalog,
            );
        });
    if let Err(error) = spawn {
        release_slot(state, session_id, &query_id);
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to start activity analytics query: {error}"),
        ));
    }
    Ok((Some(ok), Vec::new()))
}

fn run_query_sync(
    state: &ServerState,
    catalog: &crate::storage::RecordingCatalogHandle,
    query_id: &str,
    query: ActivitySummaryQuery,
) -> Result<Vec<OutboundDataMessage>, ControlCommandError> {
    let started = Instant::now();
    state
        .analytics
        .metrics
        .queries_total
        .fetch_add(1, Ordering::Relaxed);
    let summary = catalog.query_activity_summary(query).map_err(|error| {
        state
            .analytics
            .metrics
            .query_errors_total
            .fetch_add(1, Ordering::Relaxed);
        map_query_error(error)
    })?;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    state
        .analytics
        .metrics
        .last_query_duration_ms
        .store(elapsed_ms, Ordering::Relaxed);
    if started.elapsed() > QUERY_DEADLINE {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Timeout,
            504,
            "activity analytics query exceeded 5 second deadline",
        ));
    }
    encode_summary_chunks(query_id, summary)
}

fn run_query_async(
    state: ServerState,
    session_id: SessionId,
    query_id: String,
    query: ActivitySummaryQuery,
    cancelled: Arc<AtomicBool>,
    catalog: crate::storage::RecordingCatalogHandle,
) {
    let group = format!("activity-analytics:{query_id}");
    let started = Instant::now();
    state
        .analytics
        .metrics
        .queries_total
        .fetch_add(1, Ordering::Relaxed);
    let result = execute_async_query(
        &state, session_id, &query_id, query, &cancelled, &catalog, started,
    );
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    state
        .analytics
        .metrics
        .last_query_duration_ms
        .store(elapsed_ms, Ordering::Relaxed);
    deliver_async_result(&state, session_id, &group, &query_id, &cancelled, result);
}

fn execute_async_query(
    state: &ServerState,
    session_id: SessionId,
    query_id: &str,
    query: ActivitySummaryQuery,
    cancelled: &AtomicBool,
    catalog: &crate::storage::RecordingCatalogHandle,
    started: Instant,
) -> Result<Vec<OutboundDataMessage>, ControlCommandError> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    let summary = catalog
        .query_activity_summary(query)
        .map_err(map_query_error)?;
    // Re-verify session authorization after async catalog execution before sending bytes.
    super::camera_access::for_session(state, session_id)?;
    if started.elapsed() > QUERY_DEADLINE {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Timeout,
            504,
            "activity analytics query exceeded 5 second deadline",
        ));
    }
    encode_summary_chunks(query_id, summary)
}

fn deliver_async_result(
    state: &ServerState,
    session_id: SessionId,
    group: &str,
    query_id: &str,
    cancelled: &Arc<AtomicBool>,
    result: Result<Vec<OutboundDataMessage>, ControlCommandError>,
) {
    let messages = match result {
        Ok(messages) => messages,
        Err(error) if !cancelled.load(Ordering::Acquire) => {
            state
                .analytics
                .metrics
                .query_errors_total
                .fetch_add(1, Ordering::Relaxed);
            vec![analytics_data_message(
                group,
                proto::activity_analytics_message::Message::Error(proto::ActivityAnalyticsError {
                    query_id: query_id.to_owned(),
                    code: error.code as i32,
                    message: error.message,
                }),
            )]
        }
        Err(_) => Vec::new(),
    };
    for message in messages {
        if cancelled.load(Ordering::Acquire)
            || state
                .webrtc
                .enqueue_api_data(session_id, message, cancelled.clone())
                .is_err()
        {
            break;
        }
    }
    let cleanup_state = state.clone();
    let cleanup_query_id = query_id.to_owned();
    let _ = state.webrtc.complete_api_data_group(
        session_id,
        group.to_owned(),
        Box::new(move || {
            release_slot(&cleanup_state, session_id, &cleanup_query_id);
        }),
    );
}

fn build_domain_query(
    state: &ServerState,
    principal: &ApiPrincipal,
    access: &CameraAccess,
    request: &proto::QueryActivitySummary,
) -> Result<ActivitySummaryQuery, ControlCommandError> {
    let cfg = state.analytics.config();
    let (start_time_ms, end_time_ms) =
        proto_map::parse_bounds(request.start_time, request.end_time)?;
    let granularity = proto_map::parse_granularity(request.granularity)?;
    let week_start = proto_map::parse_week_start(request.week_start, cfg.week_start)?;
    let comparison = proto_map::parse_comparison(request.comparison)?;
    let filter = proto_map::parse_filter(request.filter.clone(), principal)?;
    let group_by = request
        .group_by
        .iter()
        .copied()
        .map(proto_map::parse_dimension)
        .collect::<Result<Vec<_>, _>>()?;
    let timezone = if request.timezone.trim().is_empty() {
        cfg.timezone.clone()
    } else {
        request.timezone.clone()
    };
    let (authorized_cameras, authorized_groups) = authorized_inventory(state, access);
    let mut query = ActivitySummaryQuery {
        start_time_ms,
        end_time_ms,
        timezone,
        granularity,
        week_start,
        filter,
        group_by,
        comparison,
        dense_buckets: request.dense_buckets,
        max_buckets: if request.max_buckets == 0 {
            MAX_BUCKETS
        } else {
            usize::try_from(request.max_buckets).unwrap_or(MAX_BUCKETS + 1)
        },
        max_series: if request.max_series == 0 {
            MAX_SERIES
        } else {
            usize::try_from(request.max_series).unwrap_or(MAX_SERIES + 1)
        },
        max_ranked_rows: if request.max_ranked_rows == 0 {
            MAX_RANKED_ROWS
        } else {
            usize::try_from(request.max_ranked_rows).unwrap_or(MAX_RANKED_ROWS + 1)
        },
        include_heatmap: request.include_heatmap,
        now_ms: i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
        authorized_cameras,
        authorized_groups,
        group_policy_revision: 1,
    };
    query.validate().map_err(|error| {
        ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string())
    })?;
    Ok(query)
}

fn authorized_inventory(
    state: &ServerState,
    access: &CameraAccess,
) -> (
    BTreeMap<String, AuthorizedCamera>,
    BTreeMap<String, AuthorizedGroup>,
) {
    let mut cameras = BTreeMap::new();
    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in state.camera_entries() {
        if !access.allows(&entry.info.id) {
            continue;
        }
        let id = entry.info.id.clone();
        let display_name = entry.info.name.clone().unwrap_or_else(|| id.clone());
        for group_id in &entry.groups {
            groups
                .entry(group_id.clone())
                .or_default()
                .insert(id.clone());
        }
        cameras.insert(
            id.clone(),
            AuthorizedCamera {
                camera_id: id,
                display_name,
                group_ids: entry.groups,
            },
        );
    }
    let authorized_groups = groups
        .into_iter()
        .map(|(group_id, camera_ids)| {
            (
                group_id.clone(),
                AuthorizedGroup {
                    display_name: group_id.clone(),
                    group_id,
                    camera_ids,
                },
            )
        })
        .collect();
    (cameras, authorized_groups)
}

fn encode_summary_chunks(
    query_id: &str,
    summary: crate::storage::analytics::ActivitySummaryResult,
) -> Result<Vec<OutboundDataMessage>, ControlCommandError> {
    let response = proto_map::proto_summary(query_id.to_owned(), summary);
    let bytes = response.encode_to_vec();
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            413,
            format!("activity summary response exceeds {MAX_RESPONSE_BYTES} bytes"),
        ));
    }
    let group = format!("activity-analytics:{query_id}");
    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![&[]]
    } else {
        bytes.chunks(CHUNK_BYTES).collect()
    };
    let chunk_count = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, slice)| {
            analytics_data_message(
                &group,
                proto::activity_analytics_message::Message::Chunk(proto::ActivityAnalyticsChunk {
                    query_id: query_id.to_owned(),
                    chunk_index: u32::try_from(index).unwrap_or(u32::MAX),
                    chunk_count,
                    payload: slice.to_vec(),
                }),
            )
        })
        .collect())
}

fn analytics_data_message(
    group: &str,
    message: proto::activity_analytics_message::Message,
) -> OutboundDataMessage {
    OutboundDataMessage {
        permit: None,
        target: DataChannelTarget::Reliable,
        group: group.to_owned(),
        message: proto::Message {
            message: Some(proto::message::Message::ActivityAnalytics(
                proto::ActivityAnalyticsMessage {
                    message: Some(message),
                },
            )),
        },
    }
}

fn reserve_slot(
    state: &ServerState,
    session_id: SessionId,
    principal_id: &str,
    query_id: &str,
) -> Result<Arc<AtomicBool>, ControlCommandError> {
    let mut active = state
        .analytics
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (session_id, query_id.to_owned());
    if active.contains_key(&key) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            409,
            "activity analytics query ID is already active",
        ));
    }
    if active.len() >= MAX_ACTIVE_GLOBAL {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "global activity analytics concurrency limit reached",
        ));
    }
    let principal_count = active
        .values()
        .filter(|entry| entry.principal_id == principal_id)
        .count();
    if principal_count >= MAX_ACTIVE_PER_PRINCIPAL {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "per-principal activity analytics concurrency limit reached",
        ));
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    active.insert(
        key,
        ActiveQuery {
            principal_id: principal_id.to_owned(),
            cancelled: cancelled.clone(),
        },
    );
    Ok(cancelled)
}

fn cancel_query(state: &ServerState, session_id: SessionId, query_id: &str) {
    if let Some(entry) = state
        .analytics
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(session_id, query_id.to_owned()))
    {
        entry.cancelled.store(true, Ordering::Release);
    }
}

fn release_slot(state: &ServerState, session_id: SessionId, query_id: &str) {
    state
        .analytics
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&(session_id, query_id.to_owned()));
}

fn trigger_rebuild(
    state: &ServerState,
) -> Result<crate::storage::analytics::ActivityAnalyticsStatus, ControlCommandError> {
    let catalog = event_search_catalog(state)?;
    let cfg = state.analytics.config();
    if state
        .analytics
        .rebuilding
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return catalog
            .analytics_status(&cfg.timezone, cfg.week_start)
            .map_err(|error| unavailable("read activity analytics status", error));
    }
    let status = match catalog.start_analytics_rebuild() {
        Ok(mut status) => {
            status.site_timezone = cfg.timezone.clone();
            status.week_start = cfg.week_start;
            status
        }
        Err(error) => {
            state.analytics.rebuilding.store(false, Ordering::Release);
            return Err(unavailable("start activity analytics rebuild", error));
        }
    };
    state
        .analytics
        .metrics
        .rebuild_runs_total
        .fetch_add(1, Ordering::Relaxed);
    let worker_flag = state.analytics.rebuilding.clone();
    let worker_catalog = catalog.clone();
    let spawn = std::thread::Builder::new()
        .name("activity-analytics-rebuild".to_owned())
        .spawn(move || {
            let mut batches = 0usize;
            const MAX_BACKGROUND_BATCHES: usize = 50_000;
            while batches < MAX_BACKGROUND_BATCHES {
                batches += 1;
                match worker_catalog.step_analytics_rebuild(MAX_REBUILD_BATCH_ROWS) {
                    Ok(true) => break,
                    Ok(false) => continue,
                    Err(error) => {
                        tracing::warn!(%error, "activity analytics rebuild failed");
                        break;
                    }
                }
            }
            worker_flag.store(false, Ordering::Release);
        });
    if let Err(error) = spawn {
        state.analytics.rebuilding.store(false, Ordering::Release);
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to spawn analytics rebuild worker: {error}"),
        ));
    }
    Ok(status)
}

fn map_query_error(error: anyhow::Error) -> ControlCommandError {
    let text = error.to_string();
    if text.contains("exceeds") || text.contains("limit") {
        ControlCommandError::new(proto::ErrorCode::Rejected, 400, text)
    } else if text.contains("degraded") || text.contains("unavailable") || text.contains("rebuild")
    {
        ControlCommandError::new(proto::ErrorCode::Unavailable, 503, text)
    } else {
        ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, text)
    }
}

fn unavailable(action: &str, error: anyhow::Error) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        format!("unable to {action}: {error}"),
    )
}
