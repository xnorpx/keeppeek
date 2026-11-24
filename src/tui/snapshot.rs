use crate::{
    health::{CameraHealth, HealthIssue, ServerHealthResponse, StreamHealth},
    logging::LogHub,
};
use std::time::Instant;

use super::text::for_terminal;

pub(super) const MAX_ROWS: usize = 1024;
const MAX_ISSUES: usize = 20;

pub(super) struct Snapshot {
    pub collected: Instant,
    pub version: &'static str,
    pub uptime_s: u64,
    pub health: String,
    pub cpu_percent: Option<f32>,
    pub memory_bytes: Option<u64>,
    pub storage_used: Option<u64>,
    pub storage_free: Option<u64>,
    pub storage_pressure: &'static str,
    pub storage_state: &'static str,
    pub connected_video: usize,
    pub expected_video: usize,
    pub recording_video: usize,
    pub viewer_sessions: usize,
    pub rows: Vec<StreamRow>,
    pub total_rows: usize,
    pub warnings: Vec<String>,
    pub omitted_warnings: usize,
}

pub(super) struct StreamRow {
    pub camera: String,
    pub id: String,
    pub stream: &'static str,
    pub medium: &'static str,
    pub health: &'static str,
    pub reason: String,
    pub codec: Option<String>,
    pub resolution: Option<String>,
    pub expected_fps: Option<f64>,
    pub measurement: Option<Measurement>,
}

pub(super) struct Measurement {
    pub connected: Option<bool>,
    pub observed_fps: Option<f64>,
    pub kbit_per_second: f64,
    pub report_freshness_ms: u64,
    pub frames: Option<u64>,
    pub bytes: Option<u64>,
    pub drops: Option<u64>,
    pub errors: Option<u64>,
    pub reconnects: Option<u64>,
    pub keyframe_age_ms: Option<u64>,
    pub keyframes_per_second: Option<f64>,
    pub session_ms: u64,
    pub report_age_ms: u64,
    pub frame_age_ms: Option<u64>,
    pub recording_requested: Option<bool>,
    pub recording_progress: Option<bool>,
    pub recording_age_ms: Option<u64>,
}

impl Snapshot {
    pub fn from_health(mut health: ServerHealthResponse, hub: &LogHub) -> Self {
        // Keep the viewport order independent of the registries' iteration order.
        health.cameras.sort_by(|a, b| a.id.cmp(&b.id));
        let mut rows = Vec::new();
        let mut total_rows = 0;
        for camera in &health.cameras {
            for (source, video, audio) in [
                ("main", "video_main", "audio_main"),
                ("sub", "video_sub", "audio_sub"),
            ] {
                let profile = camera
                    .configured_profiles
                    .iter()
                    .find(|p| p.stream == source);
                let expected_video = camera
                    .dimensions
                    .configured_video_stream_ids
                    .iter()
                    .any(|id| id == source);
                for (medium, kind, expected) in [
                    ("video", video, expected_video),
                    ("audio", audio, profile.is_some_and(|p| p.audio.is_some())),
                ] {
                    let report = camera
                        .streams
                        .iter()
                        .find(|s| s.ingress.report.kind == kind);
                    if !expected && report.is_none() {
                        continue;
                    }
                    total_rows += 1;
                    if rows.len() == MAX_ROWS {
                        continue;
                    }
                    let mut row =
                        StreamRow::new(camera, source, medium, hub, report, health.uptime_seconds);
                    if let Some(profile) = profile {
                        if medium == "video" {
                            row.codec = row.codec.or_else(|| {
                                profile.encoding.as_ref().map(|v| for_terminal(hub, v))
                            });
                            row.resolution = row.resolution.or_else(|| {
                                profile.resolution.as_ref().map(|v| for_terminal(hub, v))
                            });
                            row.expected_fps = row
                                .expected_fps
                                .or_else(|| profile.framerate.filter(|v| *v > 0.0));
                        } else if let Some(audio) = &profile.audio {
                            row.codec = row
                                .codec
                                .or_else(|| Some(for_terminal(hub, &audio.encoding)));
                        }
                    }
                    rows.push(row);
                }
            }
        }
        health.issues.sort_by_key(severity);
        let omitted_warnings = health.issues.len().saturating_sub(MAX_ISSUES);
        let warnings = health
            .issues
            .into_iter()
            .take(MAX_ISSUES)
            .map(|issue| warning_line(hub, &issue))
            .collect();
        let storage = &health.storage.safety;
        Self {
            collected: Instant::now(),
            version: health.version,
            uptime_s: health.uptime_seconds,
            health: for_terminal(hub, &health.status),
            cpu_percent: health.system.process.cpu_capacity_percent,
            memory_bytes: health.system.process.resident_memory_bytes,
            storage_used: storage.keeppeek_bytes,
            storage_free: storage.available_bytes,
            storage_pressure: storage.pressure.as_str(),
            storage_state: storage.recording_state.as_str(),
            connected_video: health.totals.connected_video_streams,
            expected_video: health.totals.configured_video_streams,
            recording_video: health.totals.recording_video_streams,
            viewer_sessions: health.webrtc.active_sessions,
            rows,
            total_rows,
            warnings,
            omitted_warnings,
        }
    }
}

fn display_id(hub: &LogHub, id: &str) -> String {
    let safe = for_terminal(hub, id);
    if !id.is_empty() && id == safe {
        return safe;
    }
    // The canonical id can be a private camera address. Keep rows distinguishable after redaction.
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(id.as_bytes());
    format!(
        "camera-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    )
}

fn warning_line(hub: &LogHub, issue: &HealthIssue) -> String {
    let scope = display_id(hub, &issue.scope);
    let severity = for_terminal(hub, &issue.severity);
    let message = for_terminal(hub, &issue.message);
    for_terminal(hub, &format!("{severity}  {scope}  {message}"))
}

fn severity(issue: &HealthIssue) -> u8 {
    match issue.severity.as_str() {
        "critical" => 0,
        "error" => 1,
        "warning" => 2,
        _ => 3,
    }
}

impl StreamRow {
    fn new(
        camera: &CameraHealth,
        source: &'static str,
        medium: &'static str,
        hub: &LogHub,
        stream: Option<&StreamHealth>,
        uptime: u64,
    ) -> Self {
        let video = medium == "video";
        let r = stream.map(|s| &s.ingress.report);
        let (health, reason) = stream.map_or_else(
            || {
                missing_stream_health(
                    camera.state,
                    camera.reason,
                    camera
                        .dimensions
                        .connected_video_stream_ids
                        .as_ref()
                        .map(|ids| ids.iter().any(|id| id == source)),
                    uptime,
                )
            },
            |s| (s.state, s.reason),
        );
        let measurement = stream.map(|s| {
            let r = &s.ingress.report;
            Measurement {
                connected: s.dimensions.transport_connected,
                observed_fps: video.then_some(r.fps),
                kbit_per_second: decimal_kbit_per_second(r.kbps),
                report_freshness_ms: s.dimensions.report_freshness_threshold_ms,
                frames: r.frames,
                bytes: r.bytes,
                drops: video.then_some(r.drops).flatten(),
                errors: video.then_some(r.errors).flatten(),
                reconnects: video.then_some(r.reconnects).flatten(),
                keyframe_age_ms: video.then_some(s.ingress.keyframe_age_ms).flatten(),
                keyframes_per_second: video.then_some(r.kf_fps),
                session_ms: r.session_duration_ms,
                report_age_ms: s.ingress.report_age_ms,
                frame_age_ms: s.ingress.frame_age_ms,
                recording_requested: video.then_some(s.dimensions.recording_requested),
                recording_progress: video
                    .then_some(s.dimensions.recording_progressing)
                    .flatten(),
                recording_age_ms: (video && s.dimensions.recording_requested)
                    .then_some(s.dimensions.recording_progress_age_ms)
                    .flatten(),
            }
        });
        Self {
            camera: for_terminal(hub, &camera.name),
            id: display_id(hub, &camera.id),
            stream: source,
            medium,
            health: health.as_str(),
            reason: reason.as_str().to_owned(),
            codec: r
                .and_then(|r| r.codec.as_deref())
                .map(|v| for_terminal(hub, v)),
            resolution: r
                .and_then(|r| r.resolution.as_deref())
                .map(|v| for_terminal(hub, v)),
            expected_fps: r.map(|r| r.expected_fps).filter(|v| video && *v > 0.0),
            measurement,
        }
    }
}

fn missing_stream_health(
    camera: crate::health::CameraHealthState,
    reason: crate::health::CameraHealthReason,
    connected: Option<bool>,
    uptime: u64,
) -> (
    crate::health::CameraHealthState,
    crate::health::CameraHealthReason,
) {
    use crate::{
        api::CameraLifecycle,
        health::{CameraHealthEvidence, CameraHealthState, project_camera_health},
    };
    if matches!(
        camera,
        CameraHealthState::Starting | CameraHealthState::Stopped
    ) {
        return (camera, reason);
    }
    let projection = project_camera_health(&CameraHealthEvidence {
        expected: true,
        lifecycle: connected.map(|yes| {
            if yes {
                CameraLifecycle::Connected
            } else {
                CameraLifecycle::Reconnecting
            }
        }),
        startup_grace: crate::health::in_startup_grace(uptime),
        report_age_ms: None,
        frames_fresh: None,
        decodable: None,
        frame_rate_healthy: None,
        recent_reconnects: 0,
        recent_drops: 0,
        recent_errors: 0,
        recording_requested: false,
        recording_progressing: None,
        battery_sleeping: None,
    });
    (projection.state, projection.reason)
}

// IngressRates calculates bits per 1024; display the decimal network unit used in the header.
fn decimal_kbit_per_second(kibit_per_second: f64) -> f64 {
    kibit_per_second * 1024.0 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_expected_stream_uses_its_own_evidence() {
        use crate::health::{CameraHealthReason as Reason, CameraHealthState as State};
        for (camera, reason, connected, uptime, expected) in [
            (
                State::Healthy,
                Reason::Healthy,
                Some(true),
                60,
                (State::Stale, Reason::NoStreamReport),
            ),
            (
                State::Healthy,
                Reason::Healthy,
                Some(true),
                1,
                (State::Starting, Reason::Starting),
            ),
            (
                State::Healthy,
                Reason::Healthy,
                None,
                60,
                (State::Unknown, Reason::EvidenceUnavailable),
            ),
            (
                State::Stopped,
                Reason::BatterySleeping,
                None,
                60,
                (State::Stopped, Reason::BatterySleeping),
            ),
        ] {
            assert_eq!(
                missing_stream_health(camera, reason, connected, uptime),
                expected
            );
        }
    }

    #[test]
    fn warnings_distinguish_redacted_camera_scopes_with_the_same_message() {
        let hub = LogHub::default();
        let message = "One or more streams are not receiving fresh frames";
        let line = |scope: &str| {
            warning_line(
                &hub,
                &HealthIssue {
                    severity: "warning".into(),
                    scope: scope.into(),
                    message: message.into(),
                    operational_event_id: None,
                    timeline_start_ms: None,
                    timeline_end_ms: None,
                },
            )
        };
        let first = line("rtsp://operator:canary-one@host/live");
        let second = line("rtsp://operator:canary-two@host/live");
        assert_ne!(first, second);
        for value in [first, second] {
            assert!(value.contains("camera-") && value.ends_with(message));
            assert!(!value.contains("canary") && !value.contains("operator"));
        }
        assert!(line("storage").contains("storage"));
    }

    #[test]
    fn decimal_rate_preserves_zero_and_scales_incoming_kibibits() {
        assert_eq!(decimal_kbit_per_second(0.0), 0.0);
        assert_eq!(decimal_kbit_per_second(1000.0), 1024.0);
    }

    #[test]
    fn stale_recording_age_is_hidden_when_recording_is_not_requested() {
        use crate::health::{
            CameraHealthDimensions, CameraHealthReason, CameraHealthState, StreamHealthDimensions,
        };
        use crate::stats::{StreamHealthReport, StreamReport};

        let make_stream = |recording_requested| StreamHealth {
            ingress: StreamHealthReport {
                report: StreamReport {
                    kind: "video_main".into(),
                    session_duration_ms: 10_000,
                    codec: None,
                    resolution: None,
                    fps: 15.0,
                    expected_fps: 15.0,
                    kf_fps: 1.0,
                    kbps: 500.0,
                    max_frame_kb: 30.0,
                    gap_min_ms: 60.0,
                    gap_avg_ms: 66.0,
                    gap_max_ms: 80.0,
                    jitter_samples: 10,
                    jitter_p50_ms: 1.0,
                    jitter_p99_ms: 2.0,
                    frames: Some(150),
                    bytes: Some(500_000),
                    keyframes: Some(10),
                    reconnects: None,
                    drops: None,
                    errors: None,
                },
                updated_at_ms: 10_000,
                report_age_ms: 100,
                frame_updated_at_ms: Some(10_000),
                frame_age_ms: Some(100),
                keyframe_updated_at_ms: Some(10_000),
                keyframe_age_ms: Some(100),
                recent_reconnects: 0,
                recent_drops: 0,
                recent_errors: 0,
            },
            state: CameraHealthState::Healthy,
            reason: CameraHealthReason::Healthy,
            reason_codes: vec![CameraHealthReason::Healthy],
            detail: String::new(),
            dimensions: StreamHealthDimensions {
                expected: true,
                transport_connected: Some(true),
                report_fresh: true,
                report_freshness_threshold_ms: 30_000,
                frames_fresh: true,
                frame_freshness_threshold_ms: 30_000,
                decodable: true,
                keyframe_freshness_threshold_ms: 30_000,
                recent_reconnects: 0,
                recent_drops: 0,
                recent_errors: 0,
                recording_requested,
                recording_progressing: Some(true),
                recording_progress_age_ms: Some(5_000),
                session_duration_ms: 10_000,
                recorded_duration_ms: 10_000,
            },
        };
        let camera = CameraHealth {
            id: "front".into(),
            ip: "192.0.2.10".into(),
            name: "Front".into(),
            manufacturer: None,
            model: None,
            firmware_version: None,
            backend: "retina".into(),
            transport: "tcp".into(),
            state: CameraHealthState::Healthy,
            reason: CameraHealthReason::Healthy,
            reason_codes: vec![CameraHealthReason::Healthy],
            detail: String::new(),
            dimensions: CameraHealthDimensions {
                configured: true,
                expected: true,
                configured_video_streams: 1,
                connected_video_streams: Some(1),
                reporting_video_streams: 1,
                fresh_video_streams: 1,
                decodable_video_streams: 1,
                configured_video_stream_ids: vec!["main".into()],
                connected_video_stream_ids: Some(vec!["main".into()]),
                reporting_video_stream_ids: vec!["main".into()],
                fresh_video_stream_ids: vec!["main".into()],
                decodable_video_stream_ids: vec!["main".into()],
                transport_connected: Some(true),
                latest_report_at_ms: Some(10_000),
                report_age_ms: Some(100),
                frames_fresh: Some(true),
                decodable: Some(true),
                recent_reconnects: 0,
                recent_drops: 0,
                recent_errors: 0,
                recording_requested: false,
                recording_video_streams: 0,
                recording_streams_progressing: 0,
                recording_video_stream_ids: Vec::new(),
                recording_progressing_stream_ids: Vec::new(),
                recording_progressing: None,
                recording_progress_age_ms: None,
                session_duration_ms: Some(10_000),
                recorded_main_duration_ms: 0,
                recorded_sub_duration_ms: 0,
                recorded_total_duration_ms: 0,
                battery_configured: false,
                battery_registered: None,
                battery_last_seen_age_ms: None,
                battery_wake_pending_age_ms: None,
                battery_sleeping: None,
            },
            lifecycle: None,
            last_error: None,
            configured_profiles: Vec::new(),
            streams: Vec::new(),
        };
        let hub = LogHub::default();
        let off = make_stream(false);
        let row_off = StreamRow::new(&camera, "main", "video", &hub, Some(&off), 60);
        assert_eq!(row_off.measurement.unwrap().recording_age_ms, None);

        let on = make_stream(true);
        let row_on = StreamRow::new(&camera, "main", "video", &hub, Some(&on), 60);
        assert_eq!(row_on.measurement.unwrap().recording_age_ms, Some(5_000));
    }
}
