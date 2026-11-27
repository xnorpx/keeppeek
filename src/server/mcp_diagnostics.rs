//! Bounded read-only diagnostic handlers for the KeepPeek MCP server.
//!
//! Each handler validates caller authorization, enforces strict input and
//! output bounds, and strips credentials or secret values from responses.

use super::{
    ApiPrincipal, ControlCommandError, DiscoveredCameraSettings, ServerState, camera_access,
    camera_discovery, current_config, event_search_catalog, peek_layouts,
    present_discovered_cameras, recording_coverage, runtime_configuration, server_health,
    state_store, storage_archive, storage_volumes, unix_time_ms,
};
use crate::{
    access::{AccessRole, CameraAccess, MAX_CAMERA_ACCESS_IDS},
    api::proto::{
        self, archive_command, archive_result, ok as control_ok, runtime_configuration_command,
        state_store_command, state_store_result, storage_volume_command, storage_volume_result,
    },
    runtime::{FacadeSender, RouterMessage},
    storage::{
        DEFAULT_PREVIEW_AFTER_MS, DEFAULT_PREVIEW_BEFORE_MS, EventImageFilter, EventMetadataQuery,
        EventSearch, EventSearchHit,
    },
    webrtc::SessionId,
};
use serde_json::{Map, Value, json};
use std::{
    io::Read as _,
    net::Ipv4Addr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const EVENT_SEARCH_LIMIT_DEFAULT: usize = 25;
const EVENT_SEARCH_LIMIT_MAX: usize = 100;
const EVENT_SEARCH_WINDOW_DEFAULT_MS: i64 = 86_400_000;
const EVENT_SEARCH_WINDOW_MAX_MS: i64 = 31 * 86_400_000;
const EVENT_SEARCH_QUERY_BYTES_MAX: usize = 256;
const EVENT_SEARCH_PAGE_TOKEN_BYTES_MAX: usize = 4_096;
const EVENT_SEARCH_TYPES_MAX: usize = 32;
const COVERAGE_PAGE_SIZE_DEFAULT: usize = 25;
const COVERAGE_PAGE_SIZE_MAX: usize = 50;
const COVERAGE_SEGMENTS_PER_STREAM_DEFAULT: usize = 64;
const COVERAGE_SEGMENTS_PER_STREAM_MAX: usize = 256;
const COVERAGE_WINDOW_DEFAULT_MS: u64 = 86_400_000;
const COVERAGE_WINDOW_MAX_MS: u64 = 31 * 86_400_000;
const COVERAGE_RESPONSE_BYTES_MAX: u64 = 4 * 1_024 * 1_024;
const DISCOVERY_TIMEOUT_SECONDS_DEFAULT: u64 = 5;
const DISCOVERY_TIMEOUT_SECONDS_MAX: u64 = 10;
const DISCOVERY_RESULT_LIMIT_DEFAULT: usize = 50;
const DISCOVERY_RESULT_LIMIT_MAX: usize = 100;
const DISCOVERY_NETWORKS_MAX: usize = 8;
const DISCOVERY_SUBNETS_MAX: usize = 8;
const NOTIFICATION_RULES_MAX: usize = 128;
const ARCHIVE_JOBS_LIMIT: u32 = 16;
const MAINTENANCE_JOBS_MAX: usize = 16;

const _: () = {
    assert!(EVENT_SEARCH_LIMIT_DEFAULT >= 1);
    assert!(EVENT_SEARCH_LIMIT_DEFAULT <= EVENT_SEARCH_LIMIT_MAX);
    assert!(COVERAGE_PAGE_SIZE_DEFAULT >= 1);
    assert!(COVERAGE_PAGE_SIZE_DEFAULT <= COVERAGE_PAGE_SIZE_MAX);
    assert!(COVERAGE_SEGMENTS_PER_STREAM_DEFAULT <= COVERAGE_SEGMENTS_PER_STREAM_MAX);
    assert!(DISCOVERY_TIMEOUT_SECONDS_DEFAULT <= DISCOVERY_TIMEOUT_SECONDS_MAX);
    assert!(DISCOVERY_RESULT_LIMIT_DEFAULT <= DISCOVERY_RESULT_LIMIT_MAX);
};

/// Searches timeline events within caller camera permissions and strict result bounds.
pub(super) fn search_events(
    state: &ServerState,
    _router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    args: &Value,
) -> Result<Value, ControlCommandError> {
    let map = args_object(args)?;
    let access = camera_access::for_principal(state, principal)?;
    let source_ids = resolve_event_search_sources(state, &access, map)?;
    let (start_ms, end_ms) = parse_event_window(map)?;
    let limit = clamp_usize(
        optional_u64(map, "limit")?,
        EVENT_SEARCH_LIMIT_DEFAULT,
        1,
        EVENT_SEARCH_LIMIT_MAX,
    );
    let query = build_event_metadata_query(map, source_ids, start_ms, end_ms, limit)?;
    if query.source_ids.is_empty() && !access.all_cameras {
        return Ok(empty_event_search_response(start_ms, end_ms, limit));
    }
    let catalog = event_search_catalog(state)?;
    let page = EventSearch::new(catalog.clone())
        .search_metadata(query)
        .map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("event search failed: {error}"),
            )
        })?;
    // Enforce camera authorization a second time on returned hits to prevent leaks.
    let events = page
        .hits
        .into_iter()
        .filter(|hit| access.allows(&hit.source_id))
        .take(limit)
        .map(event_hit_json)
        .collect::<Vec<_>>();
    Ok(json!({
        "start_ms": start_ms,
        "end_ms": end_ms,
        "limit": limit,
        "count": events.len(),
        "candidates_truncated": page.candidates_truncated,
        "next_page_token": page.next_page_token,
        "events": events,
    }))
}

fn build_event_metadata_query(
    map: &Map<String, Value>,
    source_ids: Vec<String>,
    start_ms: i64,
    end_ms: i64,
    limit: usize,
) -> Result<EventMetadataQuery, ControlCommandError> {
    Ok(EventMetadataQuery {
        event_ids: Vec::new(),
        source_ids,
        event_types: optional_string_list(map, "event_types", EVENT_SEARCH_TYPES_MAX)?,
        origins: Vec::new(),
        zones: Vec::new(),
        include_missing_zone: false,
        minimum_confidence: None,
        confidence_bands: Vec::new(),
        image: EventImageFilter::Any,
        text: parse_optional_text(map)?,
        stream_id: parse_stream_id(map)?,
        start_time_ms: start_ms,
        end_time_ms: end_ms,
        preview_before_ms: DEFAULT_PREVIEW_BEFORE_MS,
        preview_after_ms: DEFAULT_PREVIEW_AFTER_MS,
        page_size: u32::try_from(limit).unwrap_or(100),
        page_token: parse_optional_page_token(map)?,
        include_preview_keyframes: false,
        workflow: None,
    })
}

/// Returns bounded recording coverage and gap diagnostics for authorized cameras.
pub(super) fn get_recording_coverage(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    args: &Value,
) -> Result<Value, ControlCommandError> {
    let map = args_object(args)?;
    let access = camera_access::for_principal(state, principal)?;
    let requested_camera = optional_nonempty_str(map, "camera_id")?;
    if let Some(camera_id) = requested_camera {
        camera_access::require_camera(&access, camera_id)?;
    }
    let query_string = build_coverage_query_string(map, requested_camera)?;
    let max_segments = clamp_usize(
        optional_u64(map, "max_segments")?,
        COVERAGE_SEGMENTS_PER_STREAM_DEFAULT,
        1,
        COVERAGE_SEGMENTS_PER_STREAM_MAX,
    );
    let request = rouille::Request::fake_http(
        "GET",
        format!("/api/recording-coverage?{query_string}"),
        Vec::new(),
        Vec::new(),
    );
    let response = recording_coverage::get(&request, router_tx, state, principal);
    let mut payload = read_rouille_json(response)?;
    if let Some(camera_id) = requested_camera {
        filter_coverage_to_camera(&mut payload, camera_id);
    }
    cap_coverage_segments(&mut payload, max_segments);
    Ok(payload)
}

/// Returns Administrator storage configuration, pressure, named volumes, archive, and maintenance status.
pub(super) fn get_storage_status(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    args: &Value,
) -> Result<Value, ControlCommandError> {
    require_admin(principal)?;
    let _ = args_object(args)?;
    let config = current_config(state);
    let health = server_health(router_tx, state);
    let volumes = storage_volumes_summary(state, principal);
    let archive = storage_archive_summary(state, principal);
    let maintenance = recording_maintenance_summary(state, principal);
    Ok(json!({
        "configuration": config.storage,
        "recording_estimate": config.recording_estimate,
        "health": health.storage,
        "named_volumes": volumes,
        "archive": archive,
        "recording_maintenance": maintenance,
    }))
}

/// Returns Administrator notification, cloud signaling, and MQTT delivery status without secrets.
pub(super) fn get_notification_status(
    state: &ServerState,
    _router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    args: &Value,
) -> Result<Value, ControlCommandError> {
    require_admin(principal)?;
    let _ = args_object(args)?;
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    Ok(json!({
        "rules": notification_rules_summary(state, principal),
        "cloud_notifications": cloud_notifications_summary(state, now_ms),
        "mqtt_forwarder": mqtt_forwarder_summary(state),
    }))
}

/// Runs bounded Administrator camera discovery with strict timeout and result-count limits.
pub(super) fn discover_cameras(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    args: &Value,
) -> Result<Value, ControlCommandError> {
    require_admin(principal)?;
    let map = args_object(args)?;
    let list_networks_only = optional_bool(map, "list_networks_only")?.unwrap_or(false);
    let networks = parse_discovery_networks(map)?;
    let subnets = parse_discovery_subnets(map)?;
    let timeout_secs = clamp_u64(
        optional_u64(map, "timeout_seconds")?,
        DISCOVERY_TIMEOUT_SECONDS_DEFAULT,
        1,
        DISCOVERY_TIMEOUT_SECONDS_MAX,
    );
    let limit = clamp_usize(
        optional_u64(map, "limit")?,
        DISCOVERY_RESULT_LIMIT_DEFAULT,
        1,
        DISCOVERY_RESULT_LIMIT_MAX,
    );
    if list_networks_only {
        return Ok(discovery_networks_json(state));
    }
    run_bounded_discovery(state, router_tx, networks, subnets, timeout_secs, limit)
}

/// Returns the Administrator runtime configuration via the existing runtime configuration handler.
pub(super) fn get_runtime_configuration(
    state: &ServerState,
    _router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    args: &Value,
) -> Result<Value, ControlCommandError> {
    require_admin(principal)?;
    let _ = args_object(args)?;
    let result = runtime_configuration::dispatch(
        state,
        proto::RuntimeConfigurationCommand {
            action: Some(runtime_configuration_command::Action::Get(
                proto::GetRuntimeConfiguration {},
            )),
        },
    )?;
    let control_ok::Result::RuntimeConfigurationResult(response) = result else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "runtime configuration response was unexpected",
        ));
    };
    let config = response.config.ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "runtime configuration response is missing configuration data",
        )
    })?;
    Ok(runtime_configuration_json(
        &config,
        response.restart_required,
    ))
}

/// Lists Administrator runtime profiles and active status from the state-store profiles adapter.
pub(super) fn list_runtime_profiles(
    state: &ServerState,
    _router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    args: &Value,
) -> Result<Value, ControlCommandError> {
    require_admin(principal)?;
    let _ = args_object(args)?;
    if state.camera_config_path.is_some() {
        let registry = dispatch_profile_get(state, principal, "registry")?;
        let active = dispatch_profile_get(state, principal, "active")?;
        return Ok(json!({
            "source": "state_store_profiles",
            "active": active,
            "registry": registry,
        }));
    }
    Ok(in_memory_runtime_profiles_json(state))
}

fn require_admin(principal: &ApiPrincipal) -> Result<(), ControlCommandError> {
    if principal.role == AccessRole::Administrator {
        return Ok(());
    }
    Err(ControlCommandError::new(
        proto::ErrorCode::Rejected,
        403,
        "Administrator access is required",
    ))
}

fn args_object(args: &Value) -> Result<&Map<String, Value>, ControlCommandError> {
    if args.is_null() {
        static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
        return Ok(&EMPTY);
    }
    args.as_object().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "tool arguments must be a JSON object",
        )
    })
}

fn optional_u64(map: &Map<String, Value>, key: &str) -> Result<Option<u64>, ControlCommandError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value.as_u64().map(Some).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{key} must be an unsigned integer"),
        )
    })
}

fn optional_i64(map: &Map<String, Value>, key: &str) -> Result<Option<i64>, ControlCommandError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value.as_i64().map(Some).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{key} must be an integer"),
        )
    })
}

fn optional_bool(map: &Map<String, Value>, key: &str) -> Result<Option<bool>, ControlCommandError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value.as_bool().map(Some).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{key} must be a boolean"),
        )
    })
}

fn optional_nonempty_str<'a>(
    map: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, ControlCommandError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value.as_str().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{key} must be a string"),
        )
    })?;
    let trimmed = text.trim();
    Ok((!trimmed.is_empty()).then_some(trimmed))
}

fn optional_string_list(
    map: &Map<String, Value>,
    key: &str,
    limit: usize,
) -> Result<Vec<String>, ControlCommandError> {
    let Some(value) = map.get(key) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let items = value.as_array().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{key} must be an array of strings"),
        )
    })?;
    if items.len() > limit {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{key} cannot contain more than {limit} entries"),
        ));
    }
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        let text = item
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    format!("{key} entries must be non-empty strings"),
                )
            })?;
        values.push(text.to_owned());
    }
    Ok(values)
}

fn clamp_usize(value: Option<u64>, default: usize, min: usize, max: usize) -> usize {
    assert!(
        min <= default && default <= max,
        "clamp bounds must be ordered"
    );
    value.map_or(default, |raw| {
        usize::try_from(raw).unwrap_or(usize::MAX).clamp(min, max)
    })
}

fn clamp_u64(value: Option<u64>, default: u64, min: u64, max: u64) -> u64 {
    assert!(
        min <= default && default <= max,
        "clamp bounds must be ordered"
    );
    value.map_or(default, |raw| raw.clamp(min, max))
}

fn resolve_event_search_sources(
    state: &ServerState,
    access: &CameraAccess,
    map: &Map<String, Value>,
) -> Result<Vec<String>, ControlCommandError> {
    let mut requested = optional_string_list(map, "camera_ids", MAX_CAMERA_ACCESS_IDS)?;
    requested.extend(optional_string_list(
        map,
        "source_ids",
        MAX_CAMERA_ACCESS_IDS,
    )?);
    if let Some(camera_id) = optional_nonempty_str(map, "camera_id")? {
        requested.push(camera_id.to_owned());
    }
    if let Some(source_id) = optional_nonempty_str(map, "source_id")? {
        requested.push(source_id.to_owned());
    }
    requested.sort_unstable();
    requested.dedup();
    if !requested.is_empty() {
        camera_access::require_cameras(access, &requested)?;
        return Ok(requested);
    }
    if access.all_cameras {
        return Ok(Vec::new());
    }
    let mut allowed = access.camera_ids.clone();
    allowed.extend(
        camera_access::query_cameras(state, access, &[])?
            .into_iter()
            .map(|camera| camera.info.id),
    );
    allowed.sort_unstable();
    allowed.dedup();
    Ok(allowed)
}

fn parse_stream_id(map: &Map<String, Value>) -> Result<String, ControlCommandError> {
    let stream = optional_nonempty_str(map, "stream_id")?.unwrap_or("main");
    if matches!(stream, "main" | "sub") {
        return Ok(stream.to_owned());
    }
    Err(ControlCommandError::new(
        proto::ErrorCode::InvalidRequest,
        400,
        "stream_id must be main or sub",
    ))
}

fn parse_event_window(map: &Map<String, Value>) -> Result<(i64, i64), ControlCommandError> {
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let end_ms = optional_i64(map, "end_ms")?.unwrap_or(now_ms);
    let start_ms = optional_i64(map, "start_ms")?
        .unwrap_or_else(|| end_ms.saturating_sub(EVENT_SEARCH_WINDOW_DEFAULT_MS));
    if start_ms >= end_ms {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "start_ms must precede end_ms",
        ));
    }
    if end_ms.saturating_sub(start_ms) > EVENT_SEARCH_WINDOW_MAX_MS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search window cannot exceed 31 days",
        ));
    }
    Ok((start_ms, end_ms))
}

fn parse_optional_text(map: &Map<String, Value>) -> Result<Option<String>, ControlCommandError> {
    let text = optional_nonempty_str(map, "text")?.or(optional_nonempty_str(map, "query")?);
    if let Some(value) = text {
        if value.len() > EVENT_SEARCH_QUERY_BYTES_MAX {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "event search text cannot exceed 256 UTF-8 bytes",
            ));
        }
        return Ok(Some(value.to_owned()));
    }
    Ok(None)
}

fn parse_optional_page_token(
    map: &Map<String, Value>,
) -> Result<Option<String>, ControlCommandError> {
    let Some(token) = optional_nonempty_str(map, "page_token")? else {
        return Ok(None);
    };
    if token.len() > EVENT_SEARCH_PAGE_TOKEN_BYTES_MAX {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search page_token exceeds size limit",
        ));
    }
    Ok(Some(token.to_owned()))
}

fn empty_event_search_response(start_ms: i64, end_ms: i64, limit: usize) -> Value {
    json!({
        "start_ms": start_ms,
        "end_ms": end_ms,
        "limit": limit,
        "count": 0,
        "candidates_truncated": false,
        "next_page_token": null,
        "events": [],
    })
}

fn event_hit_json(hit: EventSearchHit) -> Value {
    json!({
        "event_id": hit.event_id,
        "revision": hit.revision,
        "camera_id": hit.source_id,
        "event_type": hit.event_type,
        "origin": hit.origin,
        "start_time_ms": hit.start_time_ms,
        "end_time_ms": hit.end_time_ms,
        "confidence": hit.confidence,
        "zone": hit.zone,
        "text": hit.text,
        "has_image_attachment": hit.has_image_attachment,
        "image_available": hit.image_available,
        "score": hit.score,
    })
}

fn build_coverage_query_string(
    map: &Map<String, Value>,
    requested_camera: Option<&str>,
) -> Result<String, ControlCommandError> {
    let now_ms = unix_time_ms();
    let end_ms = optional_u64(map, "end_ms")?.unwrap_or(now_ms);
    let start_ms = optional_u64(map, "start_ms")?
        .unwrap_or_else(|| end_ms.saturating_sub(COVERAGE_WINDOW_DEFAULT_MS));
    if start_ms >= end_ms {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "recording coverage start_ms must precede end_ms",
        ));
    }
    if end_ms.saturating_sub(start_ms) > COVERAGE_WINDOW_MAX_MS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "recording coverage window cannot exceed 31 days",
        ));
    }
    let page_size = clamp_usize(
        optional_u64(map, "limit")?.or(optional_u64(map, "page_size")?),
        COVERAGE_PAGE_SIZE_DEFAULT,
        1,
        COVERAGE_PAGE_SIZE_MAX,
    );
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("start_ms", &start_ms.to_string());
    serializer.append_pair("end_ms", &end_ms.to_string());
    serializer.append_pair("page_size", &page_size.to_string());
    if let Some(minimum_gap_ms) = optional_u64(map, "minimum_gap_ms")? {
        serializer.append_pair("minimum_gap_ms", &minimum_gap_ms.to_string());
    }
    if let Some(stream) = optional_nonempty_str(map, "stream")? {
        serializer.append_pair("stream", stream);
    }
    if let Some(state_filter) = optional_nonempty_str(map, "state")? {
        serializer.append_pair("state", state_filter);
    }
    if let Some(group) = optional_nonempty_str(map, "group")? {
        serializer.append_pair("group", group);
    }
    let search = requested_camera.or(optional_nonempty_str(map, "search")?);
    if let Some(search) = search {
        serializer.append_pair("search", search);
    }
    Ok(serializer.finish())
}

fn read_rouille_json(response: rouille::Response) -> Result<Value, ControlCommandError> {
    let status = response.status_code;
    let (reader, _) = response.data.into_reader_and_size();
    let mut body = Vec::new();
    reader
        .take(COVERAGE_RESPONSE_BYTES_MAX + 1)
        .read_to_end(&mut body)
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                "failed to read recording coverage response",
            )
        })?;
    if u64::try_from(body.len()).unwrap_or(u64::MAX) > COVERAGE_RESPONSE_BYTES_MAX {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            413,
            "recording coverage response exceeds size bound",
        ));
    }
    if status >= 400 {
        let message = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("recording coverage failed with status {status}"));
        let code = match status {
            400 => proto::ErrorCode::InvalidRequest,
            403 | 409 => proto::ErrorCode::Rejected,
            404 => proto::ErrorCode::NotFound,
            503 => proto::ErrorCode::Unavailable,
            _ => proto::ErrorCode::Internal,
        };
        return Err(ControlCommandError::new(code, status, message));
    }
    serde_json::from_slice(&body).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "recording coverage returned invalid JSON",
        )
    })
}

fn filter_coverage_to_camera(payload: &mut Value, camera_id: &str) {
    if let Some(cameras) = payload.get_mut("cameras").and_then(Value::as_array_mut) {
        cameras.retain(|camera| {
            camera
                .get("camera_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id == camera_id)
        });
    }
    if let Some(findings) = payload.get_mut("findings").and_then(Value::as_array_mut) {
        findings.retain(|finding| {
            finding
                .get("camera_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id == camera_id)
        });
    }
}

fn cap_coverage_segments(payload: &mut Value, max_segments: usize) {
    let Some(cameras) = payload.get_mut("cameras").and_then(Value::as_array_mut) else {
        return;
    };
    for camera in cameras {
        let Some(streams) = camera.get_mut("streams").and_then(Value::as_array_mut) else {
            continue;
        };
        for stream in streams {
            let mut truncated = false;
            for field in ["ranges", "gaps"] {
                if let Some(entries) = stream.get_mut(field).and_then(Value::as_array_mut)
                    && entries.len() > max_segments
                {
                    entries.truncate(max_segments);
                    truncated = true;
                }
            }
            if truncated && let Some(object) = stream.as_object_mut() {
                object.insert("detail_truncated".to_owned(), Value::Bool(true));
            }
        }
    }
}

fn storage_volumes_summary(state: &ServerState, principal: &ApiPrincipal) -> Value {
    let command = proto::StorageVolumeCommand {
        action: Some(storage_volume_command::Action::List(
            proto::ListStorageVolumes {},
        )),
    };
    match storage_volumes::management::dispatch(state, principal, command) {
        Ok(control_ok::Result::StorageVolumeResult(proto::StorageVolumeResult {
            result: Some(storage_volume_result::Result::Volumes(list)),
        })) => {
            let volumes = list
                .volumes
                .into_iter()
                .map(|volume| {
                    json!({
                        "volume_id": volume.volume_id,
                        "online": volume.online,
                        "available_bytes": volume.available_bytes,
                        "owned_bytes": volume.owned_bytes,
                        "reserved_bytes": volume.reserved_bytes,
                        "configured_draining": volume.configured_draining,
                        "operator_draining": volume.operator_draining,
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "available": true,
                "runtime_available": list.runtime_available,
                "configuration_revision": list.configuration_revision,
                "volumes": volumes,
            })
        }
        Ok(_) => json!({ "available": false, "detail": "unexpected volume status reply" }),
        Err(error) => json!({ "available": false, "detail": error.message }),
    }
}

fn storage_archive_summary(state: &ServerState, principal: &ApiPrincipal) -> Value {
    let command = proto::ArchiveCommand {
        action: Some(archive_command::Action::List(proto::ListArchiveJobs {
            after_job_id: String::new(),
            limit: ARCHIVE_JOBS_LIMIT,
        })),
    };
    match storage_archive::dispatch(state, principal, command) {
        Ok(control_ok::Result::ArchiveResult(proto::ArchiveResult {
            result: Some(archive_result::Result::Jobs(list)),
        })) => {
            let status = list.status.map(|status| {
                json!({
                    "runtime_available": status.runtime_available,
                    "online": status.online,
                    "error_code": status.error_code,
                    "probe_pending": status.probe_pending,
                })
            });
            let jobs = list
                .jobs
                .into_iter()
                .map(|job| {
                    json!({
                        "job_id": job.job_id,
                        "object_id": job.object_id,
                        "target_id": job.target_id,
                        "state": job.state,
                        "bytes": job.bytes,
                        "attempts": job.attempts,
                        "error_code": job.error_code,
                        "action": job.action,
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "available": true,
                "status": status,
                "recent_jobs": jobs,
                "has_more": !list.next_after_job_id.is_empty(),
            })
        }
        Ok(_) => json!({ "available": false, "detail": "unexpected archive status reply" }),
        Err(error) => json!({ "available": false, "detail": error.message }),
    }
}

fn recording_maintenance_summary(state: &ServerState, principal: &ApiPrincipal) -> Value {
    let active = state.maintenance_active.load(Ordering::Acquire);
    let Some(catalog) = state.catalog.as_ref() else {
        return json!({
            "active": active,
            "catalog_available": false,
            "recent_jobs": [],
        });
    };
    let jobs = catalog
        .recording_deletion_jobs(&principal.id(), "")
        .map_or_else(
            |_| Vec::new(),
            |jobs| {
                jobs.into_iter()
                    .take(MAINTENANCE_JOBS_MAX)
                    .map(|job| {
                        json!({
                            "job_id": job.id,
                            "state": format!("{:?}", job.state),
                            "reason": format!("{:?}", job.reason),
                            "recording_count": job.snapshot.recordings.len(),
                        })
                    })
                    .collect::<Vec<_>>()
            },
        );
    json!({
        "active": active,
        "catalog_available": true,
        "recent_jobs": jobs,
    })
}

fn notification_rules_summary(state: &ServerState, principal: &ApiPrincipal) -> Value {
    let Some(handle) = state.notifications.as_ref() else {
        return json!({
            "available": false,
            "configured_rules": 0,
            "rules": [],
        });
    };
    let metrics = handle.metric_snapshot();
    let (pushover_configured, webhook_configured) = handle.configured_channel_flags();
    let rules = handle
        .rules(principal.id())
        .unwrap_or_default()
        .into_iter()
        .take(NOTIFICATION_RULES_MAX)
        .map(sanitized_notification_rule)
        .collect::<Vec<_>>();
    json!({
        "available": true,
        "pushover_configured": pushover_configured,
        "webhook_configured": webhook_configured,
        "metrics": {
            "configured_rules": metrics.configured_rules,
            "pending_deliveries": metrics.pending_deliveries,
            "candidates_accepted": metrics.candidates_accepted,
            "candidates_dropped": metrics.candidates_dropped,
            "notifications_created": metrics.notifications_created,
            "notifications_replaced": metrics.notifications_replaced,
            "notifications_suppressed": metrics.notifications_suppressed,
            "delivery_attempts": metrics.delivery_attempts,
            "delivery_retries": metrics.delivery_retries,
            "delivery_successes": metrics.delivery_successes,
            "delivery_failures": metrics.delivery_failures,
        },
        "rules": rules,
    })
}

fn sanitized_notification_rule(record: crate::notifications::RuleRecord) -> Value {
    let rule = record.active.as_ref().unwrap_or(&record.draft);
    // Omit action destinations so webhook URLs, Pushover tokens, and credentials never leak.
    let channels = rule
        .actions
        .iter()
        .map(|action| {
            json!({
                "channel": action.channel.as_str(),
                "enabled": action.enabled,
                "destination_configured": !action.destination.trim().is_empty(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "id": record.id,
        "name": rule.name,
        "active": record.active.is_some(),
        "enabled": rule.enabled,
        "active_revision": record.active_revision,
        "draft_revision": record.draft_revision,
        "triggers": rule.triggers,
        "channels": channels,
        "last_match_at_ms": record.last_match_at_ms,
        "last_delivery_at_ms": record.last_delivery_at_ms,
    })
}

fn cloud_notifications_summary(state: &ServerState, now_ms: i64) -> Value {
    let Some(cloud) = state.cloud_notifications.as_ref() else {
        return json!({ "available": false });
    };
    let settings = cloud.settings();
    let status = cloud.status(now_ms);
    json!({
        "available": true,
        "revision": settings.revision,
        "enabled": settings.enabled,
        "image_previews_enabled": settings.image_previews_enabled,
        "paired": status.paired,
        "state": format!("{:?}", status.state),
        "pending": status.pending,
        "accepted_last_hour": status.accepted_last_hour,
        "accepted_last_day": status.accepted_last_day,
        "suppressed_last_hour": status.suppressed_last_hour,
        "suppressed_last_day": status.suppressed_last_day,
        "next_eligible_at_ms": status.next_eligible_at_ms,
        "last_accepted_at_ms": status.last_accepted_at_ms,
        "last_failure": format!("{:?}", status.last_failure),
    })
}

fn mqtt_forwarder_summary(state: &ServerState) -> Value {
    let Some(forwarder) = state.event_forwarder.as_ref() else {
        return json!({ "available": false });
    };
    let config = forwarder.config();
    let status = forwarder.status();
    json!({
        "available": true,
        "enabled": config.enabled,
        "broker_endpoint": sanitize_broker_url(&config.broker_url),
        "client_id": config.client_id,
        "instance_id": config.instance_id,
        "forwarder_id": config.forwarder_id,
        "topic_prefix": config.topic_prefix,
        "username_configured": config.username.is_some(),
        "password_configured": config.password.is_some(),
        "tls_ca_configured": config.tls_ca_path.is_some(),
        "qos": config.qos,
        "retain_events": config.retain_events,
        "retain_health": config.retain_health,
        "outbox_max_mb": config.outbox_max_mb,
        "status": status,
    })
}

/// Removes userinfo, query parameters, and fragments from a broker URL.
pub(super) fn sanitize_broker_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Ok(parsed) = url::Url::parse(trimmed) else {
        return "[invalid-broker-url]".to_owned();
    };
    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or("unknown");
    parsed.port().map_or_else(
        || format!("{scheme}://{host}"),
        |port| format!("{scheme}://{host}:{port}"),
    )
}

fn parse_discovery_networks(
    map: &Map<String, Value>,
) -> Result<Vec<ipnet::Ipv4Net>, ControlCommandError> {
    let raw = optional_string_list(map, "networks", DISCOVERY_NETWORKS_MAX)?;
    let mut networks = Vec::with_capacity(raw.len());
    for entry in raw {
        let network = entry.parse::<ipnet::Ipv4Net>().map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "camera discovery networks must be IPv4 CIDRs",
            )
        })?;
        if network.prefix_len() != 24 || !network.network().is_private() {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "camera discovery networks must be private IPv4 /24 CIDRs",
            ));
        }
        networks.push(network);
    }
    Ok(networks)
}

fn parse_discovery_subnets(map: &Map<String, Value>) -> Result<Vec<u8>, ControlCommandError> {
    let Some(value) = map.get("subnets") else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let items = value.as_array().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "subnets must be an array of integers",
        )
    })?;
    if items.len() > DISCOVERY_SUBNETS_MAX {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("at most {DISCOVERY_SUBNETS_MAX} additional subnets may be scanned at once"),
        ));
    }
    let mut subnets = Vec::with_capacity(items.len());
    for item in items {
        let raw = item.as_u64().ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "subnets entries must be unsigned integers between 0 and 255",
            )
        })?;
        let subnet = u8::try_from(raw).map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "subnets entries must be between 0 and 255",
            )
        })?;
        subnets.push(subnet);
    }
    subnets.sort_unstable();
    subnets.dedup();
    Ok(subnets)
}

fn discovery_networks_json(state: &ServerState) -> Value {
    let configured_ips = state
        .camera_entries()
        .into_iter()
        .filter_map(|camera| camera.info.ip.parse::<Ipv4Addr>().ok());
    let local = crate::cameras::camera_discovery_networks().unwrap_or_default();
    let networks = camera_discovery::prefer_configured_networks(local, configured_ips)
        .into_iter()
        .take(DISCOVERY_RESULT_LIMIT_MAX)
        .map(|network| {
            json!({
                "cidr": network.cidr,
                "interface_name": network.interface_name,
                "preferred": network.preferred,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "list_networks_only": true,
        "networks": networks,
        "cameras": [],
        "total_discovered": 0,
        "truncated": false,
    })
}

struct DiscoveryTaskGuard {
    task: camera_discovery::TaskHandle,
}

impl Drop for DiscoveryTaskGuard {
    fn drop(&mut self) {
        let _ = self.task.finish();
    }
}

fn run_bounded_discovery(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    networks: Vec<ipnet::Ipv4Net>,
    subnets: Vec<u8>,
    timeout_secs: u64,
    limit: usize,
) -> Result<Value, ControlCommandError> {
    let discovery_id = format!("mcp-{}", uuid::Uuid::new_v4());
    let task = state
        .camera_discovery_tasks
        .start(SessionId::from_u64(0), &discovery_id)?;
    let guard = DiscoveryTaskGuard { task };
    let cancelled = guard.task.cancellation_token();
    let done = Arc::new(AtomicBool::new(false));
    let deadline_cancelled = cancelled.clone();
    let deadline_done = done.clone();
    let timeout = Duration::from_secs(timeout_secs);
    let _timer = thread::Builder::new()
        .name("mcp-discovery-deadline".to_owned())
        .spawn(move || {
            let start = Instant::now();
            while !deadline_done.load(Ordering::Acquire) {
                if start.elapsed() >= timeout {
                    deadline_cancelled.store(true, Ordering::Release);
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
    let discovered = if networks.is_empty() {
        crate::cameras::discover_with_progress(Some(timeout), &subnets, &cancelled, |cameras| {
            guard.task.update(cameras);
        })
    } else {
        crate::cameras::discover_on_networks_with_progress(
            Some(timeout),
            &networks,
            &cancelled,
            |cameras| guard.task.update(cameras),
        )
    };
    done.store(true, Ordering::Release);
    let discovered = discovered.map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            format!("camera discovery failed: {error}"),
        )
    })?;
    let presented = present_discovered_cameras(discovered, router_tx, state);
    let total_discovered = presented.len();
    let truncated = total_discovered > limit;
    let cameras = presented
        .into_iter()
        .take(limit)
        .map(discovered_camera_json)
        .collect::<Vec<_>>();
    Ok(json!({
        "discovery_id": discovery_id,
        "timeout_seconds": timeout_secs,
        "limit": limit,
        "total_discovered": total_discovered,
        "truncated": truncated,
        "timed_out": cancelled.load(Ordering::Acquire),
        "cameras": cameras,
    }))
}

fn discovered_camera_json(camera: DiscoveredCameraSettings) -> Value {
    json!({
        "ip": camera.ip,
        "brand": camera.brand,
        "name": camera.name,
        "model": camera.model,
        "onvif_port": camera.onvif_port,
        "sources": camera.sources,
        "configured": camera.configured,
        "health": camera.health,
    })
}

fn runtime_configuration_json(
    config: &proto::SanitizedRuntimeConfiguration,
    restart_required: bool,
) -> Value {
    let storage = config.storage.as_ref().map(|storage| {
        json!({
            "medium_term_path": storage.medium_term_path,
            "long_term_path": storage.long_term_path,
            "recording_catalog_path": storage.recording_catalog_path,
            "event_thumbnail_path": storage.event_thumbnail_path,
            "event_thumbnail_max_mb": storage.event_thumbnail_max_mb,
            "short_term_secs": storage.short_term_secs,
            "medium_term_secs": storage.medium_term_secs,
            "flush_interval_secs": storage.flush_interval_secs,
            "write_buffer_bytes": storage.write_buffer_bytes,
            "long_term_max_gb": storage.long_term_max_gb,
            "minimum_free_gb": storage.minimum_free_gb,
            "maximum_used_percent": storage.maximum_used_percent,
            "warning_free_gb": storage.warning_free_gb,
            "critical_free_gb": storage.critical_free_gb,
            "cleanup_hysteresis_gb": storage.cleanup_hysteresis_gb,
            "pre_recording_stream_max_bytes": storage.pre_recording_stream_max_bytes,
            "pre_recording_global_max_bytes": storage.pre_recording_global_max_bytes,
            "named_volumes_count": storage.named_volumes.as_ref().map_or(0, |volumes| volumes.volumes.len()),
            "archive_enabled": storage.archive.as_ref().is_some_and(|archive| archive.enabled),
        })
    });
    json!({
        "host": config.host,
        "port": config.port,
        "configuration_revision": config.configuration_revision,
        "camera_count": config.camera_count,
        "restart_required": restart_required,
        "storage": storage,
    })
}

fn dispatch_profile_get(
    state: &ServerState,
    principal: &ApiPrincipal,
    key: &str,
) -> Result<Value, ControlCommandError> {
    let result = state_store::dispatch(
        state,
        SessionId::from_u64(0),
        principal,
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Get(proto::GetState {
                namespace: state_store::profiles::NAMESPACE.to_owned(),
                key: key.to_owned(),
            })),
        },
    )?;
    let control_ok::Result::StateStoreResult(proto::StateStoreResult {
        result: Some(state_store_result::Result::Entry(entry)),
    }) = result
    else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "runtime profile response was unexpected",
        ));
    };
    let value = entry
        .value
        .map(peek_layouts::struct_to_json)
        .unwrap_or(Value::Null);
    Ok(json!({
        "key": entry.key,
        "schema": entry.schema,
        "revision": entry.revision,
        "value": value,
    }))
}

fn in_memory_runtime_profiles_json(state: &ServerState) -> Value {
    let registry = state
        .runtime_profiles
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let status = state
        .runtime_profile_status
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let profiles = registry
        .profiles
        .values()
        .map(|profile| {
            json!({
                "id": profile.id,
                "name": profile.name,
                "revision": profile.revision,
                "target_count": profile.cameras.len(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "source": "in_memory",
        "revision": registry.revision,
        "active_profile_id": registry.active_profile_id,
        "profiles": profiles,
        "status": status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cameras::{CameraConfig, CameraRecordingMode},
        config::Config,
        storage::{
            RecordingCatalog, RecordingDemand, StorageConfig,
            metadata::{EventSource, TimelineEvent},
        },
        webrtc::WebRtc,
    };
    use std::{collections::HashMap, net::IpAddr, path::PathBuf};

    fn admin_principal() -> ApiPrincipal {
        ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn scoped_user_principal(state: &ServerState, allowed_cameras: Vec<String>) -> ApiPrincipal {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let issued = state
            .access_manager
            .create_credential("mcp-viewer", None, AccessRole::User, None, now_ms)
            .unwrap();
        state
            .access_manager
            .set_camera_access(
                issued.metadata.id,
                issued.metadata.revision,
                CameraAccess {
                    all_cameras: false,
                    camera_ids: allowed_cameras,
                    group_ids: Vec::new(),
                },
            )
            .unwrap();
        let header = format!("Bearer {}", issued.access_key.canonical());
        let authenticated = state
            .access_manager
            .authenticate(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &[header.as_str()],
                now_ms,
                Instant::now(),
            )
            .unwrap();
        ApiPrincipal::credential(authenticated)
    }

    fn camera_config(ip: Ipv4Addr, name: &str) -> CameraConfig {
        CameraConfig {
            ip: IpAddr::V4(ip),
            name: Some(name.to_owned()),
            display_name: None,
            manufacturer: None,
            username: String::new(),
            password: String::new(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: crate::cameras::CameraBackend::Auto,
            transport: crate::cameras::CameraTransport::Tcp,
            events: Default::default(),
            record_generic_motion_events: false,
            recording_mode: CameraRecordingMode::Off,
            event_recording_duration_secs: 60,
            event_pre_recording_duration_secs: 0,
            event_recording_stream: Default::default(),
            recorded_audio: Default::default(),
            live_audio: Default::default(),
        }
    }

    fn test_state_with_cameras() -> (
        PathBuf,
        ServerState,
        FacadeSender<RouterMessage>,
        RecordingCatalog,
    ) {
        let dir = std::env::temp_dir().join(format!("mcp-diag-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        let mut config = Config::default();
        config.storage.long_term_path = Some(dir.join("recordings").to_string_lossy().into_owned());
        config.storage.medium_term_path =
            Some(dir.join("recordings").to_string_lossy().into_owned());
        crate::config::write_private_file(
            &config_path,
            toml::to_string(&config).unwrap().as_bytes(),
        )
        .unwrap();
        let mut camera_configs = HashMap::new();
        camera_configs.insert(
            "default".to_owned(),
            vec![
                camera_config(Ipv4Addr::new(192, 0, 2, 10), "front-door"),
                camera_config(Ipv4Addr::new(192, 0, 2, 11), "private-office"),
            ],
        );
        let storage = StorageConfig::from_toml(&config.storage);
        let catalog = RecordingCatalog::open(&dir.join("catalog.db")).unwrap();
        let state = ServerState::new(
            &config,
            &camera_configs,
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        )
        .with_camera_config_path(config_path)
        .with_recording_catalog(catalog.handle());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        (dir, state, router_tx, catalog)
    }

    fn insert_event(catalog: &RecordingCatalog, id: &str, camera_id: &str, start_time_ms: i64) {
        catalog
            .handle()
            .insert_event(TimelineEvent {
                id: id.to_owned(),
                revision: 1,
                camera_id: camera_id.to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::Camera,
                kind: "person".to_owned(),
                start_time_ms,
                end_time_ms: Some(start_time_ms + 1_000),
                confidence: Some(0.95),
                bbox: None,
                bbox_attachment_id: None,
                zone: None,
                text: None,
                payload: None,
                attachments: Vec::new(),
                canonical_attachment_id: None,
                icon_key: "person".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: None,
            })
            .unwrap();
    }

    #[test]
    fn mcp_diagnostics_search_events_enforces_camera_scope_and_clamps_limit() {
        let (dir, state, router_tx, catalog) = test_state_with_cameras();
        let cameras = state.camera_entries();
        let front_id = cameras
            .iter()
            .find(|c| c.info.ip == "192.0.2.10")
            .unwrap()
            .info
            .id
            .clone();
        let office_id = cameras
            .iter()
            .find(|c| c.info.ip == "192.0.2.11")
            .unwrap()
            .info
            .id
            .clone();
        let now_ms = i64::try_from(unix_time_ms()).unwrap();
        insert_event(&catalog, "evt-front-1", &front_id, now_ms - 5_000);
        insert_event(&catalog, "evt-office-1", &office_id, now_ms - 4_000);

        let viewer = scoped_user_principal(&state, vec![front_id.clone()]);
        let denied = search_events(
            &state,
            &router_tx,
            &viewer,
            &json!({ "camera_id": office_id }),
        )
        .unwrap_err();
        assert_eq!(denied._http_status, 403);

        let scoped = search_events(&state, &router_tx, &viewer, &json!({ "limit": 999 })).unwrap();
        assert_eq!(scoped["limit"], 100);
        let events = scoped["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["camera_id"], front_id);

        let zero_clamped =
            search_events(&state, &router_tx, &viewer, &json!({ "limit": 0 })).unwrap();
        assert_eq!(zero_clamped["limit"], 1);

        let defaulted = search_events(&state, &router_tx, &viewer, &json!({})).unwrap();
        assert_eq!(defaulted["limit"], EVENT_SEARCH_LIMIT_DEFAULT);

        catalog.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_diagnostics_admin_only_handlers_reject_non_admin_principals() {
        let (dir, state, router_tx, catalog) = test_state_with_cameras();
        let viewer = scoped_user_principal(&state, Vec::new());

        for result in [
            get_storage_status(&state, &router_tx, &viewer, &json!({})),
            get_notification_status(&state, &router_tx, &viewer, &json!({})),
            discover_cameras(
                &state,
                &router_tx,
                &viewer,
                &json!({ "list_networks_only": true }),
            ),
            get_runtime_configuration(&state, &router_tx, &viewer, &json!({})),
            list_runtime_profiles(&state, &router_tx, &viewer, &json!({})),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error._http_status, 403);
            assert_eq!(error.code, proto::ErrorCode::Rejected);
        }

        catalog.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_diagnostics_coverage_enforces_camera_auth_and_window_bounds() {
        let (dir, state, router_tx, catalog) = test_state_with_cameras();
        let cameras = state.camera_entries();
        let front_id = cameras
            .iter()
            .find(|c| c.info.ip == "192.0.2.10")
            .unwrap()
            .info
            .id
            .clone();
        let office_id = cameras
            .iter()
            .find(|c| c.info.ip == "192.0.2.11")
            .unwrap()
            .info
            .id
            .clone();
        let viewer = scoped_user_principal(&state, vec![front_id.clone()]);

        let denied = get_recording_coverage(
            &state,
            &router_tx,
            &viewer,
            &json!({ "camera_id": office_id }),
        )
        .unwrap_err();
        assert_eq!(denied._http_status, 403);

        let now = unix_time_ms();
        let oversized = get_recording_coverage(
            &state,
            &router_tx,
            &viewer,
            &json!({
                "start_ms": now.saturating_sub(40 * 86_400_000),
                "end_ms": now,
            }),
        )
        .unwrap_err();
        assert_eq!(oversized._http_status, 400);

        let allowed = get_recording_coverage(
            &state,
            &router_tx,
            &viewer,
            &json!({ "camera_id": front_id, "max_segments": 4 }),
        )
        .unwrap();
        let returned_cameras = allowed["cameras"].as_array().unwrap();
        assert_eq!(returned_cameras.len(), 1);
        assert_eq!(returned_cameras[0]["camera_id"], front_id);

        catalog.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_diagnostics_admin_handlers_return_sanitized_status() {
        let (dir, state, router_tx, catalog) = test_state_with_cameras();
        let admin = admin_principal();

        let storage = get_storage_status(&state, &router_tx, &admin, &json!({})).unwrap();
        assert!(storage.get("configuration").is_some());
        assert!(storage.get("health").is_some());
        assert!(storage.get("named_volumes").is_some());
        assert!(storage.get("archive").is_some());
        assert!(storage.get("recording_maintenance").is_some());

        let notifications =
            get_notification_status(&state, &router_tx, &admin, &json!({})).unwrap();
        assert_eq!(notifications["rules"]["available"], false);
        assert_eq!(notifications["cloud_notifications"]["available"], false);
        assert_eq!(notifications["mqtt_forwarder"]["available"], false);

        let runtime_cfg =
            get_runtime_configuration(&state, &router_tx, &admin, &json!({})).unwrap();
        assert!(runtime_cfg["port"].as_u64().is_some());
        assert!(runtime_cfg["storage"].is_object());

        let profiles = list_runtime_profiles(&state, &router_tx, &admin, &json!({})).unwrap();
        assert_eq!(profiles["source"], "state_store_profiles");
        assert!(profiles["registry"].is_object());

        let networks = discover_cameras(
            &state,
            &router_tx,
            &admin,
            &json!({ "list_networks_only": true }),
        )
        .unwrap();
        assert_eq!(networks["list_networks_only"], true);

        let bad_cidr = discover_cameras(
            &state,
            &router_tx,
            &admin,
            &json!({ "networks": ["8.8.8.0/24"] }),
        )
        .unwrap_err();
        assert_eq!(bad_cidr._http_status, 400);

        assert_eq!(
            sanitize_broker_url("mqtts://user:secret-pass@broker.local:8883/path?token=xyz"),
            "mqtts://broker.local:8883"
        );
        assert!(!sanitize_broker_url("mqtt://alice:topsecret@10.0.0.5:1883").contains("topsecret"));
        assert_eq!(
            in_memory_runtime_profiles_json(&ServerState::empty())["profiles"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        catalog.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }
}
