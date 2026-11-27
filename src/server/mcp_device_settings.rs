//! Bounded Administrator handlers for inspecting and updating on-camera hardware settings.
//!
//! Supports Reolink HTTP API, Hikvision ISAPI, and ONVIF Media adapters with
//! fail-closed policy enforcement, dry-run validation, read-back verification,
//! hard security exclusions, and non-sensitive audit logging.

#![allow(
    dead_code,
    reason = "MCP dispatch wiring lands after PR #431 integration; covered by unit tests meanwhile"
)]

use super::{
    ApiPrincipal, CameraEntry, ControlCommandError, ReolinkClient, ServerState,
    record_access_audit, unix_time_ms,
};
use crate::{
    access::{AccessRole, ClientClassificationReason},
    api::proto,
    cameras::{CameraBackend, IrCutMode},
    runtime::{FacadeSender, RouterMessage},
};
use ::isapi::management::{
    Motion, Preset, PtzCapabilities, Rule, RuleKind, Stream, Time, TimeMode,
};
use onvif::soap::client::{AuthType, ClientBuilder, Credentials as OnvifCredentials};
use schema::{devicemgmt, media as onvif_media, onvif as onvif_xsd};
use serde_json::{Map, Value, json};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};
use url::Url;

const ALL_SECTIONS: [&str; 7] = [
    "encoder", "imaging", "lighting", "motion", "osd", "time", "ptz",
];
const MAX_SETTINGS_DEPTH: usize = 6;
const MAX_SETTINGS_KEYS: usize = 128;
const MAX_ONVIF_CONFIGS: usize = 8;
const MAX_ONVIF_OSDS: usize = 16;
const MAX_OSD_CHARS: usize = 64;
const REOLINK_OSD_CHARS_MAX: usize = 32;
const REOLINK_IMAGE_VALUE_MAX: u32 = 255;
const REOLINK_MD_SENSITIVITY_MAX: u32 = 50;
const REOLINK_AI_SENSITIVITY_MAX: u32 = 100;
const ISAPI_SENSITIVITY_MAX: u32 = 100;
const ISAPI_DIMENSION_MAX: u32 = 8_192;
const ISAPI_BITRATE_KBPS_MAX: u32 = 100_000;
const ISAPI_FRAME_RATE_HUNDREDTHS_MAX: u32 = 12_000;
const ISAPI_PRESET_ID_MAX: u32 = 300;

const EXCLUDED_SETTING_KEYS: [&str; 26] = [
    "factory_reset",
    "reset",
    "restore_defaults",
    "reboot",
    "firmware",
    "firmware_update",
    "upgrade",
    "flash",
    "password",
    "passwd",
    "new_password",
    "username",
    "user",
    "users",
    "credentials",
    "auth",
    "access_key",
    "ip",
    "ip_address",
    "network",
    "network_interface",
    "interfaces",
    "dhcp",
    "gateway",
    "subnet",
    "netmask",
];

const EXCLUDED_NETWORK_KEYS: [&str; 6] = [
    "dns",
    "ports",
    "http_port",
    "https_port",
    "rtsp_port",
    "onvif_port",
];

const _: () = {
    assert!(MAX_SETTINGS_DEPTH > 0);
    assert!(MAX_SETTINGS_KEYS >= 16);
    assert!(REOLINK_OSD_CHARS_MAX <= MAX_OSD_CHARS);
    assert!(ISAPI_SENSITIVITY_MAX == 100);
};

/// Explicit write policy passed by the caller without coupling to config structs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct McpDeviceWritePolicy {
    pub(super) allow_camera_device_writes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CameraProtocol {
    Reolink,
    Isapi,
    Onvif,
    RtspOnly,
}

impl CameraProtocol {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Reolink => "reolink",
            Self::Isapi => "isapi",
            Self::Onvif => "onvif",
            Self::RtspOnly => "rtsp_only",
        }
    }
}

struct DeviceSnapshot {
    protocol: CameraProtocol,
    supported_sections: Vec<&'static str>,
    settings: Map<String, Value>,
}

struct UpdateOutcome {
    validated: bool,
    diff_before: Map<String, Value>,
    diff_after: Map<String, Value>,
    unsupported: Vec<String>,
    applied_sections: Vec<String>,
    verified_settings: Map<String, Value>,
    warnings: Vec<String>,
    restart_or_reconnect_expected: bool,
}

/// Reads live hardware settings and supported sections for one camera.
pub(super) fn get_camera_device_settings(
    state: &ServerState,
    _router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    camera_id: &str,
    args: &Value,
    deadline: Instant,
) -> Result<Value, ControlCommandError> {
    require_admin(principal)?;
    let _ = args_object(args)?;
    let camera_id = validate_camera_id(camera_id)?;
    let _ = remaining_timeout(deadline)?;
    let camera = find_camera(state, camera_id)?;
    let mut snapshot = read_camera_settings(&camera, deadline)?;
    scrub_sensitive_map(&mut snapshot.settings);
    let unsupported = ALL_SECTIONS
        .iter()
        .copied()
        .filter(|section| !snapshot.supported_sections.contains(section))
        .collect::<Vec<_>>();
    Ok(json!({
        "camera_id": camera.info.id,
        "protocol": snapshot.protocol.as_str(),
        "supported_sections": snapshot.supported_sections,
        "unsupported_sections": unsupported,
        "settings": Value::Object(snapshot.settings),
    }))
}

/// Validates or applies bounded on-camera hardware setting updates with read-back verification.
#[allow(clippy::too_many_arguments)]
pub(super) fn update_camera_device_settings(
    state: &ServerState,
    _router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    camera_id: &str,
    args: &Value,
    policy: McpDeviceWritePolicy,
    read_only: bool,
    deadline: Instant,
) -> Result<Value, ControlCommandError> {
    require_admin(principal)?;
    let map = args_object(args)?;
    let camera_id = validate_camera_id(camera_id)?;
    let _ = remaining_timeout(deadline)?;
    let requested = extract_requested_settings(map)?;
    if let Err(error) = reject_excluded_fields(requested) {
        audit_device_update(state, principal, camera_id, "denied_excluded_field");
        return Err(error);
    }
    let dry_run = optional_bool(map, "dry_run")?.unwrap_or(true);
    if !dry_run && (!policy.allow_camera_device_writes || read_only) {
        audit_device_update(state, principal, camera_id, "denied_write_policy");
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "on-camera device writes are disabled in [mcp].allow_camera_device_writes or read-only mode",
        ));
    }
    let camera = find_camera(state, camera_id)?;
    let protocol = detect_protocol(&camera);
    let mut outcome = execute_protocol_update(&camera, protocol, requested, dry_run, deadline)?;
    scrub_sensitive_map(&mut outcome.diff_before);
    scrub_sensitive_map(&mut outcome.diff_after);
    scrub_sensitive_map(&mut outcome.verified_settings);
    Ok(format_update_response(
        state,
        principal,
        &camera.info.id,
        protocol,
        dry_run,
        outcome,
    ))
}

fn format_update_response(
    state: &ServerState,
    principal: &ApiPrincipal,
    camera_id: &str,
    protocol: CameraProtocol,
    dry_run: bool,
    outcome: UpdateOutcome,
) -> Value {
    let audit_outcome = if dry_run { "dry_run" } else { "applied" };
    audit_device_update(state, principal, camera_id, audit_outcome);
    let audit = json!({
        "action": "mcp_camera_device_settings_update",
        "camera_id": camera_id,
        "principal_id": principal.id(),
        "dry_run": dry_run,
        "sections": outcome.applied_sections,
        "outcome": audit_outcome,
    });
    if dry_run {
        return json!({
            "camera_id": camera_id,
            "protocol": protocol.as_str(),
            "dry_run": true,
            "validated": outcome.validated,
            "diff": {
                "before": Value::Object(outcome.diff_before),
                "after": Value::Object(outcome.diff_after),
            },
            "unsupported": outcome.unsupported,
            "audit": audit,
        });
    }
    json!({
        "camera_id": camera_id,
        "protocol": protocol.as_str(),
        "dry_run": false,
        "applied": true,
        "applied_sections": outcome.applied_sections,
        "verified_settings": Value::Object(outcome.verified_settings),
        "warnings": outcome.warnings,
        "restart_or_reconnect_expected": outcome.restart_or_reconnect_expected,
        "unsupported": outcome.unsupported,
        "audit": audit,
    })
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

fn validate_camera_id(camera_id: &str) -> Result<&str, ControlCommandError> {
    let trimmed = camera_id.trim();
    if trimmed.is_empty() || trimmed.len() > 256 || trimmed.chars().any(char::is_control) {
        return Err(invalid("camera_id must contain 1 to 256 printable bytes"));
    }
    Ok(trimmed)
}

fn remaining_timeout(deadline: Instant) -> Result<Duration, ControlCommandError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Timeout,
            504,
            "device settings request exceeded its timeout deadline",
        ));
    }
    Ok(deadline.saturating_duration_since(now))
}

fn find_camera(state: &ServerState, camera_id: &str) -> Result<CameraEntry, ControlCommandError> {
    state.camera(camera_id).ok_or_else(|| {
        ControlCommandError::new(proto::ErrorCode::NotFound, 404, "camera not found")
    })
}

fn args_object(args: &Value) -> Result<&Map<String, Value>, ControlCommandError> {
    if args.is_null() {
        static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
        return Ok(&EMPTY);
    }
    args.as_object()
        .ok_or_else(|| invalid("tool arguments must be a JSON object"))
}

fn optional_bool(map: &Map<String, Value>, key: &str) -> Result<Option<bool>, ControlCommandError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| invalid(&format!("{key} must be a boolean")))
}

fn extract_requested_settings(
    map: &Map<String, Value>,
) -> Result<&Map<String, Value>, ControlCommandError> {
    let settings = map
        .get("settings")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("settings must be a non-empty JSON object"))?;
    if settings.is_empty() {
        return Err(invalid("settings must specify at least one section"));
    }
    Ok(settings)
}

fn reject_excluded_fields(settings: &Map<String, Value>) -> Result<(), ControlCommandError> {
    let mut visited = 0usize;
    let mut stack = vec![(settings, 0usize)];
    while let Some((current, depth)) = stack.pop() {
        if depth > MAX_SETTINGS_DEPTH {
            return Err(invalid("settings nesting exceeds depth limit"));
        }
        for (key, value) in current {
            visited += 1;
            if visited > MAX_SETTINGS_KEYS {
                return Err(invalid("settings object exceeds maximum field count"));
            }
            let normalized = key.trim().to_ascii_lowercase();
            if EXCLUDED_SETTING_KEYS.contains(&normalized.as_str())
                || EXCLUDED_NETWORK_KEYS.contains(&normalized.as_str())
            {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    403,
                    format!("device setting '{key}' is prohibited by security policy"),
                ));
            }
            if let Some(child) = value.as_object() {
                stack.push((child, depth + 1));
            }
        }
    }
    Ok(())
}

fn detect_protocol(camera: &CameraEntry) -> CameraProtocol {
    if camera.info.is_reolink || camera.configuration.backend == CameraBackend::ReoProto {
        return CameraProtocol::Reolink;
    }
    if crate::isapi::Route::for_control(
        &camera.configuration,
        camera.info.ports.http,
        camera.info.manufacturer.as_deref(),
    )
    .is_some()
    {
        return CameraProtocol::Isapi;
    }
    if camera.configuration.onvif_port.is_some() || camera.info.ports.onvif.is_some() {
        return CameraProtocol::Onvif;
    }
    CameraProtocol::RtspOnly
}

fn empty_sections_map() -> Map<String, Value> {
    let mut map = Map::new();
    for section in ALL_SECTIONS {
        map.insert(section.to_owned(), Value::Null);
    }
    map
}

fn read_camera_settings(
    camera: &CameraEntry,
    deadline: Instant,
) -> Result<DeviceSnapshot, ControlCommandError> {
    match detect_protocol(camera) {
        CameraProtocol::Reolink => read_reolink_settings(camera, deadline),
        CameraProtocol::Isapi => read_isapi_settings(camera, deadline),
        CameraProtocol::Onvif => read_onvif_settings(camera, deadline),
        CameraProtocol::RtspOnly => Ok(DeviceSnapshot {
            protocol: CameraProtocol::RtspOnly,
            supported_sections: Vec::new(),
            settings: empty_sections_map(),
        }),
    }
}

fn execute_protocol_update(
    camera: &CameraEntry,
    protocol: CameraProtocol,
    requested: &Map<String, Value>,
    dry_run: bool,
    deadline: Instant,
) -> Result<UpdateOutcome, ControlCommandError> {
    for section in requested.keys() {
        if !ALL_SECTIONS.contains(&section.as_str()) {
            return Err(invalid(&format!(
                "unknown device settings section '{section}'"
            )));
        }
    }
    match protocol {
        CameraProtocol::Reolink => update_reolink_settings(camera, requested, dry_run, deadline),
        CameraProtocol::Isapi => update_isapi_settings(camera, requested, dry_run, deadline),
        CameraProtocol::Onvif => update_onvif_settings(camera, requested, dry_run, deadline),
        CameraProtocol::RtspOnly => {
            let unsupported = requested.keys().cloned().collect::<Vec<_>>();
            if !dry_run {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::UnsupportedRequest,
                    501,
                    "RTSP-only cameras do not support on-camera device setting updates",
                ));
            }
            Ok(UpdateOutcome {
                validated: false,
                diff_before: Map::new(),
                diff_after: Map::new(),
                unsupported,
                applied_sections: Vec::new(),
                verified_settings: Map::new(),
                warnings: Vec::new(),
                restart_or_reconnect_expected: false,
            })
        }
    }
}

fn open_reolink_client(
    camera: &CameraEntry,
    deadline: Instant,
) -> Result<ReolinkClient, ControlCommandError> {
    let _ = remaining_timeout(deadline)?;
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
    client
        .login(username, password)
        .map_err(|_| unavailable("Reolink camera authentication or connection failed"))?;
    Ok(client)
}

fn read_reolink_settings(
    camera: &CameraEntry,
    deadline: Instant,
) -> Result<DeviceSnapshot, ControlCommandError> {
    let mut client = open_reolink_client(camera, deadline)?;
    let mut supported = Vec::new();
    let mut settings = empty_sections_map();
    let _ = remaining_timeout(deadline)?;
    if let Some(encoder) = read_reolink_encoder(&client) {
        supported.push("encoder");
        settings.insert("encoder".to_owned(), encoder);
    }
    let _ = remaining_timeout(deadline)?;
    if let Some(imaging) = read_reolink_imaging(&client) {
        supported.push("imaging");
        settings.insert("imaging".to_owned(), imaging);
    }
    let _ = remaining_timeout(deadline)?;
    if let Some(lighting) = read_reolink_lighting(&client) {
        supported.push("lighting");
        settings.insert("lighting".to_owned(), lighting);
    }
    let _ = remaining_timeout(deadline)?;
    if let Some(motion) = read_reolink_motion(&client) {
        supported.push("motion");
        settings.insert("motion".to_owned(), motion);
    }
    read_reolink_tail_sections(&client, deadline, &mut supported, &mut settings)?;
    let _ = client.logout();
    Ok(DeviceSnapshot {
        protocol: CameraProtocol::Reolink,
        supported_sections: supported,
        settings,
    })
}

fn read_reolink_tail_sections(
    client: &ReolinkClient,
    deadline: Instant,
    supported: &mut Vec<&'static str>,
    settings: &mut Map<String, Value>,
) -> Result<(), ControlCommandError> {
    let _ = remaining_timeout(deadline)?;
    if let Ok(channel_name) = client.get_osd(0) {
        supported.push("osd");
        settings.insert(
            "osd".to_owned(),
            json!({
                "channel_name": channel_name,
                "max_chars": REOLINK_OSD_CHARS_MAX,
            }),
        );
    }
    let _ = remaining_timeout(deadline)?;
    if let Some(time) = read_reolink_time(client) {
        supported.push("time");
        settings.insert("time".to_owned(), time);
    }
    let _ = remaining_timeout(deadline)?;
    if let Some(ptz) = read_reolink_ptz(client) {
        supported.push("ptz");
        settings.insert("ptz".to_owned(), ptz);
    }
    Ok(())
}

fn read_reolink_encoder(client: &ReolinkClient) -> Option<Value> {
    let profiles = client.get_enc(0).ok();
    let audio = client.get_audio_cfg(0).ok();
    if profiles.is_none() && audio.is_none() {
        return None;
    }
    let streams = profiles.unwrap_or_default().into_iter().map(|profile| {
        json!({
            "name": profile.name,
            "video": profile.video,
            "audio": profile.audio,
        })
    });
    Some(json!({
        "streams": streams.collect::<Vec<_>>(),
        "audio": audio,
    }))
}

fn read_reolink_imaging(client: &ReolinkClient) -> Option<Value> {
    let image = client.get_image(0).ok();
    let isp = client.get_isp(0).ok();
    if image.is_none() && isp.is_none() {
        return None;
    }
    Some(json!({
        "image": image,
        "isp": isp,
        "ranges": { "brightness": [0, 255], "contrast": [0, 255], "saturation": [0, 255], "sharpness": [0, 255] },
    }))
}

fn read_reolink_lighting(client: &ReolinkClient) -> Option<Value> {
    let ir_mode = client.get_ir_lights(0).ok();
    let white_led = client.get_white_led(0).ok();
    if ir_mode.is_none() && white_led.is_none() {
        return None;
    }
    Some(json!({
        "ir_mode": ir_mode,
        "white_led": white_led,
        "options": { "ir_mode": ["auto", "on", "off"], "white_led_brightness": [0, 100] },
    }))
}

fn read_reolink_motion(client: &ReolinkClient) -> Option<Value> {
    let enabled = client.motion_enabled(0).ok();
    let md_alarm = client.get_md_alarm(0).ok();
    let ai_people = client.get_ai_alarm(0, "people").ok();
    let ai_vehicle = client.get_ai_alarm(0, "vehicle").ok();
    if enabled.is_none() && md_alarm.is_none() && ai_people.is_none() && ai_vehicle.is_none() {
        return None;
    }
    Some(json!({
        "enabled": enabled,
        "md_alarm": md_alarm,
        "ai_alarm": { "people": ai_people, "vehicle": ai_vehicle },
        "ranges": { "sensitivity": [0, REOLINK_MD_SENSITIVITY_MAX], "ai_sensitivity": [0, REOLINK_AI_SENSITIVITY_MAX] },
    }))
}

fn read_reolink_time(client: &ReolinkClient) -> Option<Value> {
    let time = client.get_time().ok();
    let ntp = client.get_ntp().ok();
    if time.is_none() && ntp.is_none() {
        return None;
    }
    Some(json!({ "time": time, "ntp": ntp }))
}

fn read_reolink_ptz(client: &ReolinkClient) -> Option<Value> {
    let presets = client.get_ptz_presets(0).ok();
    let guard = client.get_ptz_guard(0).ok();
    if presets.is_none() && guard.is_none() {
        return None;
    }
    Some(json!({ "presets": presets, "guard": guard }))
}

struct ReolinkImageUpdate {
    brightness: Option<u32>,
    contrast: Option<u32>,
    saturation: Option<u32>,
    sharpness: Option<u32>,
}

struct ReolinkIspUpdate {
    day_night: Option<String>,
    hdr: Option<u32>,
    exposure: Option<String>,
    back_light: Option<String>,
}

#[derive(Default)]
struct ReolinkPlan {
    imaging_image: Option<ReolinkImageUpdate>,
    imaging_isp: Option<ReolinkIspUpdate>,
    ir_mode: Option<IrCutMode>,
    white_led: Option<(u32, Option<u32>, Option<u32>)>,
    motion_enabled: Option<bool>,
    motion_sensitivity: Option<u32>,
    ai_people_sensitivity: Option<u32>,
    ai_vehicle_sensitivity: Option<u32>,
    osd_name: Option<String>,
    ntp: Option<(String, u16)>,
    ptz_guard: Option<(bool, Option<String>)>,
    sections: Vec<String>,
    unsupported: Vec<String>,
    diff_after: Map<String, Value>,
}

fn update_reolink_settings(
    camera: &CameraEntry,
    requested: &Map<String, Value>,
    dry_run: bool,
    deadline: Instant,
) -> Result<UpdateOutcome, ControlCommandError> {
    let plan = validate_reolink_plan(requested)?;
    let before = read_reolink_settings(camera, deadline)?;
    let mut diff_before = Map::new();
    for section in &plan.sections {
        if let Some(value) = before.settings.get(section) {
            diff_before.insert(section.clone(), value.clone());
        }
    }
    if dry_run {
        return Ok(UpdateOutcome {
            validated: !plan.sections.is_empty(),
            diff_before,
            diff_after: plan.diff_after,
            unsupported: plan.unsupported,
            applied_sections: plan.sections,
            verified_settings: Map::new(),
            warnings: Vec::new(),
            restart_or_reconnect_expected: false,
        });
    }
    if plan.sections.is_empty() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::UnsupportedRequest,
            501,
            "none of the requested sections are writable on this Reolink camera",
        ));
    }
    let mut client = open_reolink_client(camera, deadline)?;
    apply_reolink_plan(&client, &plan, deadline)?;
    let _ = client.logout();
    let after = read_reolink_settings(camera, deadline)?;
    let mut verified = Map::new();
    for section in &plan.sections {
        if let Some(value) = after.settings.get(section) {
            verified.insert(section.clone(), value.clone());
        }
    }
    Ok(UpdateOutcome {
        validated: true,
        diff_before,
        diff_after: plan.diff_after,
        unsupported: plan.unsupported,
        applied_sections: plan.sections,
        verified_settings: verified,
        warnings: Vec::new(),
        restart_or_reconnect_expected: false,
    })
}

fn validate_reolink_plan(
    requested: &Map<String, Value>,
) -> Result<ReolinkPlan, ControlCommandError> {
    let mut plan = ReolinkPlan::default();
    for (section, value) in requested {
        let obj = value
            .as_object()
            .ok_or_else(|| invalid(&format!("section '{section}' must be a JSON object")))?;
        match section.as_str() {
            "imaging" => parse_reolink_imaging_update(obj, &mut plan)?,
            "lighting" => parse_reolink_lighting_update(obj, &mut plan)?,
            "motion" => parse_reolink_motion_update(obj, &mut plan)?,
            "osd" => parse_reolink_osd_update(obj, &mut plan)?,
            "time" => parse_reolink_time_update(obj, &mut plan)?,
            "ptz" => parse_reolink_ptz_update(obj, &mut plan)?,
            "encoder" => plan.unsupported.push("encoder".to_owned()),
            _ => plan.unsupported.push(section.clone()),
        }
    }
    Ok(plan)
}

fn parse_reolink_imaging_update(
    obj: &Map<String, Value>,
    plan: &mut ReolinkPlan,
) -> Result<(), ControlCommandError> {
    let brightness = optional_u32_range(obj, "brightness", 0, REOLINK_IMAGE_VALUE_MAX)?;
    let contrast = optional_u32_range(obj, "contrast", 0, REOLINK_IMAGE_VALUE_MAX)?;
    let saturation = optional_u32_range(obj, "saturation", 0, REOLINK_IMAGE_VALUE_MAX)?;
    let sharpness = optional_u32_range(obj, "sharpness", 0, REOLINK_IMAGE_VALUE_MAX)?;
    let day_night = optional_enum_str(obj, "day_night", &["Auto", "Color", "Black&White"])?;
    let hdr = optional_u32_range(obj, "hdr", 0, 2)?;
    let exposure = optional_printable_str(obj, "exposure", 32)?;
    let back_light = optional_printable_str(obj, "back_light", 32)?;
    if brightness.is_none()
        && contrast.is_none()
        && saturation.is_none()
        && sharpness.is_none()
        && day_night.is_none()
        && hdr.is_none()
        && exposure.is_none()
        && back_light.is_none()
    {
        return Err(invalid(
            "imaging update must specify at least one supported field",
        ));
    }
    if brightness.is_some() || contrast.is_some() || saturation.is_some() || sharpness.is_some() {
        plan.imaging_image = Some(ReolinkImageUpdate {
            brightness,
            contrast,
            saturation,
            sharpness,
        });
    }
    if day_night.is_some() || hdr.is_some() || exposure.is_some() || back_light.is_some() {
        plan.imaging_isp = Some(ReolinkIspUpdate {
            day_night,
            hdr,
            exposure,
            back_light,
        });
    }
    plan.sections.push("imaging".to_owned());
    plan.diff_after
        .insert("imaging".to_owned(), Value::Object(obj.clone()));
    Ok(())
}

fn parse_reolink_lighting_update(
    obj: &Map<String, Value>,
    plan: &mut ReolinkPlan,
) -> Result<(), ControlCommandError> {
    if let Some(mode) = optional_printable_str(obj, "ir_mode", 16)? {
        plan.ir_mode = Some(match mode.to_ascii_lowercase().as_str() {
            "auto" => IrCutMode::Auto,
            "on" => IrCutMode::On,
            "off" => IrCutMode::Off,
            _ => return Err(invalid("lighting.ir_mode must be auto, on, or off")),
        });
    }
    if let Some(led) = obj.get("white_led").and_then(Value::as_object) {
        let state = optional_u32_range(led, "state", 0, 1)?
            .ok_or_else(|| invalid("lighting.white_led.state must be 0 or 1"))?;
        let bright = optional_u32_range(led, "brightness", 0, 100)?;
        let mode = optional_u32_range(led, "mode", 0, 3)?;
        plan.white_led = Some((state, bright, mode));
    }
    if plan.ir_mode.is_none() && plan.white_led.is_none() {
        return Err(invalid("lighting update must specify ir_mode or white_led"));
    }
    plan.sections.push("lighting".to_owned());
    plan.diff_after
        .insert("lighting".to_owned(), Value::Object(obj.clone()));
    Ok(())
}

fn parse_reolink_motion_update(
    obj: &Map<String, Value>,
    plan: &mut ReolinkPlan,
) -> Result<(), ControlCommandError> {
    plan.motion_enabled = optional_bool(obj, "enabled")?;
    plan.motion_sensitivity =
        optional_u32_range(obj, "sensitivity", 0, REOLINK_MD_SENSITIVITY_MAX)?;
    plan.ai_people_sensitivity =
        optional_u32_range(obj, "ai_people_sensitivity", 0, REOLINK_AI_SENSITIVITY_MAX)?;
    plan.ai_vehicle_sensitivity =
        optional_u32_range(obj, "ai_vehicle_sensitivity", 0, REOLINK_AI_SENSITIVITY_MAX)?;
    if plan.motion_enabled.is_none()
        && plan.motion_sensitivity.is_none()
        && plan.ai_people_sensitivity.is_none()
        && plan.ai_vehicle_sensitivity.is_none()
    {
        return Err(invalid(
            "motion update must specify at least one supported field",
        ));
    }
    plan.sections.push("motion".to_owned());
    plan.diff_after
        .insert("motion".to_owned(), Value::Object(obj.clone()));
    Ok(())
}

fn parse_reolink_osd_update(
    obj: &Map<String, Value>,
    plan: &mut ReolinkPlan,
) -> Result<(), ControlCommandError> {
    let name = optional_printable_str(obj, "channel_name", REOLINK_OSD_CHARS_MAX)?
        .ok_or_else(|| invalid("osd.channel_name is required"))?;
    plan.osd_name = Some(name);
    plan.sections.push("osd".to_owned());
    plan.diff_after
        .insert("osd".to_owned(), Value::Object(obj.clone()));
    Ok(())
}

fn parse_reolink_time_update(
    obj: &Map<String, Value>,
    plan: &mut ReolinkPlan,
) -> Result<(), ControlCommandError> {
    let server = optional_printable_str(obj, "ntp_server", 128)?
        .ok_or_else(|| invalid("time.ntp_server is required"))?;
    if server.chars().any(char::is_whitespace) {
        return Err(invalid("time.ntp_server must not contain whitespace"));
    }
    let port = optional_u32_range(obj, "ntp_port", 1, u32::from(u16::MAX))?
        .map_or(123, |value| u16::try_from(value).unwrap_or(123));
    plan.ntp = Some((server, port));
    plan.sections.push("time".to_owned());
    plan.diff_after
        .insert("time".to_owned(), Value::Object(obj.clone()));
    Ok(())
}

fn parse_reolink_ptz_update(
    obj: &Map<String, Value>,
    plan: &mut ReolinkPlan,
) -> Result<(), ControlCommandError> {
    let enable = optional_bool(obj, "guard_enabled")?
        .ok_or_else(|| invalid("ptz.guard_enabled is required"))?;
    let preset_cmd = optional_printable_str(obj, "guard_preset_cmd", 32)?;
    plan.ptz_guard = Some((enable, preset_cmd));
    plan.sections.push("ptz".to_owned());
    plan.diff_after
        .insert("ptz".to_owned(), Value::Object(obj.clone()));
    Ok(())
}

fn apply_reolink_plan(
    client: &ReolinkClient,
    plan: &ReolinkPlan,
    deadline: Instant,
) -> Result<(), ControlCommandError> {
    if let Some(img) = &plan.imaging_image {
        let _ = remaining_timeout(deadline)?;
        client
            .set_image(
                0,
                img.brightness,
                img.contrast,
                img.saturation,
                img.sharpness,
            )
            .map_err(|_| unavailable("Reolink SetImage failed"))?;
    }
    if let Some(isp) = &plan.imaging_isp {
        let _ = remaining_timeout(deadline)?;
        client
            .set_isp(
                0,
                isp.day_night.as_deref(),
                isp.hdr,
                isp.exposure.as_deref(),
                isp.back_light.as_deref(),
            )
            .map_err(|_| unavailable("Reolink SetIsp failed"))?;
    }
    if let Some(mode) = plan.ir_mode.clone() {
        let _ = remaining_timeout(deadline)?;
        client
            .set_ir_lights(0, mode)
            .map_err(|_| unavailable("Reolink SetIrLights failed"))?;
    }
    if let Some((state, bright, mode)) = plan.white_led {
        let _ = remaining_timeout(deadline)?;
        client
            .set_white_led(0, state, bright, mode)
            .map_err(|_| unavailable("Reolink SetWhiteLed failed"))?;
    }
    apply_reolink_tail_plan(client, plan, deadline)
}

fn apply_reolink_tail_plan(
    client: &ReolinkClient,
    plan: &ReolinkPlan,
    deadline: Instant,
) -> Result<(), ControlCommandError> {
    if let Some(enabled) = plan.motion_enabled {
        let _ = remaining_timeout(deadline)?;
        client
            .set_alarm(0, enabled)
            .map_err(|_| unavailable("Reolink SetAlarm failed"))?;
    }
    if let Some(sensitivity) = plan.motion_sensitivity {
        let _ = remaining_timeout(deadline)?;
        client
            .set_md_alarm(0, sensitivity)
            .map_err(|_| unavailable("Reolink SetMdAlarm failed"))?;
    }
    if let Some(sensitivity) = plan.ai_people_sensitivity {
        let _ = remaining_timeout(deadline)?;
        client
            .set_ai_alarm(0, "people", sensitivity)
            .map_err(|_| unavailable("Reolink SetAiAlarm people failed"))?;
    }
    if let Some(sensitivity) = plan.ai_vehicle_sensitivity {
        let _ = remaining_timeout(deadline)?;
        client
            .set_ai_alarm(0, "vehicle", sensitivity)
            .map_err(|_| unavailable("Reolink SetAiAlarm vehicle failed"))?;
    }
    if let Some(name) = &plan.osd_name {
        let _ = remaining_timeout(deadline)?;
        client
            .set_osd(0, name)
            .map_err(|_| unavailable("Reolink SetOsd failed"))?;
    }
    if let Some((server, port)) = &plan.ntp {
        let _ = remaining_timeout(deadline)?;
        client
            .set_ntp(server, *port)
            .map_err(|_| unavailable("Reolink SetNtp failed"))?;
    }
    if let Some((enable, cmd)) = &plan.ptz_guard {
        let _ = remaining_timeout(deadline)?;
        client
            .set_ptz_guard(0, *enable, cmd.as_deref())
            .map_err(|_| unavailable("Reolink SetPtzGuard failed"))?;
    }
    Ok(())
}

fn open_isapi_client(
    camera: &CameraEntry,
    deadline: Instant,
) -> Result<(::isapi::blocking::Client, crate::isapi::Route), ControlCommandError> {
    let _ = remaining_timeout(deadline)?;
    let route = crate::isapi::Route::for_control(
        &camera.configuration,
        camera.info.ports.http,
        camera.info.manufacturer.as_deref(),
    )
    .ok_or_else(|| unavailable("Hikvision ISAPI route is unavailable for this camera"))?;
    let client = ::isapi::blocking::Client::new(
        &route.origin,
        ::isapi::Credentials::new(
            camera.configuration.username.clone(),
            camera.configuration.password.clone(),
        ),
    )
    .map_err(|_| unavailable("Hikvision ISAPI client could not be initialized"))?;
    Ok((client, route))
}

fn read_isapi_settings(
    camera: &CameraEntry,
    deadline: Instant,
) -> Result<DeviceSnapshot, ControlCommandError> {
    let (mut client, route) = open_isapi_client(camera, deadline)?;
    let mut supported = Vec::new();
    let mut settings = empty_sections_map();
    let _ = remaining_timeout(deadline)?;
    if let Ok(query) = Stream::list()
        && let Ok(streams) = client.query(&query)
    {
        supported.push("encoder");
        let items = streams.iter().map(isapi_stream_json).collect::<Vec<_>>();
        settings.insert(
            "encoder".to_owned(),
            json!({
                "streams": items,
                "ranges": {
                    "width": [1, ISAPI_DIMENSION_MAX],
                    "height": [1, ISAPI_DIMENSION_MAX],
                    "bitrate_kbps": [1, ISAPI_BITRATE_KBPS_MAX],
                    "frame_rate_hundredths": [1, ISAPI_FRAME_RATE_HUNDREDTHS_MAX],
                },
            }),
        );
    }
    let _ = remaining_timeout(deadline)?;
    if let Some(motion) = read_isapi_motion(&mut client, route.channel) {
        supported.push("motion");
        settings.insert("motion".to_owned(), motion);
    }
    let _ = remaining_timeout(deadline)?;
    if let Ok(query) = Time::query()
        && let Ok(time) = client.query(&query)
    {
        supported.push("time");
        settings.insert("time".to_owned(), isapi_time_json(&time));
    }
    let _ = remaining_timeout(deadline)?;
    if let Some(ptz) = read_isapi_ptz(&mut client, route.channel) {
        supported.push("ptz");
        settings.insert("ptz".to_owned(), ptz);
    }
    Ok(DeviceSnapshot {
        protocol: CameraProtocol::Isapi,
        supported_sections: supported,
        settings,
    })
}

fn isapi_stream_json(stream: &Stream) -> Value {
    let doc = stream.document();
    json!({
        "id": stream.id(),
        "codec": isapi_doc_field(doc, &["Video", "videoCodecType"]),
        "width": isapi_doc_u32(doc, &["Video", "videoResolutionWidth"]),
        "height": isapi_doc_u32(doc, &["Video", "videoResolutionHeight"]),
        "bitrate_kbps": isapi_doc_u32(doc, &["Video", "constantBitRate"]),
        "frame_rate_hundredths": isapi_doc_u32(doc, &["Video", "maxFrameRate"]),
    })
}

fn isapi_doc_u32(doc: &::isapi::Document, path: &[&str]) -> Option<u32> {
    isapi_doc_field(doc, path)?.parse::<u32>().ok()
}

fn isapi_doc_field(doc: &::isapi::Document, path: &[&str]) -> Option<String> {
    let serialized = serde_json::to_value(doc).ok()?;
    let mut node = serialized.get("Xml")?;
    for segment in path {
        let children = node.get("content")?.as_array()?;
        node = children.iter().find_map(|item| {
            let elem = item.get("Element")?;
            let name = elem.get("name")?.as_str()?;
            let local = name.rsplit(':').next().unwrap_or(name);
            (local == *segment).then_some(elem)
        })?;
    }
    let mut text = String::new();
    for item in node.get("content")?.as_array()? {
        if let Some(part) = item.get("Text").and_then(Value::as_str) {
            text.push_str(part);
        }
    }
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn read_isapi_motion(client: &mut ::isapi::blocking::Client, channel: u32) -> Option<Value> {
    let motion = Motion::query(channel)
        .ok()
        .and_then(|query| client.query(&query).ok());
    let mut rules = Map::new();
    for (label, kind) in [
        ("line", RuleKind::Line),
        ("field", RuleKind::Field),
        ("region_entrance", RuleKind::RegionEntrance),
        ("region_exit", RuleKind::RegionExit),
    ] {
        if let Ok(query) = Rule::query(kind, channel)
            && let Ok(rule) = client.query(&query)
        {
            rules.insert(label.to_owned(), Value::Bool(rule.enabled()));
        }
    }
    if motion.is_none() && rules.is_empty() {
        return None;
    }
    Some(json!({
        "enabled": motion.as_ref().map(Motion::enabled),
        "sensitivity": motion.as_ref().and_then(Motion::sensitivity),
        "rules": Value::Object(rules),
        "ranges": { "sensitivity": [0, ISAPI_SENSITIVITY_MAX] },
    }))
}

fn isapi_time_json(time: &Time) -> Value {
    json!({
        "mode": match time.mode() {
            TimeMode::Manual => "manual",
            TimeMode::Ntp => "ntp",
        },
        "local_time": time.local_time(),
        "zone": time.zone(),
    })
}

fn read_isapi_ptz(client: &mut ::isapi::blocking::Client, channel: u32) -> Option<Value> {
    let presets = Preset::list(channel)
        .ok()
        .and_then(|query| client.query(&query).ok());
    let caps = PtzCapabilities::query(channel)
        .ok()
        .and_then(|query| client.query(&query).ok());
    if presets.is_none() && caps.is_none() {
        return None;
    }
    let preset_list = presets
        .unwrap_or_default()
        .into_iter()
        .map(|preset| json!({ "id": preset.id(), "name": preset.name() }))
        .collect::<Vec<_>>();
    Some(json!({
        "presets": preset_list,
        "pan_range": caps.as_ref().and_then(PtzCapabilities::pan_range),
        "tilt_range": caps.as_ref().and_then(PtzCapabilities::tilt_range),
        "zoom_range": caps.as_ref().and_then(PtzCapabilities::zoom_range),
    }))
}

#[derive(Default)]
struct IsapiPlan {
    stream: Option<(u32, Stream)>,
    motion: Option<Motion>,
    rules: Vec<(RuleKind, Rule)>,
    time: Option<Time>,
    preset: Option<Preset>,
    sections: Vec<String>,
    unsupported: Vec<String>,
    diff_before: Map<String, Value>,
    diff_after: Map<String, Value>,
    restart_expected: bool,
}

fn update_isapi_settings(
    camera: &CameraEntry,
    requested: &Map<String, Value>,
    dry_run: bool,
    deadline: Instant,
) -> Result<UpdateOutcome, ControlCommandError> {
    let (mut client, route) = open_isapi_client(camera, deadline)?;
    let plan = build_isapi_plan(&mut client, route.channel, requested, deadline)?;
    if dry_run {
        return Ok(UpdateOutcome {
            validated: !plan.sections.is_empty(),
            diff_before: plan.diff_before,
            diff_after: plan.diff_after,
            unsupported: plan.unsupported,
            applied_sections: plan.sections,
            verified_settings: Map::new(),
            warnings: Vec::new(),
            restart_or_reconnect_expected: plan.restart_expected,
        });
    }
    if plan.sections.is_empty() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::UnsupportedRequest,
            501,
            "none of the requested sections are writable on this ISAPI camera",
        ));
    }
    let reboot_required = apply_isapi_plan(&mut client, route.channel, &plan, deadline)?;
    let verified = read_isapi_settings(camera, deadline)?;
    let mut verified_map = Map::new();
    for section in &plan.sections {
        if let Some(value) = verified.settings.get(section) {
            verified_map.insert(section.clone(), value.clone());
        }
    }
    Ok(UpdateOutcome {
        validated: true,
        diff_before: plan.diff_before,
        diff_after: plan.diff_after,
        unsupported: plan.unsupported,
        applied_sections: plan.sections,
        verified_settings: verified_map,
        warnings: Vec::new(),
        restart_or_reconnect_expected: plan.restart_expected || reboot_required,
    })
}

fn build_isapi_plan(
    client: &mut ::isapi::blocking::Client,
    channel: u32,
    requested: &Map<String, Value>,
    deadline: Instant,
) -> Result<IsapiPlan, ControlCommandError> {
    let mut plan = IsapiPlan::default();
    for (section, value) in requested {
        let _ = remaining_timeout(deadline)?;
        let obj = value
            .as_object()
            .ok_or_else(|| invalid(&format!("section '{section}' must be a JSON object")))?;
        match section.as_str() {
            "encoder" => plan_isapi_encoder(client, channel, obj, &mut plan)?,
            "motion" => plan_isapi_motion(client, channel, obj, &mut plan)?,
            "time" => plan_isapi_time(client, obj, &mut plan)?,
            "ptz" => plan_isapi_ptz(client, channel, obj, &mut plan)?,
            _ => plan.unsupported.push(section.clone()),
        }
    }
    Ok(plan)
}

fn plan_isapi_encoder(
    client: &mut ::isapi::blocking::Client,
    channel: u32,
    obj: &Map<String, Value>,
    plan: &mut IsapiPlan,
) -> Result<(), ControlCommandError> {
    let default_stream = channel.saturating_mul(100).saturating_add(1);
    let stream_id = optional_u32_range(obj, "stream_id", 1, 99_999)?.unwrap_or(default_stream);
    let query = Stream::query(stream_id).map_err(|_| invalid("encoder.stream_id is invalid"))?;
    let mut current = client
        .query(&query)
        .map_err(|_| unavailable("ISAPI stream query failed"))?;
    let before = isapi_stream_json(&current);
    let width = optional_u32_range(obj, "width", 1, ISAPI_DIMENSION_MAX)?
        .or_else(|| isapi_doc_u32(current.document(), &["Video", "videoResolutionWidth"]))
        .ok_or_else(|| invalid("encoder.width is required"))?;
    let height = optional_u32_range(obj, "height", 1, ISAPI_DIMENSION_MAX)?
        .or_else(|| isapi_doc_u32(current.document(), &["Video", "videoResolutionHeight"]))
        .ok_or_else(|| invalid("encoder.height is required"))?;
    let bitrate = optional_u32_range(obj, "bitrate_kbps", 1, ISAPI_BITRATE_KBPS_MAX)?
        .or_else(|| isapi_doc_u32(current.document(), &["Video", "constantBitRate"]))
        .ok_or_else(|| invalid("encoder.bitrate_kbps is required"))?;
    let fps = optional_u32_range(
        obj,
        "frame_rate_hundredths",
        1,
        ISAPI_FRAME_RATE_HUNDREDTHS_MAX,
    )?
    .or_else(|| isapi_doc_u32(current.document(), &["Video", "maxFrameRate"]))
    .ok_or_else(|| invalid("encoder.frame_rate_hundredths is required"))?;
    current
        .set_video(width, height, bitrate, fps)
        .map_err(|_| invalid("encoder parameters are out of ISAPI range"))?;
    plan.diff_before.insert("encoder".to_owned(), before);
    plan.diff_after
        .insert("encoder".to_owned(), isapi_stream_json(&current));
    plan.stream = Some((stream_id, current));
    plan.sections.push("encoder".to_owned());
    plan.restart_expected = true;
    Ok(())
}

fn plan_isapi_motion(
    client: &mut ::isapi::blocking::Client,
    channel: u32,
    obj: &Map<String, Value>,
    plan: &mut IsapiPlan,
) -> Result<(), ControlCommandError> {
    let enabled = optional_bool(obj, "enabled")?;
    let sensitivity = optional_u32_range(obj, "sensitivity", 0, ISAPI_SENSITIVITY_MAX)?;
    let rules_obj = obj.get("rules").and_then(Value::as_object);
    if enabled.is_none() && sensitivity.is_none() && rules_obj.is_none() {
        return Err(invalid(
            "motion update must specify enabled, sensitivity, or rules",
        ));
    }
    let before = read_isapi_motion(client, channel).unwrap_or(Value::Null);
    let mut after = before.clone();
    if enabled.is_some() || sensitivity.is_some() {
        let query = Motion::query(channel).map_err(|_| invalid("invalid camera channel"))?;
        let mut motion = client
            .query(&query)
            .map_err(|_| unavailable("ISAPI motion query failed"))?;
        if let Some(enabled) = enabled {
            motion
                .set_enabled(enabled)
                .map_err(|_| invalid("invalid motion enabled value"))?;
            after["enabled"] = Value::Bool(enabled);
        }
        if let Some(sensitivity) = sensitivity {
            motion
                .set_sensitivity(sensitivity)
                .map_err(|_| invalid("motion sensitivity must be between 0 and 100"))?;
            after["sensitivity"] = json!(sensitivity);
        }
        plan.motion = Some(motion);
    }
    if let Some(rules_obj) = rules_obj {
        plan_isapi_rules(client, channel, rules_obj, &mut after, plan)?;
    }
    plan.diff_before.insert("motion".to_owned(), before);
    plan.diff_after.insert("motion".to_owned(), after);
    plan.sections.push("motion".to_owned());
    Ok(())
}

fn plan_isapi_rules(
    client: &mut ::isapi::blocking::Client,
    channel: u32,
    rules_obj: &Map<String, Value>,
    after: &mut Value,
    plan: &mut IsapiPlan,
) -> Result<(), ControlCommandError> {
    for (name, value) in rules_obj {
        let enabled = value
            .as_bool()
            .ok_or_else(|| invalid(&format!("motion.rules.{name} must be a boolean")))?;
        let kind = match name.as_str() {
            "line" => RuleKind::Line,
            "field" => RuleKind::Field,
            "region_entrance" => RuleKind::RegionEntrance,
            "region_exit" => RuleKind::RegionExit,
            _ => return Err(invalid(&format!("unknown ISAPI smart rule '{name}'"))),
        };
        let query = Rule::query(kind, channel).map_err(|_| invalid("invalid rule channel"))?;
        let mut rule = client
            .query(&query)
            .map_err(|_| unavailable("ISAPI smart rule query failed"))?;
        rule.set_enabled(enabled)
            .map_err(|_| invalid("invalid smart rule enabled value"))?;
        after["rules"][name] = Value::Bool(enabled);
        plan.rules.push((kind, rule));
    }
    Ok(())
}

fn plan_isapi_time(
    client: &mut ::isapi::blocking::Client,
    obj: &Map<String, Value>,
    plan: &mut IsapiPlan,
) -> Result<(), ControlCommandError> {
    let query = Time::query().map_err(|_| invalid("invalid ISAPI time query"))?;
    let mut clock = client
        .query(&query)
        .map_err(|_| unavailable("ISAPI time query failed"))?;
    let before = isapi_time_json(&clock);
    let mode = match optional_printable_str(obj, "mode", 16)?.as_deref() {
        Some("manual") => TimeMode::Manual,
        Some("ntp") | Some("NTP") => TimeMode::Ntp,
        None => clock.mode(),
        Some(_) => return Err(invalid("time.mode must be manual or ntp")),
    };
    let local_time = optional_printable_str(obj, "local_time", 64)?
        .or_else(|| clock.local_time().map(str::to_owned))
        .ok_or_else(|| invalid("time.local_time is required in RFC 3339 format"))?;
    let zone = optional_printable_str(obj, "zone", 128)?
        .or_else(|| clock.zone().map(str::to_owned))
        .ok_or_else(|| invalid("time.zone is required"))?;
    clock
        .set(mode, &local_time, &zone)
        .map_err(|_| invalid("time.local_time must be RFC 3339 and time.zone must be valid"))?;
    plan.diff_before.insert("time".to_owned(), before);
    plan.diff_after
        .insert("time".to_owned(), isapi_time_json(&clock));
    plan.time = Some(clock);
    plan.sections.push("time".to_owned());
    Ok(())
}

fn plan_isapi_ptz(
    client: &mut ::isapi::blocking::Client,
    channel: u32,
    obj: &Map<String, Value>,
    plan: &mut IsapiPlan,
) -> Result<(), ControlCommandError> {
    let store = obj
        .get("store_preset")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("ptz update requires store_preset object"))?;
    let id = optional_u32_range(store, "id", 1, ISAPI_PRESET_ID_MAX)?
        .ok_or_else(|| invalid("ptz.store_preset.id must be between 1 and 300"))?;
    let name = optional_printable_str(store, "name", 64)?
        .ok_or_else(|| invalid("ptz.store_preset.name is required"))?;
    let preset = Preset::new(id, &name).map_err(|_| invalid("ptz preset id or name is invalid"))?;
    let before = read_isapi_ptz(client, channel).unwrap_or(Value::Null);
    plan.diff_before.insert("ptz".to_owned(), before);
    plan.diff_after.insert(
        "ptz".to_owned(),
        json!({ "store_preset": { "id": id, "name": name } }),
    );
    plan.preset = Some(preset);
    plan.sections.push("ptz".to_owned());
    Ok(())
}

fn apply_isapi_plan(
    client: &mut ::isapi::blocking::Client,
    channel: u32,
    plan: &IsapiPlan,
    deadline: Instant,
) -> Result<bool, ControlCommandError> {
    let mut reboot_required = false;
    if let Some((_, stream)) = &plan.stream {
        let _ = remaining_timeout(deadline)?;
        let req = stream
            .update()
            .map_err(|_| invalid("ISAPI stream update failed"))?;
        let status = client
            .command(&req)
            .map_err(|_| unavailable("ISAPI stream write failed"))?;
        reboot_required |= status.reboot_required();
    }
    if let Some(motion) = &plan.motion {
        let _ = remaining_timeout(deadline)?;
        let req = motion
            .update(channel)
            .map_err(|_| invalid("ISAPI motion update failed"))?;
        let status = client
            .command(&req)
            .map_err(|_| unavailable("ISAPI motion write failed"))?;
        reboot_required |= status.reboot_required();
    }
    for (_, rule) in &plan.rules {
        let _ = remaining_timeout(deadline)?;
        let req = rule
            .update(channel)
            .map_err(|_| invalid("ISAPI rule update failed"))?;
        let status = client
            .command(&req)
            .map_err(|_| unavailable("ISAPI rule write failed"))?;
        reboot_required |= status.reboot_required();
    }
    if let Some(time) = &plan.time {
        let _ = remaining_timeout(deadline)?;
        let req = time
            .update()
            .map_err(|_| invalid("ISAPI time update failed"))?;
        let status = client
            .command(&req)
            .map_err(|_| unavailable("ISAPI time write failed"))?;
        reboot_required |= status.reboot_required();
    }
    if let Some(preset) = &plan.preset {
        let _ = remaining_timeout(deadline)?;
        let req = preset
            .store(channel)
            .map_err(|_| invalid("ISAPI preset update failed"))?;
        let status = client
            .command(&req)
            .map_err(|_| unavailable("ISAPI preset write failed"))?;
        reboot_required |= status.reboot_required();
    }
    Ok(reboot_required)
}

fn open_onvif_media_client(
    camera: &CameraEntry,
    deadline: Instant,
) -> Result<onvif::soap::client::Client, ControlCommandError> {
    let timeout = remaining_timeout(deadline)?;
    let port = camera
        .configuration
        .onvif_port
        .or(camera.info.ports.onvif)
        .unwrap_or(8000);
    let address = SocketAddr::new(camera.configuration.ip, port);
    let device_url = Url::parse(&format!("http://{address}/onvif/device_service"))
        .map_err(|_| unavailable("ONVIF device URL is invalid"))?;
    let creds = (!camera.configuration.username.is_empty()).then(|| OnvifCredentials {
        username: camera.configuration.username.clone(),
        password: camera.configuration.password.clone(),
    });
    let device_client = ClientBuilder::new(&device_url)
        .credentials(creds.clone())
        .auth_type(AuthType::Any)
        .timeout(timeout)
        .build();
    let media_url = devicemgmt::get_services(
        &device_client,
        &devicemgmt::GetServices {
            include_capability: false,
        },
    )
    .ok()
    .and_then(|resp| {
        resp.service
            .into_iter()
            .find(|service| service.namespace.contains("media/wsdl"))
            .and_then(|service| Url::parse(&service.x_addr).ok())
    })
    .unwrap_or_else(|| {
        Url::parse(&format!("http://{address}/onvif/media_service")).expect("valid fallback URL")
    });
    let timeout = remaining_timeout(deadline)?;
    Ok(ClientBuilder::new(&media_url)
        .credentials(creds)
        .auth_type(AuthType::Any)
        .timeout(timeout)
        .build())
}

fn read_onvif_settings(
    camera: &CameraEntry,
    deadline: Instant,
) -> Result<DeviceSnapshot, ControlCommandError> {
    let media_client = open_onvif_media_client(camera, deadline)?;
    let mut supported = Vec::new();
    let mut settings = empty_sections_map();
    let _ = remaining_timeout(deadline)?;
    if let Ok(resp) = onvif_media::get_video_encoder_configurations(
        &media_client,
        &onvif_media::GetVideoEncoderConfigurations {},
    ) {
        supported.push("encoder");
        let configs = resp
            .configurations
            .into_iter()
            .take(MAX_ONVIF_CONFIGS)
            .map(|config| {
                let options = onvif_media::get_video_encoder_configuration_options(
                    &media_client,
                    &onvif_media::GetVideoEncoderConfigurationOptions {
                        configuration_token: Some(onvif_xsd::ReferenceToken(
                            config.token.0.clone(),
                        )),
                        profile_token: None,
                    },
                )
                .ok()
                .map(|resp| onvif_encoder_options_json(&resp.options));
                onvif_encoder_json(&config, options)
            })
            .collect::<Vec<_>>();
        settings.insert("encoder".to_owned(), json!({ "configurations": configs }));
    }
    let _ = remaining_timeout(deadline)?;
    if let Ok(resp) = onvif_media::get_os_ds(
        &media_client,
        &onvif_media::GetOSDs {
            configuration_token: onvif_xsd::ReferenceToken(String::new()),
        },
    ) {
        supported.push("osd");
        let osds = resp
            .os_ds
            .iter()
            .take(MAX_ONVIF_OSDS)
            .map(onvif_osd_json)
            .collect::<Vec<_>>();
        settings.insert("osd".to_owned(), json!({ "osds": osds }));
    }
    Ok(DeviceSnapshot {
        protocol: CameraProtocol::Onvif,
        supported_sections: supported,
        settings,
    })
}

fn onvif_encoder_json(
    config: &onvif_xsd::VideoEncoderConfiguration,
    options: Option<Value>,
) -> Value {
    json!({
        "token": config.token.0,
        "name": config.name.0,
        "encoding": format!("{:?}", config.encoding),
        "width": config.resolution.width,
        "height": config.resolution.height,
        "quality": config.quality,
        "frame_rate_limit": config.rate_control.as_ref().map(|rc| rc.frame_rate_limit),
        "bitrate_limit": config.rate_control.as_ref().map(|rc| rc.bitrate_limit),
        "gov_length": config.h264.as_ref().map(|h| h.gov_length),
        "options": options,
    })
}

fn onvif_encoder_options_json(options: &onvif_xsd::VideoEncoderConfigurationOptions) -> Value {
    let h264 = options.h264.as_ref().map(|h264| {
        let resolutions = h264
            .resolutions_available
            .iter()
            .map(|res| json!({ "width": res.width, "height": res.height }))
            .collect::<Vec<_>>();
        json!({
            "resolutions": resolutions,
            "frame_rate_range": [h264.frame_rate_range.min, h264.frame_rate_range.max],
            "gov_length_range": [h264.gov_length_range.min, h264.gov_length_range.max],
        })
    });
    json!({
        "quality_range": [options.quality_range.min, options.quality_range.max],
        "h264": h264,
    })
}

fn onvif_osd_json(osd: &onvif_xsd::Osdconfiguration) -> Value {
    json!({
        "token": osd.token.0,
        "type": format!("{:?}", osd._type),
        "plain_text": osd.text_string.as_ref().and_then(|text| text.plain_text.as_deref()),
    })
}

fn update_onvif_settings(
    camera: &CameraEntry,
    requested: &Map<String, Value>,
    dry_run: bool,
    deadline: Instant,
) -> Result<UpdateOutcome, ControlCommandError> {
    let media_client = open_onvif_media_client(camera, deadline)?;
    let mut outcome = UpdateOutcome {
        validated: false,
        diff_before: Map::new(),
        diff_after: Map::new(),
        unsupported: Vec::new(),
        applied_sections: Vec::new(),
        verified_settings: Map::new(),
        warnings: Vec::new(),
        restart_or_reconnect_expected: false,
    };
    let mut pending_encoder = None;
    let mut pending_osd = None;
    for (section, value) in requested {
        let _ = remaining_timeout(deadline)?;
        let obj = value
            .as_object()
            .ok_or_else(|| invalid(&format!("section '{section}' must be a JSON object")))?;
        match section.as_str() {
            "encoder" => {
                let (before, config) = plan_onvif_encoder(&media_client, obj)?;
                outcome.diff_before.insert("encoder".to_owned(), before);
                outcome
                    .diff_after
                    .insert("encoder".to_owned(), onvif_encoder_json(&config, None));
                outcome.applied_sections.push("encoder".to_owned());
                outcome.restart_or_reconnect_expected = true;
                pending_encoder = Some(config);
            }
            "osd" => {
                let (before, osd) = plan_onvif_osd(&media_client, obj)?;
                outcome.diff_before.insert("osd".to_owned(), before);
                outcome
                    .diff_after
                    .insert("osd".to_owned(), onvif_osd_json(&osd));
                outcome.applied_sections.push("osd".to_owned());
                pending_osd = Some(osd);
            }
            _ => outcome.unsupported.push(section.clone()),
        }
    }
    outcome.validated = !outcome.applied_sections.is_empty();
    if dry_run {
        return Ok(outcome);
    }
    if !outcome.validated {
        return Err(ControlCommandError::new(
            proto::ErrorCode::UnsupportedRequest,
            501,
            "none of the requested sections are writable via ONVIF",
        ));
    }
    apply_onvif_updates(&media_client, pending_encoder, pending_osd, deadline)?;
    let verified = read_onvif_settings(camera, deadline)?;
    for section in &outcome.applied_sections {
        if let Some(value) = verified.settings.get(section) {
            outcome
                .verified_settings
                .insert(section.clone(), value.clone());
        }
    }
    Ok(outcome)
}

fn plan_onvif_encoder(
    media_client: &onvif::soap::client::Client,
    obj: &Map<String, Value>,
) -> Result<(Value, onvif_xsd::VideoEncoderConfiguration), ControlCommandError> {
    let configs = onvif_media::get_video_encoder_configurations(
        media_client,
        &onvif_media::GetVideoEncoderConfigurations {},
    )
    .map_err(|_| unavailable("ONVIF GetVideoEncoderConfigurations failed"))?
    .configurations;
    let token_filter = optional_printable_str(obj, "token", 64)?;
    let mut config = configs
        .into_iter()
        .find(|cfg| {
            token_filter
                .as_deref()
                .is_none_or(|token| cfg.token.0 == token)
        })
        .ok_or_else(|| invalid("matching ONVIF video encoder configuration was not found"))?;
    let options = onvif_media::get_video_encoder_configuration_options(
        media_client,
        &onvif_media::GetVideoEncoderConfigurationOptions {
            configuration_token: Some(onvif_xsd::ReferenceToken(config.token.0.clone())),
            profile_token: None,
        },
    )
    .ok()
    .map(|resp| resp.options);
    let before = onvif_encoder_json(&config, options.as_ref().map(onvif_encoder_options_json));
    apply_onvif_encoder_fields(obj, &mut config, options.as_ref())?;
    Ok((before, config))
}

fn apply_onvif_encoder_fields(
    obj: &Map<String, Value>,
    config: &mut onvif_xsd::VideoEncoderConfiguration,
    options: Option<&onvif_xsd::VideoEncoderConfigurationOptions>,
) -> Result<(), ControlCommandError> {
    if let Some(quality) = obj.get("quality").and_then(Value::as_f64) {
        if !quality.is_finite() {
            return Err(invalid("encoder.quality must be finite"));
        }
        if let Some(opts) = options {
            let min = f64::from(opts.quality_range.min);
            let max = f64::from(opts.quality_range.max);
            if quality < min || quality > max {
                return Err(invalid(&format!(
                    "encoder.quality must be within camera range {min}..={max}"
                )));
            }
        }
        config.quality = quality;
    }
    if let Some(fps) = optional_i32(obj, "frame_rate_limit")? {
        if let Some(h264) = options.and_then(|opts| opts.h264.as_ref())
            && (fps < h264.frame_rate_range.min || fps > h264.frame_rate_range.max)
        {
            return Err(invalid(&format!(
                "encoder.frame_rate_limit must be within camera range {}..={}",
                h264.frame_rate_range.min, h264.frame_rate_range.max
            )));
        }
        let rc = config.rate_control.get_or_insert_with(Default::default);
        rc.frame_rate_limit = fps;
    }
    if let Some(bitrate) = optional_i32(obj, "bitrate_limit")? {
        if !(1..=100_000).contains(&bitrate) {
            return Err(invalid(
                "encoder.bitrate_limit must be between 1 and 100000",
            ));
        }
        let rc = config.rate_control.get_or_insert_with(Default::default);
        rc.bitrate_limit = bitrate;
    }
    apply_onvif_resolution_and_gov(obj, config, options)
}

fn apply_onvif_resolution_and_gov(
    obj: &Map<String, Value>,
    config: &mut onvif_xsd::VideoEncoderConfiguration,
    options: Option<&onvif_xsd::VideoEncoderConfigurationOptions>,
) -> Result<(), ControlCommandError> {
    let width = optional_i32(obj, "width")?;
    let height = optional_i32(obj, "height")?;
    if width.is_some() || height.is_some() {
        let next_w = width.unwrap_or(config.resolution.width);
        let next_h = height.unwrap_or(config.resolution.height);
        if let Some(h264) = options.and_then(|opts| opts.h264.as_ref())
            && !h264.resolutions_available.is_empty()
            && !h264
                .resolutions_available
                .iter()
                .any(|res| res.width == next_w && res.height == next_h)
        {
            return Err(invalid(
                "encoder resolution is not in camera-supported resolutions",
            ));
        }
        config.resolution.width = next_w;
        config.resolution.height = next_h;
    }
    if let Some(gov) = optional_i32(obj, "gov_length")? {
        if let Some(h264) = options.and_then(|opts| opts.h264.as_ref())
            && (gov < h264.gov_length_range.min || gov > h264.gov_length_range.max)
        {
            return Err(invalid(&format!(
                "encoder.gov_length must be within camera range {}..={}",
                h264.gov_length_range.min, h264.gov_length_range.max
            )));
        }
        if let Some(h264) = config.h264.as_mut() {
            h264.gov_length = gov;
        }
    }
    Ok(())
}

fn plan_onvif_osd(
    media_client: &onvif::soap::client::Client,
    obj: &Map<String, Value>,
) -> Result<(Value, onvif_xsd::Osdconfiguration), ControlCommandError> {
    let token = optional_printable_str(obj, "token", 64)?
        .ok_or_else(|| invalid("osd.token is required"))?;
    let plain_text = optional_printable_str(obj, "plain_text", MAX_OSD_CHARS)?
        .ok_or_else(|| invalid("osd.plain_text is required"))?;
    let osds = onvif_media::get_os_ds(
        media_client,
        &onvif_media::GetOSDs {
            configuration_token: onvif_xsd::ReferenceToken(String::new()),
        },
    )
    .map_err(|_| unavailable("ONVIF GetOSDs failed"))?
    .os_ds;
    let mut target = osds
        .into_iter()
        .find(|osd| osd.token.0 == token)
        .ok_or_else(|| invalid("matching ONVIF OSD token was not found"))?;
    let before = onvif_osd_json(&target);
    let text_cfg = target.text_string.get_or_insert_with(Default::default);
    text_cfg.plain_text = Some(plain_text);
    Ok((before, target))
}

fn apply_onvif_updates(
    media_client: &onvif::soap::client::Client,
    encoder: Option<onvif_xsd::VideoEncoderConfiguration>,
    osd: Option<onvif_xsd::Osdconfiguration>,
    deadline: Instant,
) -> Result<(), ControlCommandError> {
    if let Some(configuration) = encoder {
        let _ = remaining_timeout(deadline)?;
        onvif_media::set_video_encoder_configuration(
            media_client,
            &onvif_media::SetVideoEncoderConfiguration {
                configuration,
                force_persistence: true,
            },
        )
        .map_err(|_| unavailable("ONVIF SetVideoEncoderConfiguration failed"))?;
    }
    if let Some(osd) = osd {
        let _ = remaining_timeout(deadline)?;
        onvif_media::set_osd(media_client, &onvif_media::SetOSD { osd })
            .map_err(|_| unavailable("ONVIF SetOSD failed"))?;
    }
    Ok(())
}

fn optional_u32_range(
    map: &Map<String, Value>,
    key: &str,
    min: u32,
    max: u32,
) -> Result<Option<u32>, ControlCommandError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let raw = value
        .as_u64()
        .ok_or_else(|| invalid(&format!("{key} must be an unsigned integer")))?;
    let parsed = u32::try_from(raw)
        .ok()
        .filter(|value| (min..=max).contains(value))
        .ok_or_else(|| invalid(&format!("{key} must be between {min} and {max}")))?;
    Ok(Some(parsed))
}

fn optional_i32(map: &Map<String, Value>, key: &str) -> Result<Option<i32>, ControlCommandError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let raw = value
        .as_i64()
        .ok_or_else(|| invalid(&format!("{key} must be an integer")))?;
    let parsed = i32::try_from(raw)
        .map_err(|_| invalid(&format!("{key} is outside valid integer range")))?;
    Ok(Some(parsed))
}

fn optional_printable_str(
    map: &Map<String, Value>,
    key: &str,
    max_chars: usize,
) -> Result<Option<String>, ControlCommandError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value
        .as_str()
        .map(str::trim)
        .filter(|text| {
            !text.is_empty()
                && text.chars().count() <= max_chars
                && !text.chars().any(char::is_control)
        })
        .ok_or_else(|| {
            invalid(&format!(
                "{key} must contain 1 to {max_chars} printable characters"
            ))
        })?;
    Ok(Some(text.to_owned()))
}

fn optional_enum_str(
    map: &Map<String, Value>,
    key: &str,
    allowed: &[&str],
) -> Result<Option<String>, ControlCommandError> {
    let Some(value) = optional_printable_str(map, key, 32)? else {
        return Ok(None);
    };
    if allowed.contains(&value.as_str()) {
        return Ok(Some(value));
    }
    Err(invalid(&format!(
        "{key} must be one of: {}",
        allowed.join(", ")
    )))
}

fn scrub_sensitive_map(map: &mut Map<String, Value>) {
    map.retain(|key, _| !is_sensitive_key(key));
    for value in map.values_mut() {
        scrub_sensitive_value(value, 0);
    }
}

fn scrub_sensitive_value(value: &mut Value, depth: usize) {
    if depth > MAX_SETTINGS_DEPTH {
        return;
    }
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !is_sensitive_key(key));
            for child in map.values_mut() {
                scrub_sensitive_value(child, depth + 1);
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_sensitive_value(item, depth + 1);
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "password"
            | "passwd"
            | "secret"
            | "auth_token"
            | "access_token"
            | "session_token"
            | "access_key"
            | "authorization"
    )
}

fn audit_device_update(
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
        "mcp_camera_device_settings_update",
        Some(camera_id),
        outcome,
        ClientClassificationReason::UnknownSession,
    );
}

fn invalid(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

fn unavailable(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Unavailable, 503, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        access::CameraAccess,
        cameras::{
            Camera, CameraCapabilities, CameraConfig, CameraPorts, CameraRecordingMode, DeviceInfo,
        },
        config::Config,
        storage::{RecordingDemand, StorageConfig},
        webrtc::WebRtc,
    };
    use std::{
        collections::HashMap,
        io::{Read, Write},
        net::{IpAddr, Ipv4Addr, TcpListener},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };
    use test_hikvision::FakeHikvision;

    const SENTINEL_SECRET: &str = "KEEPPEEK-SENTINEL-CAMERA-PASS-9941";

    fn admin_principal() -> ApiPrincipal {
        ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn user_principal(state: &ServerState) -> ApiPrincipal {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let issued = state
            .access_manager
            .create_credential("dev-viewer", None, AccessRole::User, None, now_ms)
            .unwrap();
        state
            .access_manager
            .set_camera_access(
                issued.metadata.id,
                issued.metadata.revision,
                CameraAccess::unrestricted(),
            )
            .unwrap();
        let header = format!("Bearer {}", issued.access_key.canonical());
        let auth = state
            .access_manager
            .authenticate(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &[header.as_str()],
                now_ms,
                Instant::now(),
            )
            .unwrap();
        ApiPrincipal::credential(auth)
    }

    fn base_camera_config(ip: Ipv4Addr, name: &str) -> CameraConfig {
        CameraConfig {
            ip: IpAddr::V4(ip),
            name: Some(name.to_owned()),
            display_name: None,
            manufacturer: None,
            username: "operator".to_owned(),
            password: SENTINEL_SECRET.to_owned(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Auto,
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

    fn build_state(
        camera_config: CameraConfig,
        runtime_camera: Option<Camera>,
    ) -> (ServerState, FacadeSender<RouterMessage>, String) {
        let mut camera_configs = HashMap::new();
        let ip = camera_config.ip;
        camera_configs.insert("default".to_owned(), vec![camera_config]);
        let mut cameras = HashMap::new();
        if let Some(camera) = runtime_camera {
            cameras.insert(ip, camera);
        }
        let config = Config::default();
        let storage = StorageConfig::default();
        let state = ServerState::new(
            &config,
            &camera_configs,
            &cameras,
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        );
        let camera_id = state.camera_entries()[0].info.id.clone();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        (state, router_tx, camera_id)
    }

    fn future_deadline() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    fn hikvision_fixture() -> (
        FakeHikvision,
        ServerState,
        FacadeSender<RouterMessage>,
        String,
    ) {
        let fake = FakeHikvision::builder()
            .credentials("operator", SENTINEL_SECRET)
            .start()
            .unwrap();
        let port = Url::parse(&fake.origin()).unwrap().port().unwrap();
        let mut cfg = base_camera_config(Ipv4Addr::LOCALHOST, "hik-front");
        cfg.manufacturer = Some("Hikvision".to_owned());
        cfg.http_port = Some(port);
        let (state, router_tx, camera_id) = build_state(cfg, None);
        (fake, state, router_tx, camera_id)
    }

    #[test]
    fn mcp_device_hikvision_isapi_read_dry_run_and_policy_gates() {
        let (_fake, state, router_tx, camera_id) = hikvision_fixture();
        let admin = admin_principal();
        let read = get_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({}),
            future_deadline(),
        )
        .unwrap();
        assert_eq!(read["protocol"], "isapi");
        assert!(!read.to_string().contains(SENTINEL_SECRET));
        assert!(
            read["supported_sections"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "motion")
        );
        assert!(
            read["unsupported_sections"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "osd")
        );

        let dry = update_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({ "settings": { "motion": { "enabled": false, "sensitivity": 42 } } }),
            McpDeviceWritePolicy {
                allow_camera_device_writes: false,
            },
            false,
            future_deadline(),
        )
        .unwrap();
        assert_eq!(dry["dry_run"], true);
        assert_eq!(dry["validated"], true);
        assert_eq!(dry["diff"]["after"]["motion"]["sensitivity"], 42);

        for (allow, ro) in [(false, false), (true, true)] {
            let denied = update_camera_device_settings(
                &state,
                &router_tx,
                &admin,
                &camera_id,
                &json!({ "dry_run": false, "settings": { "motion": { "sensitivity": 42 } } }),
                McpDeviceWritePolicy {
                    allow_camera_device_writes: allow,
                },
                ro,
                future_deadline(),
            )
            .unwrap_err();
            assert_eq!(denied._http_status, 403);
        }
    }

    #[test]
    fn mcp_device_hikvision_isapi_apply_readback_and_bounds() {
        let (_fake, state, router_tx, camera_id) = hikvision_fixture();
        let admin = admin_principal();
        let oob = update_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({ "dry_run": true, "settings": { "motion": { "sensitivity": 101 } } }),
            McpDeviceWritePolicy {
                allow_camera_device_writes: true,
            },
            false,
            future_deadline(),
        )
        .unwrap_err();
        assert_eq!(oob._http_status, 400);

        let applied = update_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({
                "dry_run": false,
                "settings": {
                    "motion": { "enabled": false, "sensitivity": 42 },
                    "encoder": { "stream_id": 101, "width": 640, "height": 360, "bitrate_kbps": 1024, "frame_rate_hundredths": 1500 },
                }
            }),
            McpDeviceWritePolicy { allow_camera_device_writes: true },
            false,
            future_deadline(),
        )
        .unwrap();
        assert_eq!(applied["applied"], true);
        assert_eq!(applied["restart_or_reconnect_expected"], true);
        assert_eq!(applied["verified_settings"]["motion"]["enabled"], false);
        assert_eq!(applied["verified_settings"]["motion"]["sensitivity"], 42);
        assert!(!applied.to_string().contains(SENTINEL_SECRET));
    }

    struct FakeReolinkState {
        bright: u32,
        ir_state: String,
        osd_name: String,
        md_enabled: u32,
        md_sens: u32,
    }

    fn spawn_fake_reolink() -> (u16, Arc<AtomicBool>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let state = Arc::new(Mutex::new(FakeReolinkState {
            bright: 128,
            ir_state: "Auto".to_owned(),
            osd_name: "FrontCam".to_owned(),
            md_enabled: 1,
            md_sens: 25,
        }));
        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let req = read_http_request(&mut stream);
                let resp_json = handle_fake_reolink_cmd(&req, &state);
                let body = resp_json.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (port, stop, handle)
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut header_end = None;
        let mut content_len = 0usize;
        while let Ok(n) = stream.read(&mut chunk) {
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if header_end.is_none()
                && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
            {
                header_end = Some(pos + 4);
                let headers = String::from_utf8_lossy(&buf[..pos]);
                for line in headers.lines() {
                    if let Some((k, v)) = line.split_once(':')
                        && k.trim().eq_ignore_ascii_case("content-length")
                    {
                        content_len = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            if let Some(end) = header_end
                && buf.len() >= end + content_len
            {
                break;
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn handle_fake_reolink_cmd(raw: &str, state: &Mutex<FakeReolinkState>) -> Value {
        let body = raw.split_once("\r\n\r\n").map_or("", |(_, b)| b);
        let parsed: Value = serde_json::from_str(body).unwrap_or_default();
        let cmd = parsed[0]["cmd"].as_str().unwrap_or("");
        let param = &parsed[0]["param"];
        let mut st = state.lock().unwrap();
        let value = match cmd {
            "Login" => json!({ "Token": { "name": "tok-123", "leaseTime": 3600 } }),
            "Logout" => json!({ "rspCode": 200 }),
            "GetImage" => {
                json!({ "Image": { "bright": st.bright, "contrast": 128, "saturation": 128, "sharpen": 128 } })
            }
            "SetImage" => {
                if let Some(b) = param["Image"]["bright"].as_u64() {
                    st.bright = b as u32;
                }
                json!({ "rspCode": 200 })
            }
            "GetIrLights" => json!({ "IrLights": { "state": st.ir_state } }),
            "SetIrLights" => {
                if let Some(s) = param["IrLights"]["state"].as_str() {
                    st.ir_state = s.to_owned();
                }
                json!({ "rspCode": 200 })
            }
            "GetOsd" => json!({ "Osd": { "osdChannel": { "name": st.osd_name } } }),
            "SetOsd" => {
                if let Some(n) = param["Osd"]["osdChannel"]["name"].as_str() {
                    st.osd_name = n.to_owned();
                }
                json!({ "rspCode": 200 })
            }
            "GetAlarm" => {
                json!({ "Alarm": { "enable": st.md_enabled, "channel": 0, "type": "md" } })
            }
            "SetAlarm" => {
                if let Some(e) = param["Alarm"]["enable"].as_u64() {
                    st.md_enabled = e as u32;
                }
                json!({ "rspCode": 200 })
            }
            "GetMdAlarm" => json!({ "MdAlarm": { "newSens": { "sensDef": st.md_sens } } }),
            "SetMdAlarm" => {
                if let Some(s) = param["MdAlarm"]["newSens"]["sensDef"].as_u64() {
                    st.md_sens = s as u32;
                }
                json!({ "rspCode": 200 })
            }
            _ => json!({}),
        };
        json!([{ "cmd": cmd, "code": 0, "value": value }])
    }

    fn reolink_fixture(port: u16) -> (ServerState, FacadeSender<RouterMessage>, String) {
        let mut cfg = base_camera_config(Ipv4Addr::LOCALHOST, "reo-yard");
        cfg.http_port = Some(port);
        let runtime = Camera {
            config: cfg.clone(),
            device: DeviceInfo::default(),
            reported_manufacturer: Some("Reolink".to_owned()),
            hostname: None,
            mac_address: None,
            ports: CameraPorts {
                http: Some(port),
                ..Default::default()
            },
            capabilities: CameraCapabilities::default(),
            event_service: None,
            profiles: Vec::new(),
            is_reolink: true,
            ptz: None,
            imaging: None,
        };
        build_state(cfg, Some(runtime))
    }

    #[test]
    fn mcp_device_reolink_read_dry_run_apply_and_zero_leak() {
        let (port, stop, handle) = spawn_fake_reolink();
        let (state, router_tx, camera_id) = reolink_fixture(port);
        let admin = admin_principal();
        let read = get_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({}),
            future_deadline(),
        )
        .unwrap();
        assert_eq!(read["protocol"], "reolink");
        assert_eq!(read["settings"]["osd"]["channel_name"], "FrontCam");
        assert!(!read.to_string().contains(SENTINEL_SECRET));

        let oob = update_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({ "dry_run": true, "settings": { "imaging": { "brightness": 300 } } }),
            McpDeviceWritePolicy {
                allow_camera_device_writes: true,
            },
            false,
            future_deadline(),
        )
        .unwrap_err();
        assert_eq!(oob._http_status, 400);

        let applied = update_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({
                "dry_run": false,
                "settings": {
                    "imaging": { "brightness": 190 },
                    "lighting": { "ir_mode": "off" },
                    "osd": { "channel_name": "Backyard" },
                }
            }),
            McpDeviceWritePolicy {
                allow_camera_device_writes: true,
            },
            false,
            future_deadline(),
        )
        .unwrap();
        assert_eq!(applied["applied"], true);
        assert_eq!(
            applied["verified_settings"]["osd"]["channel_name"],
            "Backyard"
        );
        assert_eq!(
            applied["verified_settings"]["imaging"]["image"]["brightness"],
            190.0
        );
        assert_eq!(applied["verified_settings"]["lighting"]["ir_mode"], "off");
        assert!(!applied.to_string().contains(SENTINEL_SECRET));
        stop.store(true, Ordering::Release);
        let _ = handle.join();
    }

    fn spawn_fake_onvif() -> (u16, Arc<AtomicBool>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let fps = Arc::new(Mutex::new(20i32));
        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let req = read_http_request(&mut stream);
                let body = handle_fake_onvif_soap(&req, port, &fps);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/soap+xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (port, stop, handle)
    }

    fn handle_fake_onvif_soap(raw: &str, port: u16, fps: &Mutex<i32>) -> String {
        let inner = if raw.contains("GetServices") {
            format!(
                r#"<tds:GetServicesResponse xmlns:tds="http://www.onvif.org/ver10/device/wsdl">
                    <tds:Service>
                        <tds:Namespace>http://www.onvif.org/ver10/media/wsdl</tds:Namespace>
                        <tds:XAddr>http://127.0.0.1:{port}/onvif/media_service</tds:XAddr>
                        <tds:Version><tt:Major>2</tt:Major><tt:Minor>0</tt:Minor></tds:Version>
                    </tds:Service>
                </tds:GetServicesResponse>"#
            )
        } else if raw.contains("GetVideoEncoderConfigurationOptions") {
            r#"<trt:GetVideoEncoderConfigurationOptionsResponse xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
                <trt:Options>
                    <tt:QualityRange><tt:Min>1</tt:Min><tt:Max>6</tt:Max></tt:QualityRange>
                    <tt:H264>
                        <tt:ResolutionsAvailable><tt:Width>1920</tt:Width><tt:Height>1080</tt:Height></tt:ResolutionsAvailable>
                        <tt:GovLengthRange><tt:Min>15</tt:Min><tt:Max>120</tt:Max></tt:GovLengthRange>
                        <tt:FrameRateRange><tt:Min>5</tt:Min><tt:Max>30</tt:Max></tt:FrameRateRange>
                        <tt:EncodingIntervalRange><tt:Min>1</tt:Min><tt:Max>1</tt:Max></tt:EncodingIntervalRange>
                        <tt:H264ProfilesSupported>Main</tt:H264ProfilesSupported>
                    </tt:H264>
                </trt:Options>
            </trt:GetVideoEncoderConfigurationOptionsResponse>"#
                .to_owned()
        } else if raw.contains("SetVideoEncoderConfiguration") {
            if let Some(start) = raw.find("FrameRateLimit>")
                && let Some(end) = raw[start + 15..].find('<')
                && let Ok(parsed) = raw[start + 15..start + 15 + end].trim().parse::<i32>()
            {
                *fps.lock().unwrap() = parsed;
            }
            r#"<trt:SetVideoEncoderConfigurationResponse xmlns:trt="http://www.onvif.org/ver10/media/wsdl"/>"#
                .to_owned()
        } else if raw.contains("GetVideoEncoderConfigurations") {
            let current_fps = *fps.lock().unwrap();
            format!(
                r#"<trt:GetVideoEncoderConfigurationsResponse xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
                    <trt:Configurations token="enc-main">
                        <tt:Name>Main</tt:Name>
                        <tt:UseCount>1</tt:UseCount>
                        <tt:Encoding>H264</tt:Encoding>
                        <tt:Resolution><tt:Width>1920</tt:Width><tt:Height>1080</tt:Height></tt:Resolution>
                        <tt:Quality>4</tt:Quality>
                        <tt:RateControl>
                            <tt:FrameRateLimit>{current_fps}</tt:FrameRateLimit>
                            <tt:EncodingInterval>1</tt:EncodingInterval>
                            <tt:BitrateLimit>4096</tt:BitrateLimit>
                        </tt:RateControl>
                        <tt:H264><tt:GovLength>30</tt:GovLength><tt:H264Profile>Main</tt:H264Profile></tt:H264>
                        <tt:Multicast><tt:Address><tt:Type>IPv4</tt:Type></tt:Address><tt:Port>0</tt:Port><tt:TTL>1</tt:TTL><tt:AutoStart>false</tt:AutoStart></tt:Multicast>
                        <tt:SessionTimeout>PT60S</tt:SessionTimeout>
                    </trt:Configurations>
                </trt:GetVideoEncoderConfigurationsResponse>"#
            )
        } else {
            r#"<trt:GetOSDsResponse xmlns:trt="http://www.onvif.org/ver10/media/wsdl"/>"#.to_owned()
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:tt="http://www.onvif.org/ver10/schema"><s:Body>{inner}</s:Body></s:Envelope>"#
        )
    }

    #[test]
    fn mcp_device_onvif_read_range_validation_and_apply() {
        let (port, stop, handle) = spawn_fake_onvif();
        let mut cfg = base_camera_config(Ipv4Addr::LOCALHOST, "onvif-side");
        cfg.onvif_port = Some(port);
        let (state, router_tx, camera_id) = build_state(cfg, None);
        let admin = admin_principal();

        let read = get_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({}),
            future_deadline(),
        )
        .unwrap();
        assert_eq!(read["protocol"], "onvif");
        assert_eq!(
            read["settings"]["encoder"]["configurations"][0]["frame_rate_limit"],
            20
        );

        let oob = update_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({ "dry_run": true, "settings": { "encoder": { "frame_rate_limit": 90 } } }),
            McpDeviceWritePolicy {
                allow_camera_device_writes: true,
            },
            false,
            future_deadline(),
        )
        .unwrap_err();
        assert_eq!(oob._http_status, 400);

        let applied = update_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({ "dry_run": false, "settings": { "encoder": { "frame_rate_limit": 25 } } }),
            McpDeviceWritePolicy {
                allow_camera_device_writes: true,
            },
            false,
            future_deadline(),
        )
        .unwrap();
        assert_eq!(applied["applied"], true);
        assert_eq!(applied["restart_or_reconnect_expected"], true);
        assert_eq!(
            applied["verified_settings"]["encoder"]["configurations"][0]["frame_rate_limit"],
            25
        );

        stop.store(true, Ordering::Release);
        let _ = handle.join();
    }

    #[test]
    fn mcp_device_enforces_admin_exclusions_and_deadline() {
        let cfg = base_camera_config(Ipv4Addr::new(192, 0, 2, 55), "rtsp-only");
        let (state, router_tx, camera_id) = build_state(cfg, None);
        let admin = admin_principal();
        let viewer = user_principal(&state);

        assert_eq!(
            get_camera_device_settings(
                &state,
                &router_tx,
                &viewer,
                &camera_id,
                &json!({}),
                future_deadline()
            )
            .unwrap_err()
            ._http_status,
            403
        );
        assert_eq!(
            update_camera_device_settings(
                &state,
                &router_tx,
                &viewer,
                &camera_id,
                &json!({ "settings": { "osd": { "channel_name": "x" } } }),
                McpDeviceWritePolicy {
                    allow_camera_device_writes: true
                },
                false,
                future_deadline(),
            )
            .unwrap_err()
            ._http_status,
            403
        );
        for forbidden in [
            "factory_reset",
            "firmware",
            "password",
            "ip_address",
            "network",
        ] {
            let error = update_camera_device_settings(
                &state,
                &router_tx,
                &admin,
                &camera_id,
                &json!({ "dry_run": true, "settings": { forbidden: true } }),
                McpDeviceWritePolicy {
                    allow_camera_device_writes: true,
                },
                false,
                future_deadline(),
            )
            .unwrap_err();
            assert_eq!(error._http_status, 403);
        }
        let expired = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        assert_eq!(
            get_camera_device_settings(&state, &router_tx, &admin, &camera_id, &json!({}), expired)
                .unwrap_err()
                ._http_status,
            504
        );
    }

    #[test]
    fn mcp_device_rtsp_only_reports_unsupported_sections_honestly() {
        let cfg = base_camera_config(Ipv4Addr::new(192, 0, 2, 55), "rtsp-only");
        let (state, router_tx, camera_id) = build_state(cfg, None);
        let admin = admin_principal();

        let read = get_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({}),
            future_deadline(),
        )
        .unwrap();
        assert_eq!(read["protocol"], "rtsp_only");
        assert_eq!(read["supported_sections"].as_array().unwrap().len(), 0);
        assert_eq!(read["unsupported_sections"].as_array().unwrap().len(), 7);
        let dry = update_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({ "dry_run": true, "settings": { "motion": { "enabled": false } } }),
            McpDeviceWritePolicy {
                allow_camera_device_writes: false,
            },
            false,
            future_deadline(),
        )
        .unwrap();
        assert_eq!(dry["validated"], false);
        assert_eq!(dry["unsupported"], json!(["motion"]));
        let live = update_camera_device_settings(
            &state,
            &router_tx,
            &admin,
            &camera_id,
            &json!({ "dry_run": false, "settings": { "motion": { "enabled": false } } }),
            McpDeviceWritePolicy {
                allow_camera_device_writes: true,
            },
            false,
            future_deadline(),
        )
        .unwrap_err();
        assert_eq!(live._http_status, 501);
    }
}
