//! MCP handlers for camera catalog lookup and multi-candidate stream probing.
//!
//! All handlers require Administrator access, reject raw plaintext credentials
//! in favor of `{secret:KEY}` references, enforce SSRF and timeout bounds, stop
//! immediately on first match or camera lockout, and redact all diagnostics.

use super::{
    ApiPrincipal, ControlCommandError, MAX_CAMERA_CATALOG_QUERY_CHARS, ServerState,
    normalize_rtsp_url,
};
use crate::{
    access::AccessRole,
    api::proto,
    camera_database::{CatalogCamera, StreamHints},
    cameras::{CameraBackend, CameraConfig, CameraTransport, MediaProfile, probe_onvif_camera},
    config::{
        self, Secrets,
        secrets_store::{
            load_secrets_file_with_lock, resolve_loaded_reference, validate_secret_key,
        },
    },
    rtsp::{RtspTransport, probe_rtsp_video},
    runtime::{FacadeSender, RouterMessage},
};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::{Duration, Instant},
};
use url::{Host, Url};
use zeroize::Zeroizing;

const CATALOG_LIMIT_MIN: usize = 1;
const CATALOG_LIMIT_DEFAULT: usize = 10;
const CATALOG_LIMIT_MAX: usize = 25;
const CREDENTIAL_CANDIDATES_MAX: usize = 5;
const TIMEOUT_SECONDS_MIN: u64 = 1;
const TIMEOUT_SECONDS_DEFAULT: u64 = 8;
const TIMEOUT_SECONDS_MAX: u64 = 15;
const DIAGNOSTIC_TEXT_MAX_BYTES: usize = 256;

/// Searches the embedded camera catalog and attaches optional RTSP stream hints.
pub(super) fn search_camera_catalog(
    state: &ServerState,
    _router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, ControlCommandError> {
    require_administrator(principal)?;
    let request: SearchCatalogRequest = parse_arguments(arguments)?;
    let query = request.query.trim();
    if query.is_empty() || query.chars().count() > MAX_CAMERA_CATALOG_QUERY_CHARS {
        return Err(invalid_request(
            "camera catalog query must be between 1 and 128 characters",
        ));
    }
    let limit = request.limit.unwrap_or(CATALOG_LIMIT_DEFAULT);
    if !(CATALOG_LIMIT_MIN..=CATALOG_LIMIT_MAX).contains(&limit) {
        return Err(invalid_request(
            "camera catalog limit must be between 1 and 25",
        ));
    }
    let ip = request
        .ip
        .as_deref()
        .map(|raw| {
            raw.parse::<IpAddr>()
                .map_err(|_| invalid_request("camera catalog ip must be a valid IP address"))
        })
        .transpose()?;
    let database = state.camera_database.as_deref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "camera catalog is unavailable",
        )
    })?;
    let cameras: Vec<CatalogCameraEntry> = database
        .search(query, limit)
        .into_iter()
        .map(|camera| {
            let hints = ip.and_then(|address| database.stream_hints(&camera.id, address));
            CatalogCameraEntry::from_catalog(camera, hints)
        })
        .collect();
    let count = cameras.len();
    serialize_response(&SearchCatalogResponse {
        query: query.to_owned(),
        limit,
        count,
        cameras,
    })
}

/// Probes ONVIF and RTSP camera streams using bounded secret-reference candidates.
pub(super) fn probe_camera_streams(
    state: &ServerState,
    _router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    arguments: &serde_json::Value,
    deadline: Option<Instant>,
) -> Result<serde_json::Value, ControlCommandError> {
    probe_camera_streams_with(state, principal, arguments, deadline, run_live_probe)
}

fn probe_camera_streams_with<F>(
    state: &ServerState,
    principal: &ApiPrincipal,
    arguments: &serde_json::Value,
    caller_deadline: Option<Instant>,
    mut probe_candidate: F,
) -> Result<serde_json::Value, ControlCommandError>
where
    F: FnMut(&ProbePlan, &ResolvedCandidate, Instant) -> CandidateProbeOutcome,
{
    require_administrator(principal)?;
    reject_raw_credential_fields(arguments)?;
    let request: ProbeStreamsRequest = parse_arguments(arguments)?;
    let plan = build_probe_plan(state, request)?;
    let (secrets, resolved) = resolve_candidates(state, &plan.candidates)?;
    let redaction_terms = collect_redaction_terms(&secrets, &resolved);
    let effective_deadline = compute_deadline(plan.timeout_seconds, caller_deadline);

    let outcome = execute_candidates(
        &plan,
        &resolved,
        effective_deadline,
        &redaction_terms,
        &mut probe_candidate,
    );
    serialize_response(&outcome)
}

fn execute_candidates<F>(
    plan: &ProbePlan,
    candidates: &[ResolvedCandidate],
    deadline: Instant,
    redaction_terms: &[Zeroizing<String>],
    probe_candidate: &mut F,
) -> ProbeStreamsResponse
where
    F: FnMut(&ProbePlan, &ResolvedCandidate, Instant) -> CandidateProbeOutcome,
{
    let mut state = CandidateLoopState::default();
    for (index, candidate) in candidates.iter().enumerate() {
        if Instant::now() >= deadline {
            state
                .diagnostics
                .push("probe deadline reached before testing remaining candidates".to_owned());
            return state.finish(plan, None, "deadline_exceeded", redaction_terms);
        }
        state.attempted_candidates += 1;
        match probe_candidate(plan, candidate, deadline) {
            CandidateProbeOutcome::Matched(detail) => {
                state.last_detail = detail;
                let matched = MatchedCredentials {
                    candidate_index: index,
                    username_secret_reference: candidate.refs.username_secret_reference.clone(),
                    password_secret_reference: candidate.refs.password_secret_reference.clone(),
                    uid_secret_reference: candidate.refs.uid_secret_reference.clone(),
                };
                return state.finish(plan, Some(matched), "matched", redaction_terms);
            }
            CandidateProbeOutcome::AuthFailed { detail, message } => {
                state.record_failure(index, detail, message);
            }
            CandidateProbeOutcome::LockoutOrRateLimited { detail, message } => {
                state.record_failure(index, detail, message);
                return state.finish(plan, None, "lockout_or_rate_limited", redaction_terms);
            }
            CandidateProbeOutcome::Unreachable { detail, message } => {
                state.record_failure(index, detail, message);
                return state.finish(plan, None, "unreachable", redaction_terms);
            }
        }
    }
    state.finish(plan, None, "all_candidates_failed", redaction_terms)
}

#[derive(Default)]
struct CandidateLoopState {
    failed_candidates: Vec<usize>,
    diagnostics: Vec<String>,
    attempted_candidates: usize,
    last_detail: ProbeDetail,
}

impl CandidateLoopState {
    fn record_failure(&mut self, index: usize, detail: ProbeDetail, message: String) {
        self.failed_candidates.push(index);
        self.diagnostics
            .push(format!("candidate {index}: {message}"));
        self.last_detail = detail;
    }

    fn finish(
        self,
        plan: &ProbePlan,
        matched_credentials: Option<MatchedCredentials>,
        stop_reason: &'static str,
        redaction_terms: &[Zeroizing<String>],
    ) -> ProbeStreamsResponse {
        build_response(
            plan,
            matched_credentials,
            self.failed_candidates,
            self.attempted_candidates,
            stop_reason,
            self.last_detail,
            self.diagnostics,
            redaction_terms,
        )
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "response builder collects distinct validated probe outputs in one place"
)]
fn build_response(
    plan: &ProbePlan,
    matched_credentials: Option<MatchedCredentials>,
    failed_candidates: Vec<usize>,
    attempted_candidates: usize,
    stop_reason: &'static str,
    mut detail: ProbeDetail,
    diagnostics: Vec<String>,
    redaction_terms: &[Zeroizing<String>],
) -> ProbeStreamsResponse {
    redact_detail(&mut detail, redaction_terms);
    let diagnostics = diagnostics
        .into_iter()
        .map(|entry| redact_text(&entry, redaction_terms))
        .collect();
    let has_uid = matched_credentials
        .as_ref()
        .and_then(|matched| matched.uid_secret_reference.as_ref())
        .is_some();
    let recommended_backend = recommend_backend(detail.device.as_ref(), has_uid);
    let verified = matched_credentials.is_some();
    ProbeStreamsResponse {
        ip: plan.ip.to_string(),
        verified,
        stop_reason,
        attempted_candidates,
        matched_credentials,
        failed_candidates,
        recommended_backend,
        main_rtsp_url: detail.main_rtsp_url,
        sub_rtsp_url: detail.sub_rtsp_url,
        device: detail.device,
        profiles: detail.profiles,
        streams: detail.streams,
        onvif_error: detail.onvif_error,
        diagnostics,
    }
}

fn build_probe_plan(
    state: &ServerState,
    request: ProbeStreamsRequest,
) -> Result<ProbePlan, ControlCommandError> {
    let ip = request
        .ip
        .trim()
        .parse::<IpAddr>()
        .map_err(|_| invalid_request("camera stream probe requires a valid IP address"))?;
    let ip = normalize_ip(ip);
    validate_probe_target_ip(state, ip)?;
    if request.onvif_port == Some(0) {
        return Err(invalid_request("ONVIF port must be between 1 and 65535"));
    }
    let timeout_seconds = request.timeout_seconds.unwrap_or(TIMEOUT_SECONDS_DEFAULT);
    if !(TIMEOUT_SECONDS_MIN..=TIMEOUT_SECONDS_MAX).contains(&timeout_seconds) {
        return Err(invalid_request(
            "timeout_seconds must be between 1 and 15 seconds",
        ));
    }
    let transport = parse_transport(request.transport.as_deref())?;
    let main_rtsp_url = validate_rtsp_url_for_ip("main_rtsp_url", request.main_rtsp_url, ip)?;
    let sub_rtsp_url = validate_rtsp_url_for_ip("sub_rtsp_url", request.sub_rtsp_url, ip)?;
    let candidates = extract_credential_candidates(
        request.username_secret_reference,
        request.password_secret_reference,
        request.uid_secret_reference,
        request.credential_candidates,
    )?;
    Ok(ProbePlan {
        ip,
        onvif_port: request.onvif_port,
        main_rtsp_url,
        sub_rtsp_url,
        transport,
        query_onvif: request.query_onvif.unwrap_or(true),
        timeout_seconds,
        candidates,
    })
}

fn extract_credential_candidates(
    single_user: Option<String>,
    single_pass: Option<String>,
    single_uid: Option<String>,
    multi: Option<Vec<CredentialCandidateInput>>,
) -> Result<Vec<CredentialCandidateRef>, ControlCommandError> {
    let has_single = single_user.is_some() || single_pass.is_some() || single_uid.is_some();
    if has_single && multi.is_some() {
        return Err(invalid_request(
            "specify either single credential references or credential_candidates, not both",
        ));
    }
    let raw_candidates = if let Some(list) = multi {
        if list.is_empty() {
            return Err(invalid_request(
                "credential_candidates must contain between 1 and 5 entries",
            ));
        }
        if list.len() > CREDENTIAL_CANDIDATES_MAX {
            return Err(invalid_request(
                "credential_candidates may contain at most 5 entries",
            ));
        }
        list
    } else {
        let Some(username_secret_reference) = single_user else {
            return Err(invalid_request(
                "probe_camera_streams requires username_secret_reference and password_secret_reference or credential_candidates",
            ));
        };
        let Some(password_secret_reference) = single_pass else {
            return Err(invalid_request(
                "probe_camera_streams requires username_secret_reference and password_secret_reference or credential_candidates",
            ));
        };
        vec![CredentialCandidateInput {
            username_secret_reference,
            password_secret_reference,
            uid_secret_reference: single_uid,
        }]
    };
    raw_candidates
        .into_iter()
        .map(validate_candidate_refs)
        .collect()
}

fn validate_candidate_refs(
    input: CredentialCandidateInput,
) -> Result<CredentialCandidateRef, ControlCommandError> {
    let username = validate_secret_ref(
        "username_secret_reference",
        &input.username_secret_reference,
    )?;
    let password = validate_secret_ref(
        "password_secret_reference",
        &input.password_secret_reference,
    )?;
    let uid = input
        .uid_secret_reference
        .as_deref()
        .map(|value| validate_secret_ref("uid_secret_reference", value))
        .transpose()?;
    Ok(CredentialCandidateRef {
        username_secret_reference: username,
        password_secret_reference: password,
        uid_secret_reference: uid,
    })
}

fn validate_secret_ref(field: &str, value: &str) -> Result<String, ControlCommandError> {
    let trimmed = value.trim();
    if !config::is_secret_reference(trimmed) {
        return Err(invalid_request(format!(
            "{field} must use {{secret:KEY}} or {{secret:KEY|url}} syntax; raw credentials are rejected"
        )));
    }
    let body = trimmed
        .strip_prefix("{secret:")
        .and_then(|rest| rest.strip_suffix('}'))
        .expect("validated secret reference must retain braces");
    let key = body.split_once('|').map_or(body, |(key, _)| key);
    validate_secret_key(key)
        .map_err(|_| invalid_request(format!("{field} contains an invalid secret key name")))?;
    Ok(trimmed.to_owned())
}

fn reject_raw_credential_fields(arguments: &serde_json::Value) -> Result<(), ControlCommandError> {
    let Some(object) = arguments.as_object() else {
        return Err(invalid_request("tool arguments must be a JSON object"));
    };
    for forbidden in ["username", "password", "uid"] {
        if object.contains_key(forbidden) {
            return Err(invalid_request(format!(
                "raw credential field '{forbidden}' is not permitted; use {{secret:KEY}} references"
            )));
        }
    }
    if let Some(candidates) = object
        .get("credential_candidates")
        .and_then(serde_json::Value::as_array)
    {
        for candidate in candidates {
            if let Some(entry) = candidate.as_object() {
                for forbidden in ["username", "password", "uid"] {
                    if entry.contains_key(forbidden) {
                        return Err(invalid_request(format!(
                            "raw credential field '{forbidden}' is not permitted in credential_candidates; use {{secret:KEY}} references"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_probe_target_ip(state: &ServerState, ip: IpAddr) -> Result<(), ControlCommandError> {
    if is_disallowed_target_ip(ip) {
        return Err(invalid_request(
            "camera stream probe target IP is outside permitted private or configured networks",
        ));
    }
    let configured = state.camera_entries().into_iter().any(|entry| {
        entry
            .info
            .ip
            .parse::<IpAddr>()
            .is_ok_and(|configured_ip| normalize_ip(configured_ip) == ip)
    });
    if configured {
        return Ok(());
    }
    if ip.is_loopback() {
        return Err(invalid_request(
            "loopback addresses require an existing configured camera entry",
        ));
    }
    let in_private = is_private_lan_ip(ip);
    let in_local_policy = state.network_access.classify(ip, std::iter::empty()).local;
    if in_private || in_local_policy {
        return Ok(());
    }
    Err(invalid_request(
        "camera stream probe target IP must belong to a private or configured camera network",
    ))
}

const fn is_disallowed_target_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_link_local()
                || is_ipv4_documentation(v4)
        }
        IpAddr::V6(v6) => {
            v6.is_unspecified() || v6.is_multicast() || is_ipv6_unicast_link_local(v6)
        }
    }
}

const fn is_private_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

const fn is_ipv4_documentation(v4: Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
}

const fn is_ipv6_unicast_link_local(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xffc0) == 0xfe80
}

const fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        IpAddr::V4(v4) => IpAddr::V4(v4),
    }
}

fn parse_url_host_ip(url: &Url) -> Option<IpAddr> {
    match url.host()? {
        Host::Ipv4(v4) => Some(IpAddr::V4(v4)),
        Host::Ipv6(v6) => Some(normalize_ip(IpAddr::V6(v6))),
        Host::Domain(domain) => {
            let trimmed = domain
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .unwrap_or(domain);
            trimmed.parse::<IpAddr>().ok().map(normalize_ip)
        }
    }
}

fn validate_rtsp_url_for_ip(
    field: &str,
    raw: Option<String>,
    target_ip: IpAddr,
) -> Result<Option<String>, ControlCommandError> {
    let normalized =
        normalize_rtsp_url(raw).map_err(|error| invalid_request(format!("{field} {error}")))?;
    let Some(url_text) = normalized else {
        return Ok(None);
    };
    let parsed = Url::parse(&url_text)
        .map_err(|_| invalid_request(format!("{field} must be a valid RTSP URL")))?;
    if has_credential_query_param(&parsed) {
        return Err(invalid_request(format!(
            "{field} must not contain credentials in the query string"
        )));
    }
    if parse_url_host_ip(&parsed) != Some(target_ip) {
        return Err(invalid_request(format!(
            "{field} host must match the probe target IP address"
        )));
    }
    Ok(Some(url_text))
}

fn has_credential_query_param(url: &Url) -> bool {
    url.query_pairs().any(|(name, _)| {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "user" | "username" | "pass" | "password" | "pwd" | "token" | "secret" | "auth"
        )
    })
}

fn resolve_candidates(
    state: &ServerState,
    candidates: &[CredentialCandidateRef],
) -> Result<(Secrets, Vec<ResolvedCandidate>), ControlCommandError> {
    let config_path = state.camera_config_path.as_deref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "secret references require configuration persistence",
        )
    })?;
    let secrets_path = config::secrets_path(config_path);
    let secrets =
        load_secrets_file_with_lock(&secrets_path, &state.config_update).map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                "unable to load secrets.toml for stream probe",
            )
        })?;
    let mut resolved = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let username = resolve_loaded_reference(&candidate.username_secret_reference, &secrets)
            .map_err(|error| {
                invalid_request(format!("username_secret_reference failed: {error}"))
            })?;
        let password = resolve_loaded_reference(&candidate.password_secret_reference, &secrets)
            .map_err(|error| {
                invalid_request(format!("password_secret_reference failed: {error}"))
            })?;
        let uid = candidate
            .uid_secret_reference
            .as_deref()
            .map(|reference| {
                resolve_loaded_reference(reference, &secrets).map_err(|error| {
                    invalid_request(format!("uid_secret_reference failed: {error}"))
                })
            })
            .transpose()?;
        resolved.push(ResolvedCandidate {
            refs: candidate.clone(),
            username,
            password,
            uid,
        });
    }
    Ok((secrets, resolved))
}

fn run_live_probe(
    plan: &ProbePlan,
    candidate: &ResolvedCandidate,
    deadline: Instant,
) -> CandidateProbeOutcome {
    let camera_config = build_probe_camera_config(plan, candidate);
    let detail = if plan.query_onvif {
        match probe_onvif_stage(plan, &camera_config) {
            Ok(detail) => detail,
            Err(early_stop) => return *early_stop,
        }
    } else {
        ProbeDetail {
            main_rtsp_url: plan.main_rtsp_url.clone(),
            sub_rtsp_url: plan.sub_rtsp_url.clone(),
            ..Default::default()
        }
    };
    verify_candidate_streams(plan, candidate, deadline, detail)
}

fn probe_onvif_stage(
    plan: &ProbePlan,
    camera_config: &CameraConfig,
) -> Result<ProbeDetail, Box<CandidateProbeOutcome>> {
    let mut detail = ProbeDetail::default();
    match probe_onvif_camera(camera_config) {
        Ok(onvif) => {
            detail.device = Some(DeviceSummary {
                onvif_port: Some(onvif.onvif_port),
                manufacturer: onvif.device.manufacturer,
                model: onvif.device.model,
                firmware_version: onvif.device.firmware_version,
                serial_number: onvif.device.serial_number,
                hardware_id: onvif.device.hardware_id,
            });
            detail.profiles = onvif
                .profiles
                .iter()
                .enumerate()
                .map(|(idx, profile)| profile_to_summary(idx, profile))
                .collect();
            detail.main_rtsp_url = plan
                .main_rtsp_url
                .clone()
                .or_else(|| sanitize_onvif_rtsp_url(onvif.main_rtsp_url, plan.ip));
            detail.sub_rtsp_url = plan
                .sub_rtsp_url
                .clone()
                .or_else(|| sanitize_onvif_rtsp_url(onvif.sub_rtsp_url, plan.ip));
            Ok(detail)
        }
        Err(error) => {
            let kind = classify_probe_error(&error.to_string());
            if matches!(kind, FailureKind::LockoutOrRateLimited) {
                return Err(Box::new(CandidateProbeOutcome::LockoutOrRateLimited {
                    detail,
                    message: "camera reported lockout or rate limiting during ONVIF probe"
                        .to_owned(),
                }));
            }
            if plan.main_rtsp_url.is_none() && plan.sub_rtsp_url.is_none() {
                return Err(Box::new(match kind {
                    FailureKind::AuthFailed => CandidateProbeOutcome::AuthFailed {
                        detail,
                        message: "ONVIF authentication failed".to_owned(),
                    },
                    FailureKind::Unreachable | FailureKind::LockoutOrRateLimited => {
                        CandidateProbeOutcome::Unreachable {
                            detail,
                            message: "ONVIF service was unreachable".to_owned(),
                        }
                    }
                }));
            }
            detail.onvif_error = Some(
                "ONVIF did not respond; supplied RTSP endpoints were verified directly.".to_owned(),
            );
            detail.main_rtsp_url.clone_from(&plan.main_rtsp_url);
            detail.sub_rtsp_url.clone_from(&plan.sub_rtsp_url);
            Ok(detail)
        }
    }
}

fn verify_candidate_streams(
    plan: &ProbePlan,
    candidate: &ResolvedCandidate,
    deadline: Instant,
    mut detail: ProbeDetail,
) -> CandidateProbeOutcome {
    let stream_targets = [
        ("main", detail.main_rtsp_url.clone()),
        ("sub", detail.sub_rtsp_url.clone()),
    ];
    let mut any_verified = false;
    let mut saw_lockout = false;
    let mut saw_auth_failure = false;

    for (name, url) in stream_targets {
        let profile_audio = detail
            .profiles
            .iter()
            .find(|profile| profile.stream == name)
            .and_then(|profile| profile.audio.clone());
        let (summary, failure) = verify_single_stream(
            name,
            url,
            profile_audio,
            plan.transport,
            candidate,
            deadline,
        );
        if summary.verified {
            any_verified = true;
        }
        detail.streams.push(summary);
        match failure {
            Some(FailureKind::LockoutOrRateLimited) => {
                saw_lockout = true;
                break;
            }
            Some(FailureKind::AuthFailed) => saw_auth_failure = true,
            Some(FailureKind::Unreachable) | None => {}
        }
    }

    if saw_lockout {
        return CandidateProbeOutcome::LockoutOrRateLimited {
            detail,
            message: "camera reported lockout or rate limiting during RTSP probe".to_owned(),
        };
    }
    if any_verified || (detail.device.is_some() && !saw_auth_failure) {
        return CandidateProbeOutcome::Matched(detail);
    }
    if saw_auth_failure {
        return CandidateProbeOutcome::AuthFailed {
            detail,
            message: "RTSP authentication was rejected".to_owned(),
        };
    }
    CandidateProbeOutcome::Unreachable {
        detail,
        message: "camera RTSP endpoints were unreachable or timed out".to_owned(),
    }
}

fn verify_single_stream(
    name: &str,
    url: Option<String>,
    audio: Option<ProbeAudioSummary>,
    transport: RtspTransport,
    candidate: &ResolvedCandidate,
    deadline: Instant,
) -> (StreamVerificationSummary, Option<FailureKind>) {
    let Some(rtsp_url) = url else {
        return (StreamVerificationSummary::missing(name), None);
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return (
            StreamVerificationSummary::failed(name, Some(rtsp_url), "probe deadline reached"),
            Some(FailureKind::Unreachable),
        );
    }
    match probe_rtsp_video(
        &rtsp_url,
        &candidate.username,
        &candidate.password,
        transport,
        remaining,
    ) {
        Ok(evidence) => (
            StreamVerificationSummary {
                stream: name.to_owned(),
                verified: true,
                rtsp_url: Some(rtsp_url),
                codec: Some(evidence.codec),
                resolution: Some(format!("{}x{}", evidence.width, evidence.height)),
                declared_fps: evidence.declared_fps,
                frames_received: evidence.frames_received,
                keyframe_received: evidence.keyframe_received,
                elapsed_ms: u64::try_from(evidence.elapsed.as_millis()).unwrap_or(u64::MAX),
                audio,
                error: None,
            },
            None,
        ),
        Err(error) => {
            let message = error.to_string();
            let kind = classify_probe_error(&message);
            (
                StreamVerificationSummary::failed(
                    name,
                    Some(rtsp_url),
                    rtsp_error_summary(kind, &message),
                ),
                Some(kind),
            )
        }
    }
}

fn build_probe_camera_config(plan: &ProbePlan, candidate: &ResolvedCandidate) -> CameraConfig {
    let transport = match plan.transport {
        RtspTransport::Tcp => CameraTransport::Tcp,
        RtspTransport::Udp => CameraTransport::Udp,
    };
    CameraConfig {
        events: Default::default(),
        ip: plan.ip,
        name: None,
        display_name: None,
        manufacturer: None,
        username: (*candidate.username).clone(),
        password: (*candidate.password).clone(),
        onvif_port: plan.onvif_port,
        http_port: None,
        main_rtsp_url: None,
        sub_rtsp_url: None,
        uid: candidate.uid.as_ref().map(|uid| (**uid).clone()),
        backend: CameraBackend::Retina,
        transport,
        record_generic_motion_events: false,
        recording_mode: Default::default(),
        event_recording_duration_secs: 60,
        event_pre_recording_duration_secs: 0,
        event_recording_stream: Default::default(),
        recorded_audio: Default::default(),
        live_audio: Default::default(),
    }
}

fn sanitize_onvif_rtsp_url(raw: Option<String>, target_ip: IpAddr) -> Option<String> {
    let raw = raw?;
    let mut url = Url::parse(&raw).ok()?;
    if !matches!(url.scheme(), "rtsp" | "rtsps") {
        return None;
    }
    if parse_url_host_ip(&url) != Some(target_ip) {
        return None;
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    Some(url.into())
}

fn classify_probe_error(message: &str) -> FailureKind {
    let lower = message.to_ascii_lowercase();
    if [
        "429",
        "too many requests",
        "rate limit",
        "rate-limit",
        "locked",
        "lockout",
        "too many failed",
        "too many attempts",
        "account lock",
        "retry-after",
        "temporarily blocked",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return FailureKind::LockoutOrRateLimited;
    }
    if [
        "401",
        "unauthorized",
        "notauthorized",
        "not authorized",
        "authentication",
        "failedauthentication",
        "invalidsecurity",
        "invalid credentials",
        "wrong password",
        "bad credentials",
        "login failed",
        "403",
        "forbidden",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return FailureKind::AuthFailed;
    }
    FailureKind::Unreachable
}

fn rtsp_error_summary(kind: FailureKind, raw: &str) -> &'static str {
    let lower = raw.to_ascii_lowercase();
    match kind {
        FailureKind::LockoutOrRateLimited => "Camera reported lockout or rate limiting.",
        FailureKind::AuthFailed => "RTSP authentication was rejected.",
        FailureKind::Unreachable => {
            if lower.contains("keyframe") || lower.contains("no media progress") {
                "No video keyframe arrived before the verification deadline."
            } else if lower.contains("did not describe a video stream") {
                "The RTSP endpoint did not describe a video stream."
            } else {
                "RTSP connection or media verification failed."
            }
        }
    }
}

fn recommend_backend(device: Option<&DeviceSummary>, has_uid: bool) -> &'static str {
    if has_uid {
        return "reo-proto";
    }
    if let Some(device) = device {
        let is_reolink = device
            .manufacturer
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("reolink"))
            || device
                .model
                .as_deref()
                .is_some_and(|value| value.to_ascii_lowercase().contains("reolink"));
        if is_reolink {
            return "reo-proto";
        }
    }
    "retina"
}

fn collect_redaction_terms(
    secrets: &Secrets,
    candidates: &[ResolvedCandidate],
) -> Vec<Zeroizing<String>> {
    let mut terms: Vec<Zeroizing<String>> = secrets
        .redaction_values()
        .into_iter()
        .map(Zeroizing::new)
        .collect();
    for candidate in candidates {
        terms.push(candidate.username.clone());
        terms.push(candidate.password.clone());
        if let Some(uid) = &candidate.uid {
            terms.push(uid.clone());
        }
    }
    let encoded: Vec<Zeroizing<String>> = terms
        .iter()
        .filter(|value| !value.is_empty())
        .map(|value| Zeroizing::new(percent_encode_component(value)))
        .collect();
    terms.extend(encoded);
    terms.retain(|value| !value.is_empty());
    terms.sort_by_key(|term| std::cmp::Reverse(term.len()));
    terms.dedup();
    terms
}

fn percent_encode_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn redact_detail(detail: &mut ProbeDetail, terms: &[Zeroizing<String>]) {
    if let Some(error) = &mut detail.onvif_error {
        *error = redact_text(error, terms);
    }
    for stream in &mut detail.streams {
        if let Some(error) = &mut stream.error {
            *error = redact_text(error, terms);
        }
    }
    if let Some(device) = &mut detail.device {
        for text in [
            &mut device.manufacturer,
            &mut device.model,
            &mut device.firmware_version,
            &mut device.serial_number,
            &mut device.hardware_id,
        ]
        .into_iter()
        .flatten()
        {
            *text = redact_text(text, terms);
        }
    }
}

fn redact_text(input: &str, terms: &[Zeroizing<String>]) -> String {
    let mut output = input.to_owned();
    for term in terms {
        if !term.is_empty() && output.contains(term.as_str()) {
            output = output.replace(term.as_str(), "[REDACTED]");
        }
    }
    if output.len() > DIAGNOSTIC_TEXT_MAX_BYTES {
        let mut cutoff = DIAGNOSTIC_TEXT_MAX_BYTES;
        while !output.is_char_boundary(cutoff) {
            cutoff -= 1;
        }
        output.truncate(cutoff);
    }
    output
}

fn compute_deadline(timeout_seconds: u64, caller_deadline: Option<Instant>) -> Instant {
    let tool_deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    caller_deadline.map_or(tool_deadline, |deadline| deadline.min(tool_deadline))
}

fn parse_transport(raw: Option<&str>) -> Result<RtspTransport, ControlCommandError> {
    match raw.map(str::trim) {
        None | Some("") | Some("tcp") | Some("TCP") => Ok(RtspTransport::Tcp),
        Some("udp") | Some("UDP") => Ok(RtspTransport::Udp),
        Some(_) => Err(invalid_request("transport must be 'tcp' or 'udp'")),
    }
}

fn profile_to_summary(index: usize, profile: &MediaProfile) -> ProbeProfileSummary {
    ProbeProfileSummary {
        name: profile.name.clone(),
        stream: if index == 0 { "main" } else { "sub" }.to_owned(),
        encoding: profile
            .video
            .as_ref()
            .map(|video| video.encoding.to_string()),
        resolution: profile
            .video
            .as_ref()
            .map(|video| format!("{}x{}", video.width, video.height)),
        framerate: profile.video.as_ref().map(|video| video.framerate),
        bitrate_kbps: profile.video.as_ref().and_then(|video| video.bitrate_kbps),
        audio: profile.audio.as_ref().map(|audio| ProbeAudioSummary {
            encoding: audio.encoding.to_string(),
            sample_rate: audio.sample_rate,
            bitrate_kbps: audio.bitrate_kbps,
        }),
    }
}

fn require_administrator(principal: &ApiPrincipal) -> Result<(), ControlCommandError> {
    if principal.role != AccessRole::Administrator {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "administrator access is required",
        ));
    }
    Ok(())
}

fn parse_arguments<T: for<'de> Deserialize<'de>>(
    arguments: &serde_json::Value,
) -> Result<T, ControlCommandError> {
    serde_json::from_value(arguments.clone())
        .map_err(|error| invalid_request(format!("invalid tool arguments: {error}")))
}

fn serialize_response<T: Serialize>(value: &T) -> Result<serde_json::Value, ControlCommandError> {
    serde_json::to_value(value).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to serialize MCP response: {error}"),
        )
    })
}

fn invalid_request(message: impl Into<String>) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchCatalogRequest {
    query: String,
    #[serde(default)]
    ip: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct SearchCatalogResponse {
    query: String,
    limit: usize,
    count: usize,
    cameras: Vec<CatalogCameraEntry>,
}

#[derive(Debug, Serialize)]
struct CatalogCameraEntry {
    id: String,
    brand: String,
    model: String,
    aliases: Vec<String>,
    camera_type: String,
    resolution_label: Option<String>,
    megapixels: Option<f64>,
    two_way_audio: Option<bool>,
    protocols: Vec<String>,
    codecs: Vec<String>,
    streams: Vec<CatalogStreamEntry>,
    stream_hints: Option<CatalogStreamHints>,
}

impl CatalogCameraEntry {
    fn from_catalog(camera: CatalogCamera, hints: Option<StreamHints>) -> Self {
        Self {
            id: camera.id,
            brand: camera.brand,
            model: camera.model,
            aliases: camera.aliases.into_vec(),
            camera_type: camera.camera_type,
            resolution_label: camera.resolution_label,
            megapixels: camera.megapixels,
            two_way_audio: camera.two_way_audio,
            protocols: camera.protocols.into_vec(),
            codecs: camera.codecs.into_vec(),
            streams: camera
                .streams
                .into_vec()
                .into_iter()
                .map(|stream| CatalogStreamEntry {
                    name: stream.name,
                    resolution: stream.resolution,
                    fps: stream.fps,
                    codec: stream.codec,
                })
                .collect(),
            stream_hints: hints.map(|hints| CatalogStreamHints {
                main_rtsp_url: hints.main,
                sub_rtsp_url: hints.sub,
            }),
        }
    }
}

#[derive(Debug, Serialize)]
struct CatalogStreamEntry {
    name: String,
    resolution: Option<String>,
    fps: Option<u16>,
    codec: Option<String>,
}

#[derive(Debug, Serialize)]
struct CatalogStreamHints {
    main_rtsp_url: Option<String>,
    sub_rtsp_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeStreamsRequest {
    ip: String,
    #[serde(default)]
    onvif_port: Option<u16>,
    #[serde(default)]
    main_rtsp_url: Option<String>,
    #[serde(default)]
    sub_rtsp_url: Option<String>,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    query_onvif: Option<bool>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    username_secret_reference: Option<String>,
    #[serde(default)]
    password_secret_reference: Option<String>,
    #[serde(default)]
    uid_secret_reference: Option<String>,
    #[serde(default)]
    credential_candidates: Option<Vec<CredentialCandidateInput>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialCandidateInput {
    username_secret_reference: String,
    password_secret_reference: String,
    #[serde(default)]
    uid_secret_reference: Option<String>,
}

#[derive(Debug, Clone)]
struct CredentialCandidateRef {
    username_secret_reference: String,
    password_secret_reference: String,
    uid_secret_reference: Option<String>,
}

struct ProbePlan {
    ip: IpAddr,
    onvif_port: Option<u16>,
    main_rtsp_url: Option<String>,
    sub_rtsp_url: Option<String>,
    transport: RtspTransport,
    query_onvif: bool,
    timeout_seconds: u64,
    candidates: Vec<CredentialCandidateRef>,
}

struct ResolvedCandidate {
    refs: CredentialCandidateRef,
    username: Zeroizing<String>,
    password: Zeroizing<String>,
    uid: Option<Zeroizing<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    AuthFailed,
    LockoutOrRateLimited,
    Unreachable,
}

enum CandidateProbeOutcome {
    Matched(ProbeDetail),
    AuthFailed {
        detail: ProbeDetail,
        message: String,
    },
    LockoutOrRateLimited {
        detail: ProbeDetail,
        message: String,
    },
    Unreachable {
        detail: ProbeDetail,
        message: String,
    },
}

#[derive(Debug, Default)]
struct ProbeDetail {
    main_rtsp_url: Option<String>,
    sub_rtsp_url: Option<String>,
    device: Option<DeviceSummary>,
    profiles: Vec<ProbeProfileSummary>,
    streams: Vec<StreamVerificationSummary>,
    onvif_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProbeStreamsResponse {
    ip: String,
    verified: bool,
    stop_reason: &'static str,
    attempted_candidates: usize,
    matched_credentials: Option<MatchedCredentials>,
    failed_candidates: Vec<usize>,
    recommended_backend: &'static str,
    main_rtsp_url: Option<String>,
    sub_rtsp_url: Option<String>,
    device: Option<DeviceSummary>,
    profiles: Vec<ProbeProfileSummary>,
    streams: Vec<StreamVerificationSummary>,
    onvif_error: Option<String>,
    diagnostics: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MatchedCredentials {
    candidate_index: usize,
    username_secret_reference: String,
    password_secret_reference: String,
    uid_secret_reference: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeviceSummary {
    onvif_port: Option<u16>,
    manufacturer: Option<String>,
    model: Option<String>,
    firmware_version: Option<String>,
    serial_number: Option<String>,
    hardware_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProbeProfileSummary {
    name: String,
    stream: String,
    encoding: Option<String>,
    resolution: Option<String>,
    framerate: Option<f64>,
    bitrate_kbps: Option<u32>,
    audio: Option<ProbeAudioSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct ProbeAudioSummary {
    encoding: String,
    sample_rate: Option<u32>,
    bitrate_kbps: Option<u32>,
}

#[derive(Debug, Serialize)]
struct StreamVerificationSummary {
    stream: String,
    verified: bool,
    rtsp_url: Option<String>,
    codec: Option<String>,
    resolution: Option<String>,
    declared_fps: Option<f64>,
    frames_received: u32,
    keyframe_received: bool,
    elapsed_ms: u64,
    audio: Option<ProbeAudioSummary>,
    error: Option<String>,
}

impl StreamVerificationSummary {
    fn missing(stream: &str) -> Self {
        Self {
            stream: stream.to_owned(),
            verified: false,
            rtsp_url: None,
            codec: None,
            resolution: None,
            declared_fps: None,
            frames_received: 0,
            keyframe_received: false,
            elapsed_ms: 0,
            audio: None,
            error: Some("No RTSP endpoint is available for this stream.".to_owned()),
        }
    }

    fn failed(stream: &str, rtsp_url: Option<String>, message: &str) -> Self {
        Self {
            stream: stream.to_owned(),
            verified: false,
            rtsp_url,
            codec: None,
            resolution: None,
            declared_fps: None,
            frames_received: 0,
            keyframe_received: false,
            elapsed_ms: 0,
            audio: None,
            error: Some(message.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        access::AuthenticatedCredential,
        config::secrets_store::set_secret_entry,
        storage::{RecordingDemand, StorageConfig},
        test_support::TestCameraCatalog,
        webrtc::WebRtc,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::{
        collections::HashMap,
        io::{Read as _, Write as _},
        net::TcpListener,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn admin_principal() -> ApiPrincipal {
        ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn user_principal() -> ApiPrincipal {
        ApiPrincipal::credential(AuthenticatedCredential {
            id: uuid::Uuid::new_v4(),
            name: "Viewer".to_owned(),
            role: AccessRole::User,
            revision: 1,
            expires_at_ms: None,
        })
    }

    fn state_with_secrets(
        secrets: &[(&str, &str)],
        configured_ip: Option<IpAddr>,
    ) -> (TempDir, ServerState) {
        let dir = TempDir::new("keeppeek-mcp-probe");
        let config_path = dir.0.join("config.toml");
        config::write_private_file(&config_path, b"host = \"127.0.0.1\"\nport = 3000\n").unwrap();
        let secrets_path = config::secrets_path(&config_path);
        for (key, value) in secrets {
            set_secret_entry(&secrets_path, key, value).unwrap();
        }
        let mut camera_configs = HashMap::new();
        if let Some(ip) = configured_ip {
            camera_configs.insert(
                "cameras".to_owned(),
                vec![CameraConfig {
                    events: Default::default(),
                    ip,
                    name: Some("mock".to_owned()),
                    display_name: None,
                    manufacturer: None,
                    username: "admin".to_owned(),
                    password: "secret".to_owned(),
                    onvif_port: None,
                    http_port: None,
                    main_rtsp_url: None,
                    sub_rtsp_url: None,
                    uid: None,
                    backend: CameraBackend::Retina,
                    transport: CameraTransport::Tcp,
                    record_generic_motion_events: false,
                    recording_mode: Default::default(),
                    event_recording_duration_secs: 60,
                    event_pre_recording_duration_secs: 0,
                    event_recording_stream: Default::default(),
                    recorded_audio: Default::default(),
                    live_audio: Default::default(),
                }],
            );
        }
        let state = ServerState::new(
            &config::Config::default(),
            &camera_configs,
            &HashMap::new(),
            &StorageConfig::default(),
            RecordingDemand::new(Duration::from_secs(1)),
            WebRtc::new(),
        )
        .with_camera_config_path(config_path)
        .with_test_camera_catalog(TestCameraCatalog::standard());
        (dir, state)
    }

    #[test]
    fn search_camera_catalog_enforces_admin_limit_bounds_and_stream_hints() {
        let (_dir, state) = state_with_secrets(&[], None);
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        let denied = search_camera_catalog(
            &state,
            &router_tx,
            &user_principal(),
            &serde_json::json!({ "query": "Reolink" }),
        )
        .unwrap_err();
        assert_eq!(denied.code, proto::ErrorCode::Rejected);
        assert_eq!(denied._http_status, 403);

        for invalid_limit in [0, 26] {
            let err = search_camera_catalog(
                &state,
                &router_tx,
                &admin_principal(),
                &serde_json::json!({ "query": "Reolink", "limit": invalid_limit }),
            )
            .unwrap_err();
            assert_eq!(err.code, proto::ErrorCode::InvalidRequest);
        }

        let response = search_camera_catalog(
            &state,
            &router_tx,
            &admin_principal(),
            &serde_json::json!({
                "query": "RLC-Test",
                "ip": "192.168.10.42",
                "limit": 1,
            }),
        )
        .unwrap();
        assert_eq!(response["count"], 1);
        assert_eq!(response["limit"], 1);
        assert_eq!(response["cameras"][0]["brand"], "Reolink");
        assert_eq!(response["cameras"][0]["model"], "RLC-Test");
        assert_eq!(
            response["cameras"][0]["stream_hints"]["main_rtsp_url"],
            "rtsp://192.168.10.42/main"
        );
        assert_eq!(
            response["cameras"][0]["stream_hints"]["sub_rtsp_url"],
            "rtsp://192.168.10.42/sub"
        );
    }

    #[test]
    fn probe_camera_streams_rejects_raw_credentials_and_excess_candidates() {
        let (_dir, state) = state_with_secrets(&[("U1", "admin"), ("P1", "pass1")], None);
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let pair = serde_json::json!({
            "username_secret_reference": "{secret:U1}",
            "password_secret_reference": "{secret:P1}",
        });
        for bad_args in [
            serde_json::json!({ "ip": "192.168.1.50", "username": "admin", "password": "raw" }),
            serde_json::json!({
                "ip": "192.168.1.50",
                "username_secret_reference": "admin",
                "password_secret_reference": "{secret:P1}",
            }),
            serde_json::json!({
                "ip": "192.168.1.50",
                "credential_candidates": vec![pair; 6],
            }),
            serde_json::json!({
                "ip": "192.168.1.50",
                "credential_candidates": [],
            }),
        ] {
            let err = probe_camera_streams(&state, &router_tx, &admin_principal(), &bad_args, None)
                .unwrap_err();
            assert_eq!(err.code, proto::ErrorCode::InvalidRequest, "for {bad_args}");
        }
    }

    #[test]
    fn probe_camera_streams_enforces_ssrf_timeout_and_admin_role() {
        let (_dir, state) = state_with_secrets(&[("U1", "admin"), ("P1", "pass1")], None);
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        for bad_args in [
            serde_json::json!({
                "ip": "192.168.1.50",
                "timeout_seconds": 0,
                "username_secret_reference": "{secret:U1}",
                "password_secret_reference": "{secret:P1}",
            }),
            serde_json::json!({
                "ip": "192.168.1.50",
                "timeout_seconds": 16,
                "username_secret_reference": "{secret:U1}",
                "password_secret_reference": "{secret:P1}",
            }),
            serde_json::json!({
                "ip": "169.254.169.254",
                "username_secret_reference": "{secret:U1}",
                "password_secret_reference": "{secret:P1}",
            }),
            serde_json::json!({
                "ip": "8.8.8.8",
                "username_secret_reference": "{secret:U1}",
                "password_secret_reference": "{secret:P1}",
            }),
            serde_json::json!({
                "ip": "127.0.0.1",
                "username_secret_reference": "{secret:U1}",
                "password_secret_reference": "{secret:P1}",
            }),
            serde_json::json!({
                "ip": "192.168.1.50",
                "main_rtsp_url": "rtsp://192.168.1.99/main",
                "username_secret_reference": "{secret:U1}",
                "password_secret_reference": "{secret:P1}",
            }),
            serde_json::json!({
                "ip": "192.168.1.50",
                "main_rtsp_url": "rtsp://camera.example.com/main",
                "username_secret_reference": "{secret:U1}",
                "password_secret_reference": "{secret:P1}",
            }),
            serde_json::json!({
                "ip": "192.168.1.50",
                "main_rtsp_url": "rtsp://192.168.1.50/main?password=leaked",
                "username_secret_reference": "{secret:U1}",
                "password_secret_reference": "{secret:P1}",
            }),
        ] {
            let err = probe_camera_streams(&state, &router_tx, &admin_principal(), &bad_args, None)
                .unwrap_err();
            assert_eq!(err.code, proto::ErrorCode::InvalidRequest, "for {bad_args}");
        }

        let denied = probe_camera_streams(
            &state,
            &router_tx,
            &user_principal(),
            &serde_json::json!({
                "ip": "192.168.1.50",
                "username_secret_reference": "{secret:U1}",
                "password_secret_reference": "{secret:P1}",
            }),
            None,
        )
        .unwrap_err();
        assert_eq!(denied.code, proto::ErrorCode::Rejected);
        assert_eq!(denied._http_status, 403);
    }

    fn mock_reolink_match_detail(sentinel_pass: &str) -> ProbeDetail {
        ProbeDetail {
            main_rtsp_url: Some("rtsp://192.168.50.10/main".to_owned()),
            device: Some(DeviceSummary {
                onvif_port: Some(8000),
                manufacturer: Some(format!("Reolink {sentinel_pass}")),
                model: Some("RLC-810A".to_owned()),
                firmware_version: None,
                serial_number: None,
                hardware_id: None,
            }),
            streams: vec![StreamVerificationSummary {
                stream: "main".to_owned(),
                verified: true,
                rtsp_url: Some("rtsp://192.168.50.10/main".to_owned()),
                codec: Some("h264".to_owned()),
                resolution: Some("3840x2160".to_owned()),
                declared_fps: Some(25.0),
                frames_received: 2,
                keyframe_received: true,
                elapsed_ms: 42,
                audio: None,
                error: None,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn multi_candidate_probe_stops_on_first_match_and_never_leaks_sentinels() {
        let sentinel_user = "SENTINEL_USER_DO_NOT_LEAK_111";
        let sentinel_pass = "SENTINEL_PASS_DO_NOT_LEAK_222!";
        let (_dir, state) = state_with_secrets(
            &[
                ("U_BAD", "wrong-user"),
                ("P_BAD", "wrong-pass"),
                ("U_GOOD", sentinel_user),
                ("P_GOOD", sentinel_pass),
                ("U_NEVER", "never-tried-user"),
                ("P_NEVER", "never-tried-pass"),
            ],
            None,
        );
        let args = serde_json::json!({
            "ip": "192.168.50.10",
            "timeout_seconds": 5,
            "credential_candidates": [
                { "username_secret_reference": "{secret:U_BAD}", "password_secret_reference": "{secret:P_BAD}" },
                { "username_secret_reference": "{secret:U_GOOD}", "password_secret_reference": "{secret:P_GOOD}" },
                { "username_secret_reference": "{secret:U_NEVER}", "password_secret_reference": "{secret:P_NEVER}" }
            ]
        });

        let mut seen_passwords = Vec::new();
        let response = probe_camera_streams_with(
            &state,
            &admin_principal(),
            &args,
            None,
            |_, candidate, _| {
                seen_passwords.push((*candidate.password).clone());
                if candidate.password.as_str() == sentinel_pass {
                    CandidateProbeOutcome::Matched(mock_reolink_match_detail(sentinel_pass))
                } else {
                    CandidateProbeOutcome::AuthFailed {
                        detail: ProbeDetail::default(),
                        message: format!("rejected password {}", candidate.password.as_str()),
                    }
                }
            },
        )
        .unwrap();

        assert_eq!(seen_passwords, vec!["wrong-pass", sentinel_pass]);
        assert_eq!(response["verified"], true);
        assert_eq!(response["stop_reason"], "matched");
        assert_eq!(response["attempted_candidates"], 2);
        assert_eq!(response["failed_candidates"], serde_json::json!([0]));
        assert_eq!(response["recommended_backend"], "reo-proto");
        assert_eq!(
            response["matched_credentials"],
            serde_json::json!({
                "candidate_index": 1,
                "username_secret_reference": "{secret:U_GOOD}",
                "password_secret_reference": "{secret:P_GOOD}",
                "uid_secret_reference": null,
            })
        );
        let serialized = serde_json::to_string(&response).unwrap();
        assert!(!serialized.contains(sentinel_user));
        assert!(!serialized.contains(sentinel_pass));
        assert!(!serialized.contains("wrong-pass"));
        assert!(!serialized.contains(&percent_encode_component(sentinel_pass)));
    }

    #[test]
    fn multi_candidate_probe_stops_immediately_on_lockout_and_unreachable() {
        let (_dir, state) = state_with_secrets(
            &[
                ("U1", "u1"),
                ("P1", "p1"),
                ("U2", "u2"),
                ("P2", "p2"),
                ("U3", "u3"),
                ("P3", "p3"),
            ],
            None,
        );
        let args = serde_json::json!({
            "ip": "192.168.50.11",
            "credential_candidates": [
                { "username_secret_reference": "{secret:U1}", "password_secret_reference": "{secret:P1}" },
                { "username_secret_reference": "{secret:U2}", "password_secret_reference": "{secret:P2}" },
                { "username_secret_reference": "{secret:U3}", "password_secret_reference": "{secret:P3}" }
            ]
        });

        let mut lockout_attempts = 0;
        let lockout_resp =
            probe_camera_streams_with(&state, &admin_principal(), &args, None, |_, _, _| {
                lockout_attempts += 1;
                CandidateProbeOutcome::LockoutOrRateLimited {
                    detail: ProbeDetail::default(),
                    message: "HTTP 429 Too Many Requests".to_owned(),
                }
            })
            .unwrap();
        assert_eq!(lockout_attempts, 1);
        assert_eq!(lockout_resp["verified"], false);
        assert_eq!(lockout_resp["stop_reason"], "lockout_or_rate_limited");
        assert_eq!(lockout_resp["attempted_candidates"], 1);
        assert_eq!(lockout_resp["failed_candidates"], serde_json::json!([0]));

        let mut unreachable_attempts = 0;
        let unreachable_resp =
            probe_camera_streams_with(&state, &admin_principal(), &args, None, |_, _, _| {
                unreachable_attempts += 1;
                CandidateProbeOutcome::Unreachable {
                    detail: ProbeDetail::default(),
                    message: "connection refused".to_owned(),
                }
            })
            .unwrap();
        assert_eq!(unreachable_attempts, 1);
        assert_eq!(unreachable_resp["verified"], false);
        assert_eq!(unreachable_resp["stop_reason"], "unreachable");
        assert_eq!(unreachable_resp["attempted_candidates"], 1);
    }

    #[test]
    fn mock_rtsp_server_verifies_multi_candidate_auth_and_live_reload() {
        let sentinel_pass = "MOCK_RTSP_SENTINEL_PASS_7744";
        let (dir, state) = state_with_secrets(
            &[("CAM_USER", "operator"), ("CAM_BAD", "wrong-pass")],
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        );
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();

        // Add the valid secret AFTER ServerState construction to prove live reload works.
        set_secret_entry(
            &config::secrets_path(&dir.0.join("config.toml")),
            "CAM_GOOD",
            sentinel_pass,
        )
        .unwrap();

        let (port, describe_count, handle) = spawn_mock_rtsp_server(MockRtspMode::RequireAuth {
            expected_basic: STANDARD.encode(format!("operator:{sentinel_pass}")),
        });
        let rtsp_url = format!("rtsp://127.0.0.1:{port}/main");
        let response = probe_camera_streams(
            &state,
            &router_tx,
            &admin_principal(),
            &serde_json::json!({
                "ip": "127.0.0.1",
                "query_onvif": false,
                "main_rtsp_url": rtsp_url,
                "timeout_seconds": 5,
                "credential_candidates": [
                    { "username_secret_reference": "{secret:CAM_USER}", "password_secret_reference": "{secret:CAM_BAD}" },
                    { "username_secret_reference": "{secret:CAM_USER}", "password_secret_reference": "{secret:CAM_GOOD}" },
                    { "username_secret_reference": "{secret:CAM_USER}", "password_secret_reference": "{secret:CAM_BAD}" }
                ]
            }),
            None,
        )
        .unwrap();
        handle.join().unwrap();

        assert_eq!(response["verified"], true);
        assert_eq!(response["stop_reason"], "matched");
        assert_eq!(response["attempted_candidates"], 2);
        assert_eq!(response["failed_candidates"], serde_json::json!([0]));
        assert_eq!(response["matched_credentials"]["candidate_index"], 1);
        assert_eq!(
            response["matched_credentials"]["password_secret_reference"],
            "{secret:CAM_GOOD}"
        );
        assert_eq!(response["streams"][0]["verified"], true);
        assert_eq!(response["streams"][0]["codec"], "h264");
        assert_eq!(response["streams"][0]["keyframe_received"], true);
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains(sentinel_pass)
        );
        assert!(describe_count.load(Ordering::Acquire) <= 4);
    }

    #[test]
    fn mock_rtsp_server_stops_immediately_on_429_lockout() {
        let (_dir, state) = state_with_secrets(
            &[
                ("CAM_USER", "operator"),
                ("CAM_BAD", "wrong-pass"),
                ("CAM_GOOD", "good-pass"),
            ],
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        );
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        let (lock_port, lock_describes, lock_handle) =
            spawn_mock_rtsp_server(MockRtspMode::Lockout429);
        let lock_url = format!("rtsp://127.0.0.1:{lock_port}/main");
        let lock_response = probe_camera_streams(
            &state,
            &router_tx,
            &admin_principal(),
            &serde_json::json!({
                "ip": "127.0.0.1",
                "query_onvif": false,
                "main_rtsp_url": lock_url,
                "timeout_seconds": 5,
                "credential_candidates": [
                    { "username_secret_reference": "{secret:CAM_USER}", "password_secret_reference": "{secret:CAM_BAD}" },
                    { "username_secret_reference": "{secret:CAM_USER}", "password_secret_reference": "{secret:CAM_GOOD}" }
                ]
            }),
            None,
        )
        .unwrap();
        lock_handle.join().unwrap();

        assert_eq!(lock_response["verified"], false);
        assert_eq!(lock_response["stop_reason"], "lockout_or_rate_limited");
        assert_eq!(lock_response["attempted_candidates"], 1);
        assert_eq!(lock_describes.load(Ordering::Acquire), 1);
    }

    enum MockRtspMode {
        RequireAuth { expected_basic: String },
        Lockout429,
    }

    fn spawn_mock_rtsp_server(
        mode: MockRtspMode,
    ) -> (u16, Arc<AtomicUsize>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let describes = Arc::new(AtomicUsize::new(0));
        let describes_clone = describes.clone();
        let handle = thread::spawn(move || {
            let max_connections = match mode {
                MockRtspMode::RequireAuth { .. } => 2,
                MockRtspMode::Lockout429 => 1,
            };
            for _ in 0..max_connections {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                handle_mock_rtsp_connection(&mut stream, &mode, &describes_clone, port);
            }
        });
        (port, describes, handle)
    }

    fn handle_mock_rtsp_connection(
        stream: &mut std::net::TcpStream,
        mode: &MockRtspMode,
        describes: &AtomicUsize,
        port: u16,
    ) {
        let mut buf = [0_u8; 4096];
        for _ in 0..8 {
            let Ok(read) = stream.read(&mut buf) else {
                return;
            };
            if read == 0 {
                return;
            }
            let text = String::from_utf8_lossy(&buf[..read]);
            let method = text.split_whitespace().next().unwrap_or("");
            let cseq = extract_cseq(&text);
            match method {
                "DESCRIBE" => {
                    describes.fetch_add(1, Ordering::AcqRel);
                    if !reply_mock_describe(stream, mode, &text, cseq) {
                        return;
                    }
                }
                "SETUP" => {
                    let response = format!(
                        "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nSession: mock\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1;ssrc=00000001\r\n\r\n"
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
                "PLAY" => {
                    reply_mock_play(stream, cseq, port);
                    return;
                }
                _ => return,
            }
        }
    }

    fn extract_cseq(text: &str) -> &str {
        text.lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("CSeq").then(|| value.trim())
            })
            .unwrap_or("1")
    }

    fn reply_mock_describe(
        stream: &mut std::net::TcpStream,
        mode: &MockRtspMode,
        text: &str,
        cseq: &str,
    ) -> bool {
        match mode {
            MockRtspMode::Lockout429 => {
                let response = format!("RTSP/1.0 429 Too Many Requests\r\nCSeq: {cseq}\r\n\r\n");
                let _ = stream.write_all(response.as_bytes());
                false
            }
            MockRtspMode::RequireAuth { expected_basic } => {
                let expected_header = format!("Basic {expected_basic}");
                let authorized = text.lines().any(|line| {
                    line.split_once(':').is_some_and(|(name, value)| {
                        name.eq_ignore_ascii_case("Authorization")
                            && value.trim() == expected_header
                    })
                });
                if !authorized {
                    let response = format!(
                        "RTSP/1.0 401 Unauthorized\r\nCSeq: {cseq}\r\nWWW-Authenticate: Basic realm=\"cam\"\r\n\r\n"
                    );
                    let _ = stream.write_all(response.as_bytes());
                    return true;
                }
                let sdp = concat!(
                    "v=0\r\n",
                    "o=- 1 1 IN IP4 127.0.0.1\r\n",
                    "s=mock\r\n",
                    "t=0 0\r\n",
                    "m=video 0 RTP/AVP 96\r\n",
                    "a=rtpmap:96 H264/90000\r\n",
                    "a=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAHukBQHsg,aM4G4g==\r\n",
                    "a=control:track1\r\n"
                );
                let response = format!(
                    "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}",
                    sdp.len()
                );
                let _ = stream.write_all(response.as_bytes());
                true
            }
        }
    }

    fn reply_mock_play(stream: &mut std::net::TcpStream, cseq: &str, port: u16) {
        let mut response = format!(
            "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nSession: mock\r\nRTP-Info: url=rtsp://127.0.0.1:{port}/main/track1;seq=1;rtptime=90000\r\n\r\n"
        )
        .into_bytes();
        // One H.264 IDR NAL packet (type 5) wrapped in RTP interleaved TCP framing.
        let rtp: [u8; 16] = [
            0x80, 0xe0, 0x00, 0x01, 0x00, 0x01, 0x5f, 0x90, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88,
            0x84, 0x00,
        ];
        let len = u16::try_from(rtp.len()).unwrap().to_be_bytes();
        response.extend_from_slice(&[b'$', 0, len[0], len[1]]);
        response.extend_from_slice(&rtp);
        let _ = stream.write_all(&response);
    }
}
