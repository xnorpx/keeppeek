use crate::{
    health::{CameraHealth, HealthIssue, ServerHealthResponse, StreamHealth},
    logging::LogHub,
};
use std::{collections::HashMap, time::Instant};

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
    pub duplicate_name: bool,
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
    pub kbit_per_second: Option<f64>,
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
        let (rows, total_rows) = collect_stream_rows(&health.cameras, health.uptime_seconds, hub);
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
            cpu_percent: health
                .system
                .process
                .cpu_capacity_percent
                .filter(|v| v.is_finite() && *v >= 0.0),
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

fn duplicate_camera_names(labels: &[(String, String)]) -> HashMap<&str, Option<&str>> {
    let mut first_id_by_name: HashMap<&str, Option<&str>> = HashMap::with_capacity(labels.len());
    for (name, id) in labels {
        if name.is_empty() {
            continue;
        }
        first_id_by_name
            .entry(name.as_str())
            .and_modify(|existing| {
                if *existing != Some(id.as_str()) {
                    *existing = None;
                }
            })
            .or_insert(Some(id.as_str()));
    }
    first_id_by_name
}

fn collect_stream_rows(
    cameras: &[CameraHealth],
    uptime_seconds: u64,
    hub: &LogHub,
) -> (Vec<StreamRow>, usize) {
    let camera_labels: Vec<(String, String)> = cameras
        .iter()
        .map(|camera| (for_terminal(hub, &camera.name), display_id(hub, &camera.id)))
        .collect();
    let first_id_by_name = duplicate_camera_names(&camera_labels);
    let mut rows = Vec::new();
    let mut total_rows = 0;
    for (camera, (name, id)) in cameras.iter().zip(&camera_labels) {
        let label = CameraLabel {
            name,
            id,
            duplicate_name: !name.is_empty() && first_id_by_name.get(name.as_str()) == Some(&None),
        };
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
                    StreamRow::new(camera, label, source, medium, hub, report, uptime_seconds);
                if let Some(profile) = profile {
                    apply_profile_defaults(&mut row, profile, medium, hub);
                }
                rows.push(row);
            }
        }
    }
    (rows, total_rows)
}

fn apply_profile_defaults(
    row: &mut StreamRow,
    profile: &crate::api::ProfileSummary,
    medium: &'static str,
    hub: &LogHub,
) {
    if medium == "video" {
        row.codec = row
            .codec
            .take()
            .or_else(|| profile.encoding.as_ref().map(|v| for_terminal(hub, v)));
        row.resolution = row
            .resolution
            .take()
            .or_else(|| profile.resolution.as_ref().map(|v| for_terminal(hub, v)));
        row.expected_fps = row
            .expected_fps
            .or_else(|| profile.framerate.and_then(finite_positive_f64));
    } else if let Some(audio) = &profile.audio {
        row.codec = row
            .codec
            .take()
            .or_else(|| Some(for_terminal(hub, &audio.encoding)));
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

#[derive(Clone, Copy)]
struct CameraLabel<'a> {
    name: &'a str,
    id: &'a str,
    duplicate_name: bool,
}

impl StreamRow {
    fn new(
        camera: &CameraHealth,
        label: CameraLabel<'_>,
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
                observed_fps: video.then_some(r.fps).and_then(finite_non_negative_f64),
                kbit_per_second: decimal_kbit_per_second(r.kbps),
                report_freshness_ms: s.dimensions.report_freshness_threshold_ms,
                frames: r.frames,
                bytes: r.bytes,
                drops: video.then_some(r.drops).flatten(),
                errors: video.then_some(r.errors).flatten(),
                reconnects: video.then_some(r.reconnects).flatten(),
                keyframe_age_ms: video.then_some(s.ingress.keyframe_age_ms).flatten(),
                keyframes_per_second: video.then_some(r.kf_fps).and_then(finite_non_negative_f64),
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
            camera: label.name.to_owned(),
            id: label.id.to_owned(),
            duplicate_name: label.duplicate_name,
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
            expected_fps: r
                .and_then(|r| video.then_some(r.expected_fps))
                .and_then(finite_positive_f64),
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

fn finite_non_negative_f64(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn finite_positive_f64(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

// IngressRates calculates bits per 1024; display the decimal network unit used in the header.
fn decimal_kbit_per_second(kibit_per_second: f64) -> Option<f64> {
    let valid = finite_non_negative_f64(kibit_per_second)?;
    finite_non_negative_f64(valid * 1024.0 / 1000.0)
}

#[cfg(test)]
pub(super) fn test_server_health(
    cameras: Vec<CameraHealth>,
    issues: Vec<HealthIssue>,
    uptime_seconds: u64,
) -> ServerHealthResponse {
    use crate::{
        health::{
            HealthTotals, LoadHealth, MemoryHealth, ProcessHealth, StorageHealth, SystemHealth,
        },
        storage::{RecordingDemand, safety::StorageSafetyHealthSnapshot},
        webrtc::WebRtc,
    };
    use std::time::Duration;

    let status = if issues
        .iter()
        .any(|issue| matches!(issue.severity.as_str(), "critical" | "warning"))
    {
        "degraded"
    } else {
        "healthy"
    };
    ServerHealthResponse {
        status: status.into(),
        health_contract_version: 1,
        generated_at_ms: 0,
        uptime_seconds,
        version: "test",
        totals: HealthTotals::default(),
        system: SystemHealth {
            host_name: None,
            os_name: None,
            os_version: None,
            kernel_version: None,
            architecture: "test",
            system_uptime_seconds: 0,
            boot_time_seconds: 0,
            logical_cores: 1,
            physical_cores: Some(1),
            cpu_brand: None,
            system_cpu_percent: 0.0,
            process: ProcessHealth {
                pid: 1,
                name: None,
                executable: None,
                working_directory: None,
                cpu_percent: None,
                cpu_capacity_percent: None,
                cpu_core_equivalents: None,
                resident_memory_bytes: None,
                memory_capacity_percent: None,
                virtual_memory_bytes: None,
                started_at_seconds: None,
                uptime_seconds: None,
                tasks: None,
                read_bytes_per_second: None,
                write_bytes_per_second: None,
                total_read_bytes: None,
                total_written_bytes: None,
            },
            memory: MemoryHealth {
                total_bytes: 0,
                used_bytes: 0,
                available_bytes: 0,
                total_swap_bytes: 0,
                used_swap_bytes: 0,
            },
            load: LoadHealth {
                one_minute: 0.0,
                five_minutes: 0.0,
                fifteen_minutes: 0.0,
            },
            cpus: Vec::new(),
            network_egress_bps: 0,
            networks: Vec::new(),
            disks: Vec::new(),
            temperatures: Vec::new(),
        },
        storage: StorageHealth {
            medium_term_path: String::new(),
            long_term_path: String::new(),
            paths_are_same: true,
            short_term_seconds: 0,
            medium_term_seconds: 0,
            flush_interval_seconds: 0,
            write_buffer_bytes: 0,
            long_term_max_bytes: 0,
            minimum_free_bytes: 0,
            maximum_used_percent: None,
            warning_free_bytes: 0,
            critical_free_bytes: 0,
            cleanup_hysteresis_bytes: 0,
            catalog_bytes: None,
            catalog: None,
            safety: StorageSafetyHealthSnapshot::default(),
            demand: RecordingDemand::new(Duration::ZERO).health_snapshot(),
        },
        webrtc: WebRtc::new().health_snapshot(),
        cameras,
        issues,
        operational_events: Vec::new(),
    }
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
    fn decimal_rate_preserves_zero_scales_kibibits_and_rejects_invalid_floats() {
        assert_eq!(decimal_kbit_per_second(0.0), Some(0.0));
        assert_eq!(decimal_kbit_per_second(1000.0), Some(1024.0));
        assert_eq!(decimal_kbit_per_second(f64::NAN), None);
        assert_eq!(decimal_kbit_per_second(f64::INFINITY), None);
        assert_eq!(decimal_kbit_per_second(-1.0), None);
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
        let label = CameraLabel {
            name: "Front",
            id: "front",
            duplicate_name: false,
        };
        let off = make_stream(false);
        let row_off = StreamRow::new(&camera, label, "main", "video", &hub, Some(&off), 60);
        assert_eq!(row_off.measurement.unwrap().recording_age_ms, None);

        let on = make_stream(true);
        let row_on = StreamRow::new(&camera, label, "main", "video", &hub, Some(&on), 60);
        assert_eq!(row_on.measurement.unwrap().recording_age_ms, Some(5_000));
    }

    fn base_camera(
        id: &str,
        name: &str,
        state: crate::health::CameraHealthState,
        reason: crate::health::CameraHealthReason,
    ) -> CameraHealth {
        use crate::health::CameraHealthDimensions;
        CameraHealth {
            id: id.into(),
            ip: "192.0.2.10".into(),
            name: name.into(),
            manufacturer: None,
            model: None,
            firmware_version: None,
            backend: "retina".into(),
            transport: "tcp".into(),
            state,
            reason,
            reason_codes: vec![reason],
            detail: reason.detail().into(),
            dimensions: CameraHealthDimensions {
                configured: true,
                expected: true,
                configured_video_streams: 1,
                connected_video_streams: Some(0),
                reporting_video_streams: 0,
                fresh_video_streams: 0,
                decodable_video_streams: 0,
                configured_video_stream_ids: vec!["main".into()],
                connected_video_stream_ids: Some(Vec::new()),
                reporting_video_stream_ids: Vec::new(),
                fresh_video_stream_ids: Vec::new(),
                decodable_video_stream_ids: Vec::new(),
                transport_connected: Some(false),
                latest_report_at_ms: None,
                report_age_ms: None,
                frames_fresh: None,
                decodable: None,
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
                session_duration_ms: None,
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
        }
    }

    fn base_health(cameras: Vec<CameraHealth>, issues: Vec<HealthIssue>) -> ServerHealthResponse {
        test_server_health(cameras, issues, 120)
    }

    #[test]
    fn snapshot_tracks_empty_disconnected_sleeping_127_camera_and_inventory_changes() {
        use crate::{
            api::{AudioProfileSummary, ProfileSummary},
            health::{CameraHealthReason as Reason, CameraHealthState as State},
        };
        let hub = LogHub::default();
        let empty = Snapshot::from_health(base_health(Vec::new(), Vec::new()), &hub);
        assert!(empty.rows.is_empty());
        assert_eq!(empty.total_rows, 0);

        let disconnected = base_camera(
            "cam-offline",
            "Gate",
            State::Offline,
            Reason::TransportDisconnected,
        );
        let mut sleeping =
            base_camera("cam-sleep", "Gate", State::Stopped, Reason::BatterySleeping);
        sleeping.dimensions.battery_configured = true;
        sleeping.dimensions.battery_sleeping = Some(true);
        let snap =
            Snapshot::from_health(base_health(vec![sleeping, disconnected], Vec::new()), &hub);
        assert_eq!(snap.rows.len(), 2);
        assert_eq!(snap.rows[0].id, "cam-offline");
        assert_eq!(snap.rows[0].health, "offline");
        assert_eq!(snap.rows[0].reason, "transport_disconnected");
        assert!(snap.rows[0].duplicate_name);
        assert_eq!(snap.rows[1].id, "cam-sleep");
        assert_eq!(snap.rows[1].health, "stopped");
        assert_eq!(snap.rows[1].reason, "battery_sleeping");
        assert!(snap.rows[1].duplicate_name);

        let build_fleet = |range: std::ops::RangeInclusive<u16>, connected_first: bool| {
            let profile = |stream: &str| ProfileSummary {
                name: stream.into(),
                stream: stream.into(),
                encoding: Some("H264".into()),
                resolution: Some("1920x1080".into()),
                framerate: Some(25.0),
                bitrate_kbps: Some(2000),
                gop: Some(25),
                h264_profile: None,
                audio: Some(AudioProfileSummary {
                    encoding: "AAC".into(),
                    sample_rate: Some(48_000),
                    bitrate_kbps: Some(64),
                }),
            };
            range
                .enumerate()
                .map(|(offset, index)| {
                    let mut cam = base_camera(
                        &format!("cam-{index:03}"),
                        &format!("Camera {index:03}"),
                        State::Offline,
                        Reason::TransportDisconnected,
                    );
                    cam.dimensions.configured_video_streams = 2;
                    cam.dimensions.configured_video_stream_ids = vec!["main".into(), "sub".into()];
                    if connected_first && offset == 0 {
                        cam.dimensions.connected_video_stream_ids =
                            Some(vec!["main".into(), "sub".into()]);
                    }
                    cam.configured_profiles = vec![profile("main"), profile("sub")];
                    cam
                })
                .collect::<Vec<_>>()
        };

        let full =
            Snapshot::from_health(base_health(build_fleet(1..=127, false), Vec::new()), &hub);
        assert_eq!(full.rows.len(), 127 * 4);
        assert_eq!(full.total_rows, 127 * 4);
        assert_eq!(full.rows[0].id, "cam-001");
        assert_eq!(
            (full.rows[0].stream, full.rows[0].medium),
            ("main", "video")
        );
        assert_eq!(
            (full.rows[1].stream, full.rows[1].medium),
            ("main", "audio")
        );
        assert_eq!((full.rows[2].stream, full.rows[2].medium), ("sub", "video"));
        assert_eq!((full.rows[3].stream, full.rows[3].medium), ("sub", "audio"));

        let updated =
            Snapshot::from_health(base_health(build_fleet(2..=127, true), Vec::new()), &hub);
        assert_eq!(updated.rows.len(), 126 * 4);
        assert_eq!(updated.rows[0].id, "cam-002");
        assert_eq!(updated.rows[0].health, "stale");
    }

    #[test]
    fn snapshot_preserves_canonical_states_storage_pressure_and_warning_order() {
        use crate::{
            health::{CameraHealthReason as Reason, CameraHealthState as State},
            storage::safety::{StoragePressure, StorageRecordingState},
        };
        let hub = LogHub::default();
        let cameras = vec![
            base_camera("cam-1", "One", State::Starting, Reason::Starting),
            base_camera(
                "cam-2",
                "Two",
                State::Offline,
                Reason::TransportDisconnected,
            ),
            base_camera("cam-3", "Three", State::Stopped, Reason::BatterySleeping),
        ];
        let issues = vec![
            HealthIssue {
                severity: "warning".into(),
                scope: "cam-2".into(),
                message: Reason::RecordingNotProgressing.detail().into(),
                operational_event_id: None,
                timeline_start_ms: None,
                timeline_end_ms: None,
            },
            HealthIssue {
                severity: "critical".into(),
                scope: "storage".into(),
                message: "Recording paused due to low disk space".into(),
                operational_event_id: None,
                timeline_start_ms: None,
                timeline_end_ms: None,
            },
        ];
        let mut health = base_health(cameras, issues);
        health.storage.safety.pressure = StoragePressure::Critical;
        health.storage.safety.recording_state = StorageRecordingState::Paused;
        health.storage.safety.keeppeek_bytes = Some(512 * 1024 * 1024);
        health.storage.safety.available_bytes = Some(128 * 1024 * 1024);
        health.system.process.cpu_capacity_percent = Some(f32::NAN);

        let snap = Snapshot::from_health(health, &hub);
        assert_eq!(snap.health, "degraded");
        assert_eq!(snap.cpu_percent, None);
        assert_eq!(snap.storage_pressure, "critical");
        assert_eq!(snap.storage_state, "paused");
        assert_eq!(snap.storage_used, Some(512 * 1024 * 1024));
        assert_eq!(snap.storage_free, Some(128 * 1024 * 1024));
        assert!(snap.warnings[0].starts_with("critical"));
        assert!(snap.warnings[1].contains("Requested recording writes are not progressing"));
        assert_eq!(snap.rows[0].health, "starting");
        assert_eq!(snap.rows[1].health, "offline");
        assert_eq!(snap.rows[2].health, "stopped");
    }

    #[test]
    fn counter_reset_produces_zero_rates_and_non_finite_metrics_stay_unavailable() {
        use crate::stats::IngressSnapshot;
        let previous = IngressSnapshot {
            uptime_secs: 20.0,
            reconnects: 1,
            video_frames: 500,
            video_keyframes: 25,
            video_bytes: 5_000_000,
            video_max_frame: 30_000,
            audio_frames: 1_000,
            audio_bytes: 200_000,
            audio_max_frame: 200,
            dropped_frames: 0,
            error_count: 0,
            gap_min_ms: 33.0,
            gap_max_ms: 40.0,
            gap_avg_ms: 35.0,
            jitter_samples: 10,
            jitter_p50_ms: 1.0,
            jitter_p99_ms: 2.0,
        };
        let reset = IngressSnapshot {
            uptime_secs: 30.0,
            video_frames: 50,
            video_keyframes: 2,
            video_bytes: 100_000,
            audio_frames: 100,
            audio_bytes: 10_000,
            ..previous
        };
        let rates = reset.rates_since(&previous);
        assert_eq!(rates.video_fps, 0.0);
        assert_eq!(rates.video_kbps, 0.0);
        assert_eq!(rates.audio_kbps, 0.0);
        assert_eq!(decimal_kbit_per_second(rates.video_kbps), Some(0.0));
        assert_eq!(finite_non_negative_f64(f64::NAN), None);
        assert_eq!(finite_positive_f64(f64::INFINITY), None);
    }
}
