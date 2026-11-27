//! Privacy-gated and permission-gated single-frame snapshot engine for MCP.
//!
//! Enforces fail-closed policy checks, per-camera rate limits, bounded JPEG
//! decoding, software privacy masking before downscaling, size ceilings, and
//! non-sensitive audit logging.

use super::{
    ApiPrincipal, CameraEntry, ControlCommandError, ReolinkClient, ServerState, camera_access,
    record_access_audit, unix_time_ms,
};
use crate::{
    access::ClientClassificationReason,
    api::proto,
    privacy::MediaPermit,
    runtime::{FacadeSender, RouterMessage},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use image::{
    ImageFormat, ImageReader, Limits, Rgb, RgbImage, codecs::jpeg::JpegEncoder,
    imageops::FilterType,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const RATE_LIMITER_ENTRIES_MAX: usize = 256;
const RAW_SNAPSHOT_BYTES_MAX: usize = 8 * 1_024 * 1_024;
const DECODE_DIMENSION_MAX: u32 = 8_192;
const DECODE_ALLOC_BYTES_MAX: u64 = 64 * 1_024 * 1_024;
const PRIVACY_MASKS_MAX: usize = 32;
const SNAPSHOT_HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const LATEST_EVENT_LOOKBACK_MS: i64 = 7 * 86_400_000;
const LATEST_EVENT_SCAN_MAX: usize = 64;
const JPEG_QUALITY_STEPS: [u8; 5] = [85, 70, 55, 40, 25];

const _: () = {
    assert!(RATE_LIMITER_ENTRIES_MAX > 0);
    assert!(RAW_SNAPSHOT_BYTES_MAX >= 1_024 * 1_024);
    assert!(DECODE_DIMENSION_MAX >= 1_920);
    assert!(JPEG_QUALITY_STEPS[0] > JPEG_QUALITY_STEPS[4]);
};

/// Explicit snapshot policy passed by the caller without coupling to config structs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct McpSnapshotPolicy {
    pub(super) allow_snapshots: bool,
    pub(super) max_bytes: usize,
    pub(super) max_dimension: u32,
    pub(super) min_interval_secs: u64,
}

/// Normalized `[0.0, 1.0]` rectangle filled with solid black before downscaling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PrivacyMaskRect {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

impl PrivacyMaskRect {
    fn validate(self) -> Result<(), ControlCommandError> {
        let coords = [self.x, self.y, self.width, self.height];
        if coords
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                403,
                "camera privacy mask configuration is invalid",
            ));
        }
        Ok(())
    }
}

/// Bounded per-camera snapshot rate limiter capped at 256 camera entries.
#[derive(Debug, Default)]
pub(super) struct SnapshotRateLimiter {
    entries: Mutex<HashMap<String, Instant>>,
}

impl SnapshotRateLimiter {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn check_and_record(
        &self,
        camera_id: &str,
        min_interval_secs: u64,
        now: Instant,
    ) -> Result<(), ControlCommandError> {
        let interval = Duration::from_secs(min_interval_secs);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = entries.get(camera_id)
            && now.saturating_duration_since(*previous) < interval
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                429,
                "camera snapshot rate limit exceeded; retry later",
            ));
        }
        if !entries.contains_key(camera_id) && entries.len() >= RATE_LIMITER_ENTRIES_MAX {
            entries.retain(|_, seen| now.saturating_duration_since(*seen) < interval);
            if entries.len() >= RATE_LIMITER_ENTRIES_MAX
                && let Some(oldest_key) = entries
                    .iter()
                    .min_by_key(|(_, seen)| **seen)
                    .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest_key);
            }
        }
        entries.insert(camera_id.to_owned(), now);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourcePreference {
    Auto,
    Live,
    Isapi,
    Onvif,
    Reolink,
    LatestEvent,
}

impl SourcePreference {
    fn parse(raw: Option<&str>) -> Result<Self, ControlCommandError> {
        match raw.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("auto") => Ok(Self::Auto),
            Some("live") => Ok(Self::Live),
            Some("isapi") => Ok(Self::Isapi),
            Some("onvif") => Ok(Self::Onvif),
            Some("reolink") => Ok(Self::Reolink),
            Some("latest_event") => Ok(Self::LatestEvent),
            Some(_) => Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "source_preference must be auto, live, isapi, onvif, reolink, or latest_event",
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ProcessedSnapshot {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) masks_applied: usize,
    pub(super) jpeg_bytes: Vec<u8>,
}

struct CapturedFrame {
    jpeg_bytes: Vec<u8>,
    source: &'static str,
}

// ponytail: module-scoped limiter avoids ServerState edits; move into ServerState during merge if needed.
fn default_rate_limiter() -> &'static SnapshotRateLimiter {
    static LIMITER: OnceLock<SnapshotRateLimiter> = OnceLock::new();
    LIMITER.get_or_init(SnapshotRateLimiter::new)
}

/// Captures one privacy-gated, masked, resized JPEG snapshot for an authorized camera.
pub(super) fn capture_camera_snapshot(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    camera_id: &str,
    source_preference: Option<&str>,
    policy: &McpSnapshotPolicy,
) -> Result<Value, ControlCommandError> {
    capture_with_options(
        state,
        router_tx,
        principal,
        camera_id,
        source_preference,
        policy,
        default_rate_limiter(),
        Instant::now(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn capture_with_options(
    state: &ServerState,
    _router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    camera_id: &str,
    source_preference: Option<&str>,
    policy: &McpSnapshotPolicy,
    limiter: &SnapshotRateLimiter,
    now: Instant,
    explicit_masks: Option<&[PrivacyMaskRect]>,
) -> Result<Value, ControlCommandError> {
    let camera_id = validate_camera_id(camera_id)?;
    validate_policy(policy)?;
    let preference = SourcePreference::parse(source_preference)?;
    if !policy.allow_snapshots {
        audit_snapshot(state, principal, camera_id, "denied_disabled");
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "MCP snapshots are disabled in [mcp].allow_snapshots",
        ));
    }
    authorize_camera_view(state, principal, camera_id)?;
    let Some(camera) = state.camera(camera_id) else {
        audit_snapshot(state, principal, camera_id, "not_found");
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "camera was not found",
        ));
    };
    if let Err(error) = limiter.check_and_record(&camera.info.id, policy.min_interval_secs, now) {
        audit_snapshot(state, principal, &camera.info.id, "denied_rate_limit");
        return Err(error);
    }
    let permit = verify_privacy_fail_closed(state, principal, &camera)?;
    let masks = resolve_snapshot_masks(state, principal, &camera, explicit_masks)?;
    let captured = acquire_frame(state, &camera, preference).inspect_err(|_| {
        audit_snapshot(state, principal, &camera.info.id, "unavailable");
    })?;
    // Recheck privacy after capture so a transition during fetch discards the frame.
    if permit.deliver(|| ()).is_none() {
        audit_snapshot(state, principal, &camera.info.id, "denied_privacy");
        return Err(privacy_denied_error());
    }
    let processed =
        process_snapshot_jpeg(&captured.jpeg_bytes, &masks, policy).inspect_err(|error| {
            let outcome = if error._http_status == 413 {
                "rejected_oversize"
            } else {
                "failed_decode"
            };
            audit_snapshot(state, principal, &camera.info.id, outcome);
        })?;
    Ok(finalize_snapshot_response(
        state,
        principal,
        &camera.info.id,
        captured.source,
        processed,
    ))
}

fn resolve_snapshot_masks(
    state: &ServerState,
    principal: &ApiPrincipal,
    camera: &CameraEntry,
    explicit_masks: Option<&[PrivacyMaskRect]>,
) -> Result<Vec<PrivacyMaskRect>, ControlCommandError> {
    explicit_masks
        .map_or_else(
            || load_configured_privacy_masks(state, camera),
            validate_mask_slice,
        )
        .inspect_err(|_| {
            audit_snapshot(state, principal, &camera.info.id, "denied_privacy");
        })
}

fn validate_camera_id(camera_id: &str) -> Result<&str, ControlCommandError> {
    let trimmed = camera_id.trim();
    if trimmed.is_empty() || trimmed.len() > 256 || trimmed.chars().any(char::is_control) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "camera_id must contain 1 to 256 printable bytes",
        ));
    }
    Ok(trimmed)
}

fn validate_policy(policy: &McpSnapshotPolicy) -> Result<(), ControlCommandError> {
    if policy.max_bytes == 0 || policy.max_dimension == 0 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "snapshot policy max_bytes and max_dimension must be nonzero",
        ));
    }
    Ok(())
}

fn authorize_camera_view(
    state: &ServerState,
    principal: &ApiPrincipal,
    camera_id: &str,
) -> Result<(), ControlCommandError> {
    let access = match camera_access::for_principal(state, principal) {
        Ok(access) => access,
        Err(error) => {
            audit_snapshot(state, principal, camera_id, "denied_permission");
            return Err(error);
        }
    };
    if let Err(error) = camera_access::require_camera(&access, camera_id) {
        audit_snapshot(state, principal, camera_id, "denied_permission");
        return Err(error);
    }
    Ok(())
}

fn verify_privacy_fail_closed(
    state: &ServerState,
    principal: &ApiPrincipal,
    camera: &CameraEntry,
) -> Result<MediaPermit, ControlCommandError> {
    let now = chrono::Utc::now();
    let by_id = state.privacy.decision(&camera.info.id, now);
    let by_ip = state.privacy.decision(&camera.info.ip, now);
    let active_or_unverifiable =
        !matches!(by_id, Ok((false, _))) || !matches!(by_ip, Ok((false, _)));
    if active_or_unverifiable {
        audit_snapshot(state, principal, &camera.info.id, "denied_privacy");
        return Err(privacy_denied_error());
    }
    let Some(permit) = state.privacy.media_permit(camera.info.id.as_str().into()) else {
        audit_snapshot(state, principal, &camera.info.id, "denied_privacy");
        return Err(privacy_denied_error());
    };
    Ok(permit)
}

fn privacy_denied_error() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        403,
        "camera privacy mode is active or unverifiable",
    )
}

fn validate_mask_slice(
    masks: &[PrivacyMaskRect],
) -> Result<Vec<PrivacyMaskRect>, ControlCommandError> {
    if masks.len() > PRIVACY_MASKS_MAX {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "camera privacy mask count exceeds limit",
        ));
    }
    for mask in masks {
        mask.validate()?;
    }
    Ok(masks.to_vec())
}

fn load_configured_privacy_masks(
    state: &ServerState,
    camera: &CameraEntry,
) -> Result<Vec<PrivacyMaskRect>, ControlCommandError> {
    let Some(path) = state.camera_config_path.as_deref() else {
        return Ok(Vec::new());
    };
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let root = crate::config::load_configuration_table(path).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "camera privacy mask configuration could not be verified",
        )
    })?;
    let Some(mask_array) = find_camera_mask_array(&root, &camera.info.id, &camera.info.ip) else {
        return Ok(Vec::new());
    };
    let entries = mask_array.as_array().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "camera privacy mask configuration is invalid",
        )
    })?;
    if entries.len() > PRIVACY_MASKS_MAX {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "camera privacy mask count exceeds limit",
        ));
    }
    entries.iter().map(parse_toml_mask_rect).collect()
}

fn find_camera_mask_array<'a>(
    root: &'a toml::Table,
    camera_id: &str,
    camera_ip: &str,
) -> Option<&'a toml::Value> {
    let privacy = root.get("privacy")?.as_table()?;
    if let Some(masks) = privacy.get("masks").and_then(toml::Value::as_table)
        && let Some(found) = masks.get(camera_id).or_else(|| masks.get(camera_ip))
    {
        return Some(found);
    }
    privacy
        .get("cameras")
        .and_then(toml::Value::as_table)
        .and_then(|cameras| cameras.get(camera_id).or_else(|| cameras.get(camera_ip)))
        .and_then(toml::Value::as_table)
        .and_then(|camera| camera.get("masks"))
}

fn parse_toml_mask_rect(value: &toml::Value) -> Result<PrivacyMaskRect, ControlCommandError> {
    let table = value.as_table().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "camera privacy mask entry must be a table",
        )
    })?;
    let rect = PrivacyMaskRect {
        x: toml_f32(table, "x")?,
        y: toml_f32(table, "y")?,
        width: toml_f32(table, "width")?,
        height: toml_f32(table, "height")?,
    };
    rect.validate()?;
    Ok(rect)
}

fn toml_f32(table: &toml::Table, key: &str) -> Result<f32, ControlCommandError> {
    let value = table.get(key).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "camera privacy mask coordinate is missing",
        )
    })?;
    let number = value
        .as_float()
        .or_else(|| value.as_integer().map(|v| v as f64))
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Rejected,
                403,
                "camera privacy mask coordinate must be numeric",
            )
        })?;
    Ok(number as f32)
}

fn acquire_frame(
    state: &ServerState,
    camera: &CameraEntry,
    preference: SourcePreference,
) -> Result<CapturedFrame, ControlCommandError> {
    match preference {
        SourcePreference::Isapi => capture_isapi_frame(camera),
        SourcePreference::Reolink => capture_reolink_frame(camera),
        SourcePreference::Onvif => capture_onvif_frame(camera),
        SourcePreference::LatestEvent => capture_latest_event_frame(state, &camera.info.id),
        SourcePreference::Live => capture_live_frame(camera),
        SourcePreference::Auto => capture_live_frame(camera)
            .or_else(|_| capture_latest_event_frame(state, &camera.info.id)),
    }
}

fn capture_live_frame(camera: &CameraEntry) -> Result<CapturedFrame, ControlCommandError> {
    if let Ok(frame) = capture_isapi_frame(camera) {
        return Ok(frame);
    }
    if let Ok(frame) = capture_reolink_frame(camera) {
        return Ok(frame);
    }
    if let Ok(frame) = capture_onvif_frame(camera) {
        return Ok(frame);
    }
    Err(ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        "no live snapshot source is available for this camera",
    ))
}

fn capture_isapi_frame(camera: &CameraEntry) -> Result<CapturedFrame, ControlCommandError> {
    let route = crate::isapi::Route::for_control(
        &camera.configuration,
        camera.info.ports.http,
        camera.info.manufacturer.as_deref(),
    )
    .ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "ISAPI snapshot is not available for this camera",
        )
    })?;
    let mut client = ::isapi::blocking::Client::new(
        &route.origin,
        ::isapi::Credentials::new(
            camera.configuration.username.clone(),
            camera.configuration.password.clone(),
        ),
    )
    .map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "ISAPI snapshot client could not be initialized",
        )
    })?;
    let main_stream = route.channel.saturating_mul(100).saturating_add(1);
    let sub_stream = route.channel.saturating_mul(100).saturating_add(2);
    let bytes = client
        .snapshot(main_stream)
        .or_else(|_| client.snapshot(sub_stream))
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "ISAPI snapshot capture failed",
            )
        })?;
    Ok(CapturedFrame {
        jpeg_bytes: bytes,
        source: "isapi",
    })
}

fn capture_reolink_frame(camera: &CameraEntry) -> Result<CapturedFrame, ControlCommandError> {
    if !camera.info.is_reolink {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "Reolink snapshot is not available for this camera",
        ));
    }
    let (ip, http_port, username, password) = camera.control.as_ref().map_or_else(
        || {
            (
                camera.configuration.ip,
                camera.configuration.http_port.or(camera.info.ports.http),
                camera.configuration.username.as_str(),
                camera.configuration.password.as_str(),
            )
        },
        |control| {
            (
                control.ip,
                control.http_port,
                control.username.as_str(),
                control.password.as_str(),
            )
        },
    );
    let mut client = ReolinkClient::new_with_http_port(ip, http_port);
    client.login(username, password).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "Reolink snapshot login failed",
        )
    })?;
    let bytes = client.snap(0).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "Reolink snapshot capture failed",
        )
    })?;
    Ok(CapturedFrame {
        jpeg_bytes: bytes,
        source: "reolink",
    })
}

fn capture_onvif_frame(camera: &CameraEntry) -> Result<CapturedFrame, ControlCommandError> {
    let snapshot_uri = camera
        .runtime_camera
        .as_ref()
        .and_then(|runtime| {
            runtime
                .profiles
                .iter()
                .find_map(|profile| profile.snapshot_uri.as_deref())
        })
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "ONVIF snapshot URI is not available for this camera",
            )
        })?;
    let port = camera
        .configuration
        .http_port
        .or(camera.info.ports.http)
        .unwrap_or(80);
    let address = std::net::SocketAddr::new(camera.configuration.ip, port);
    let origin = onvif::event::Endpoint::new(format!("http://{address}/")).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "ONVIF snapshot origin is invalid",
        )
    })?;
    let endpoint = onvif::event::Endpoint::new(snapshot_uri).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "ONVIF snapshot URI is invalid",
        )
    })?;
    let mut client = onvif::event::Client::new(
        origin,
        onvif::soap::client::Credentials {
            username: camera.configuration.username.clone(),
            password: camera.configuration.password.clone(),
        },
    )
    .map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "ONVIF snapshot client could not be initialized",
        )
    })?;
    let bytes = client
        .snapshot(&endpoint, SNAPSHOT_HTTP_TIMEOUT)
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "ONVIF snapshot capture failed",
            )
        })?;
    Ok(CapturedFrame {
        jpeg_bytes: bytes,
        source: "onvif",
    })
}

fn capture_latest_event_frame(
    state: &ServerState,
    camera_id: &str,
) -> Result<CapturedFrame, ControlCommandError> {
    let event_store = state.events.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "stored event thumbnails are unavailable",
        )
    })?;
    let end_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let start_ms = end_ms.saturating_sub(LATEST_EVENT_LOOKBACK_MS);
    let mut events = event_store
        .events_in_range(camera_id, start_ms, end_ms)
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "event store query failed",
            )
        })?;
    events.sort_unstable_by_key(|event| std::cmp::Reverse(event.start_time_ms));
    for event in events.into_iter().take(LATEST_EVENT_SCAN_MAX) {
        let Ok(Some(path)) = event_store.thumbnail_path(camera_id, &event.id) else {
            continue;
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.len() == 0
            || metadata.len() > u64::try_from(RAW_SNAPSHOT_BYTES_MAX).unwrap_or(u64::MAX)
        {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path)
            && is_jpeg_framed(&bytes)
        {
            return Ok(CapturedFrame {
                jpeg_bytes: bytes,
                source: "latest_event",
            });
        }
    }
    Err(ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        "no recent event JPEG snapshot is available for this camera",
    ))
}

fn is_jpeg_framed(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9])
}

/// Decodes a JPEG with strict limits, applies privacy masks, downscales, and re-encodes.
pub(super) fn process_snapshot_jpeg(
    raw_jpeg: &[u8],
    masks: &[PrivacyMaskRect],
    policy: &McpSnapshotPolicy,
) -> Result<ProcessedSnapshot, ControlCommandError> {
    validate_policy(policy)?;
    let validated_masks = validate_mask_slice(masks)?;
    let mut image = decode_bounded_jpeg(raw_jpeg)?;
    // Apply privacy masks before downscaling so masked pixels cannot bleed into neighbors.
    let masks_applied = apply_privacy_masks(&mut image, &validated_masks);
    let image = downscale_if_needed(image, policy.max_dimension);
    let jpeg_bytes = encode_within_budget(&image, policy.max_bytes)?;
    assert!(
        jpeg_bytes.len() <= policy.max_bytes,
        "encoded JPEG exceeded max_bytes budget"
    );
    assert!(
        image.width() <= policy.max_dimension && image.height() <= policy.max_dimension,
        "resized snapshot exceeded max_dimension"
    );
    Ok(ProcessedSnapshot {
        width: image.width(),
        height: image.height(),
        masks_applied,
        jpeg_bytes,
    })
}

fn decode_bounded_jpeg(raw_jpeg: &[u8]) -> Result<RgbImage, ControlCommandError> {
    if raw_jpeg.len() < 4
        || raw_jpeg.len() > RAW_SNAPSHOT_BYTES_MAX
        || !raw_jpeg.starts_with(&[0xff, 0xd8])
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            "camera returned an invalid or oversized JPEG frame",
        ));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(DECODE_DIMENSION_MAX);
    limits.max_image_height = Some(DECODE_DIMENSION_MAX);
    limits.max_alloc = Some(DECODE_ALLOC_BYTES_MAX);
    let mut reader = ImageReader::with_format(Cursor::new(raw_jpeg), ImageFormat::Jpeg);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            502,
            "camera JPEG frame failed bounded decoding",
        )
    })?;
    Ok(decoded.to_rgb8())
}

fn apply_privacy_masks(image: &mut RgbImage, masks: &[PrivacyMaskRect]) -> usize {
    let width = image.width();
    let height = image.height();
    let mut applied = 0usize;
    for mask in masks {
        let x0 = (f64::from(mask.x) * f64::from(width)).floor() as u32;
        let y0 = (f64::from(mask.y) * f64::from(height)).floor() as u32;
        let x1 =
            ((f64::from(mask.x) + f64::from(mask.width)).min(1.0) * f64::from(width)).ceil() as u32;
        let y1 = ((f64::from(mask.y) + f64::from(mask.height)).min(1.0) * f64::from(height)).ceil()
            as u32;
        let x1 = x1.min(width);
        let y1 = y1.min(height);
        for py in y0..y1 {
            for px in x0..x1 {
                image.put_pixel(px, py, Rgb([0, 0, 0]));
            }
        }
        applied += 1;
    }
    applied
}

fn downscale_if_needed(image: RgbImage, max_dimension: u32) -> RgbImage {
    let (width, height) = image.dimensions();
    if width <= max_dimension && height <= max_dimension {
        return image;
    }
    let (target_w, target_h) = scaled_dimensions(width, height, max_dimension);
    image::imageops::resize(&image, target_w, target_h, FilterType::Triangle)
}

fn scaled_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    assert!(
        width > 0 && height > 0 && max_dimension > 0,
        "dimensions must be positive"
    );
    if width >= height {
        let scaled_h = (u64::from(height) * u64::from(max_dimension)) / u64::from(width);
        (
            max_dimension,
            u32::try_from(scaled_h)
                .unwrap_or(1)
                .max(1)
                .min(max_dimension),
        )
    } else {
        let scaled_w = (u64::from(width) * u64::from(max_dimension)) / u64::from(height);
        (
            u32::try_from(scaled_w)
                .unwrap_or(1)
                .max(1)
                .min(max_dimension),
            max_dimension,
        )
    }
}

fn encode_within_budget(
    image: &RgbImage,
    max_bytes: usize,
) -> Result<Vec<u8>, ControlCommandError> {
    for quality in JPEG_QUALITY_STEPS {
        let mut encoded = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut encoded, quality);
        encoder.encode_image(image).map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                "failed to encode snapshot JPEG",
            )
        })?;
        if encoded.len() <= max_bytes {
            return Ok(encoded);
        }
    }
    Err(ControlCommandError::new(
        proto::ErrorCode::Rejected,
        413,
        "encoded snapshot exceeds max_bytes limit",
    ))
}

fn finalize_snapshot_response(
    state: &ServerState,
    principal: &ApiPrincipal,
    camera_id: &str,
    source: &'static str,
    processed: ProcessedSnapshot,
) -> Value {
    let captured_at_ms = unix_time_ms();
    let bytes = processed.jpeg_bytes.len();
    let data_base64 = BASE64_STANDARD.encode(&processed.jpeg_bytes);
    audit_snapshot(state, principal, camera_id, "allowed");
    // Log metadata only and never log raw JPEG or base64 image payloads.
    tracing::info!(
        event = "mcp_camera_snapshot",
        camera_id,
        principal_id = %principal.id(),
        source,
        width = processed.width,
        height = processed.height,
        bytes,
        masks_applied = processed.masks_applied,
        outcome = "allowed",
        "MCP camera snapshot captured"
    );
    json!({
        "camera_id": camera_id,
        "mime_type": "image/jpeg",
        "width": processed.width,
        "height": processed.height,
        "bytes": bytes,
        "masks_applied": processed.masks_applied,
        "source": source,
        "captured_at_ms": captured_at_ms,
        "data_base64": data_base64,
    })
}

fn audit_snapshot(
    state: &ServerState,
    principal: &ApiPrincipal,
    camera_id: &str,
    outcome: &'static str,
) {
    record_access_audit(
        state,
        i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
        Some(&principal.id()),
        Some(principal.role),
        "mcp_camera_snapshot",
        Some(camera_id),
        outcome,
        ClientClassificationReason::UnknownSession,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        access::{AccessRole, CameraAccess},
        cameras::{CameraConfig, CameraRecordingMode},
        config::Config,
        storage::{
            EventStore, RecordingCatalog, RecordingDemand, StorageConfig,
            metadata::{EventSource, TimelineEvent},
        },
        webrtc::WebRtc,
    };
    use std::{
        collections::BTreeMap,
        net::{IpAddr, Ipv4Addr},
        path::PathBuf,
    };

    fn default_policy() -> McpSnapshotPolicy {
        McpSnapshotPolicy {
            allow_snapshots: true,
            max_bytes: 262_144,
            max_dimension: 1_280,
            min_interval_secs: 5,
        }
    }

    fn admin_principal() -> ApiPrincipal {
        ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn scoped_user(state: &ServerState, cameras: Vec<String>) -> ApiPrincipal {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let issued = state
            .access_manager
            .create_credential("snap-viewer", None, AccessRole::User, None, now_ms)
            .unwrap();
        state
            .access_manager
            .set_camera_access(
                issued.metadata.id,
                issued.metadata.revision,
                CameraAccess {
                    all_cameras: false,
                    camera_ids: cameras,
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

    fn sample_jpeg(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        let image = RgbImage::from_pixel(width, height, Rgb(color));
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 95)
            .encode_image(&image)
            .unwrap();
        bytes
    }

    fn noisy_jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut image = RgbImage::new(width, height);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let r = ((x.wrapping_mul(73) ^ y.wrapping_mul(151)) & 0xff) as u8;
            let g = ((x.wrapping_mul(199) ^ y.wrapping_mul(41)) & 0xff) as u8;
            let b = ((x.wrapping_mul(13) ^ y.wrapping_mul(97)) & 0xff) as u8;
            *pixel = Rgb([r, g, b]);
        }
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 95)
            .encode_image(&image)
            .unwrap();
        bytes
    }

    fn test_camera_config() -> CameraConfig {
        CameraConfig {
            ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20)),
            name: Some("driveway".to_owned()),
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

    fn seed_event_thumbnail(event_store: &EventStore, camera_id: &str, jpeg: &[u8]) {
        let now_ms = i64::try_from(unix_time_ms()).unwrap();
        event_store
            .insert(TimelineEvent {
                id: "evt-snap-1".to_owned(),
                revision: 1,
                camera_id: camera_id.to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::Camera,
                kind: "person".to_owned(),
                start_time_ms: now_ms - 1_000,
                end_time_ms: Some(now_ms),
                confidence: Some(0.9),
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
        event_store
            .save_thumbnail(camera_id, "evt-snap-1", jpeg)
            .unwrap();
    }

    fn fixture_state(
        with_event_jpeg: Option<Vec<u8>>,
    ) -> (
        PathBuf,
        ServerState,
        FacadeSender<RouterMessage>,
        RecordingCatalog,
        String,
    ) {
        let dir = std::env::temp_dir().join(format!("mcp-snap-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        let mut config = Config::default();
        config.storage.long_term_path = Some(dir.join("recordings").to_string_lossy().into_owned());
        config.storage.medium_term_path =
            Some(dir.join("recordings").to_string_lossy().into_owned());
        config.storage.event_thumbnail_path =
            Some(dir.join("thumbnails").to_string_lossy().into_owned());
        crate::config::write_private_file(
            &config_path,
            toml::to_string(&config).unwrap().as_bytes(),
        )
        .unwrap();
        let mut cameras = HashMap::new();
        cameras.insert("default".to_owned(), vec![test_camera_config()]);
        let storage = StorageConfig::from_toml(&config.storage);
        let catalog = RecordingCatalog::open(&dir.join("catalog.db")).unwrap();
        let event_store = EventStore::new(
            catalog.handle(),
            &storage.event_thumbnail_path,
            storage.event_thumbnail_max_bytes,
        )
        .unwrap();
        let state = ServerState::new(
            &config,
            &cameras,
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        )
        .with_camera_config_path(config_path)
        .with_recording_catalog(catalog.handle())
        .with_event_store(event_store.clone());
        let camera_id = state.camera_entries()[0].info.id.clone();
        if let Some(jpeg) = with_event_jpeg {
            seed_event_thumbnail(&event_store, &camera_id, &jpeg);
        }
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        (dir, state, router_tx, catalog, camera_id)
    }

    #[test]
    fn mcp_snapshot_rejects_when_disabled_in_policy() {
        let (dir, state, router_tx, catalog, camera_id) = fixture_state(None);
        let policy = McpSnapshotPolicy {
            allow_snapshots: false,
            ..default_policy()
        };
        let limiter = SnapshotRateLimiter::new();
        let error = capture_with_options(
            &state,
            &router_tx,
            &admin_principal(),
            &camera_id,
            Some("latest_event"),
            &policy,
            &limiter,
            Instant::now(),
            None,
        )
        .unwrap_err();
        assert_eq!(error._http_status, 403);
        assert!(error.message.contains("allow_snapshots"));

        catalog.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_snapshot_rejects_unauthorized_principal() {
        let (dir, state, router_tx, catalog, camera_id) =
            fixture_state(Some(sample_jpeg(160, 120, [255, 255, 255])));
        let viewer = scoped_user(&state, Vec::new());
        let limiter = SnapshotRateLimiter::new();
        let error = capture_with_options(
            &state,
            &router_tx,
            &viewer,
            &camera_id,
            Some("latest_event"),
            &default_policy(),
            &limiter,
            Instant::now(),
            None,
        )
        .unwrap_err();
        assert_eq!(error._http_status, 403);
        assert_eq!(error.code, proto::ErrorCode::Rejected);

        catalog.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_snapshot_enforces_per_camera_rate_limit_and_entry_cap() {
        let limiter = SnapshotRateLimiter::new();
        let t0 = Instant::now();
        limiter.check_and_record("cam-a", 5, t0).unwrap();
        let limited = limiter
            .check_and_record("cam-a", 5, t0 + Duration::from_secs(4))
            .unwrap_err();
        assert_eq!(limited._http_status, 429);
        limiter
            .check_and_record("cam-a", 5, t0 + Duration::from_secs(5))
            .unwrap();
        for idx in 0..(RATE_LIMITER_ENTRIES_MAX + 32) {
            limiter
                .check_and_record(
                    &format!("cam-{idx}"),
                    60,
                    t0 + Duration::from_millis(idx as u64),
                )
                .unwrap();
        }
        assert!(limiter.entries.lock().unwrap().len() <= RATE_LIMITER_ENTRIES_MAX);
    }

    #[test]
    fn mcp_snapshot_refuses_fail_closed_when_privacy_is_active() {
        let (dir, state, router_tx, catalog, camera_id) =
            fixture_state(Some(sample_jpeg(160, 120, [255, 255, 255])));
        let mut overrides = BTreeMap::new();
        overrides.insert(camera_id.clone(), true);
        state.privacy.replace_profile_overrides(overrides).unwrap();

        let limiter = SnapshotRateLimiter::new();
        let error = capture_with_options(
            &state,
            &router_tx,
            &admin_principal(),
            &camera_id,
            Some("latest_event"),
            &default_policy(),
            &limiter,
            Instant::now(),
            None,
        )
        .unwrap_err();
        assert_eq!(error._http_status, 403);
        assert_eq!(
            error.message,
            "camera privacy mode is active or unverifiable"
        );

        catalog.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_snapshot_applies_privacy_masks_as_solid_black_before_downscale() {
        let raw = sample_jpeg(400, 200, [255, 255, 255]);
        let masks = [PrivacyMaskRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 0.5,
        }];
        let policy = McpSnapshotPolicy {
            allow_snapshots: true,
            max_bytes: 262_144,
            max_dimension: 200,
            min_interval_secs: 5,
        };
        let processed = process_snapshot_jpeg(&raw, &masks, &policy).unwrap();
        assert_eq!(processed.width, 200);
        assert_eq!(processed.height, 100);
        assert_eq!(processed.masks_applied, 1);

        let decoded = image::load_from_memory_with_format(&processed.jpeg_bytes, ImageFormat::Jpeg)
            .unwrap()
            .to_rgb8();
        let masked_pixel = decoded.get_pixel(25, 25);
        assert!(
            masked_pixel[0] <= 8 && masked_pixel[1] <= 8 && masked_pixel[2] <= 8,
            "masked region pixel must be near solid black, got {masked_pixel:?}"
        );
        let unmasked_pixel = decoded.get_pixel(175, 75);
        assert!(
            unmasked_pixel[0] >= 245 && unmasked_pixel[1] >= 245 && unmasked_pixel[2] >= 245,
            "unmasked region pixel must stay white, got {unmasked_pixel:?}"
        );
    }

    #[test]
    fn mcp_snapshot_downscales_and_enforces_jpeg_byte_ceiling() {
        let raw = noisy_jpeg(1_600, 800);
        let policy = McpSnapshotPolicy {
            allow_snapshots: true,
            max_bytes: 60_000,
            max_dimension: 640,
            min_interval_secs: 5,
        };
        let processed = process_snapshot_jpeg(&raw, &[], &policy).unwrap();
        assert_eq!(processed.width, 640);
        assert_eq!(processed.height, 320);
        assert!(processed.jpeg_bytes.len() <= policy.max_bytes);

        let impossible = McpSnapshotPolicy {
            max_bytes: 16,
            ..policy
        };
        let error = process_snapshot_jpeg(&raw, &[], &impossible).unwrap_err();
        assert_eq!(error._http_status, 413);
    }

    #[test]
    fn mcp_snapshot_captures_latest_event_and_emits_audit_without_image_bytes() {
        let (dir, state, router_tx, catalog, camera_id) =
            fixture_state(Some(sample_jpeg(320, 180, [220, 220, 220])));
        let limiter = SnapshotRateLimiter::new();
        let masks = [PrivacyMaskRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        }];
        let result = capture_with_options(
            &state,
            &router_tx,
            &admin_principal(),
            &camera_id,
            Some("latest_event"),
            &default_policy(),
            &limiter,
            Instant::now(),
            Some(&masks),
        )
        .unwrap();
        assert_eq!(result["camera_id"], camera_id);
        assert_eq!(result["mime_type"], "image/jpeg");
        assert_eq!(result["source"], "latest_event");
        assert_eq!(result["masks_applied"], 1);
        let b64 = result["data_base64"].as_str().unwrap();
        let decoded_bytes = BASE64_STANDARD.decode(b64).unwrap();
        assert_eq!(
            decoded_bytes.len(),
            result["bytes"].as_u64().unwrap() as usize
        );

        let audit = state.access_manager.list_audit(16);
        let snapshot_event = audit
            .iter()
            .find(|event| event.action == "mcp_camera_snapshot")
            .expect("snapshot audit event must be emitted");
        assert_eq!(snapshot_event.result, "allowed");
        assert_eq!(
            snapshot_event.target_id.as_deref(),
            Some(camera_id.as_str())
        );
        let serialized_audit = serde_json::to_string(&audit).unwrap();
        assert!(
            !serialized_audit.contains(&b64[..16.min(b64.len())]),
            "audit records must never include image bytes or base64 data"
        );

        catalog.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }
}
