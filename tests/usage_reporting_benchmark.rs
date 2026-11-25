//! Release-build AC-10 ingest and recording isolation benchmark for usage reporting.
//!
//! Run with:
//! `cargo test --release --test usage_reporting_benchmark -- --ignored --nocapture`

use keeppeek::{
    api::{CameraInfo, ProfileSummary},
    cameras::{CameraCapabilities, CameraPorts},
    config::{Config, usage_reporting::UsageReportingConfig},
    storage::{
        MediaFrame, RecordingFrame, StorageConfig, StorageEngine, VideoCodec, VideoFrame,
        nal::annexb_to_avcc,
    },
    usage_reporting::{
        model::{CURRENT_DISCLOSURE_REVISION, MAX_CAMERA_GROUPS, MAX_PAYLOAD_BYTES},
        projection::{ConsentSelection, ProjectionHealthInput, ProjectionSnapshot, project_report},
        sender::{
            Clock, JitterSource, TelemetryTransport, TransportResponse, UsageReportingController,
        },
        state::FailureKind,
    },
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use test_frame::{EncodedFrame, TestFrameConfig, TestFrameSource};

const WARMUP_TRIALS: usize = 5;
const MEASURED_TRIALS: usize = 30;
const FRAMES_PER_TRIAL: usize = 60;
const CAMERA_ID: &str = "usage-bench-cam-1";

struct FixedClock(i64);
impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0
    }
}

struct ZeroJitter;
impl JitterSource for ZeroJitter {
    fn non_negative_jitter_ms(&self, _max_ms: i64) -> i64 {
        0
    }
}

struct CountingTransport(AtomicU64);
impl TelemetryTransport for CountingTransport {
    fn put_report(
        &self,
        _origin: &str,
        _installation_id: &str,
        _installation_token: &str,
        body: &[u8],
    ) -> Result<TransportResponse, FailureKind> {
        assert!(body.len() <= MAX_PAYLOAD_BYTES);
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(TransportResponse { status: 200 })
    }

    fn delete_installation(
        &self,
        _origin: &str,
        _installation_id: &str,
        _installation_token: &str,
    ) -> Result<TransportResponse, FailureKind> {
        Ok(TransportResponse { status: 204 })
    }
}

fn bench_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-usage-bench-{name}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("create benchmark dir");
    dir
}

fn storage_config(root: PathBuf) -> StorageConfig {
    StorageConfig {
        medium_term_path: root.clone(),
        long_term_path: root.clone(),
        recording_catalog_path: root.join("recordings.db"),
        event_thumbnail_path: root.join(".event-thumbnails"),
        event_thumbnail_max_bytes: 128 * 1_048_576,
        pre_recording_stream_max_bytes: 16 * 1024 * 1024,
        pre_recording_global_max_bytes: 64 * 1024 * 1024,
        short_term_duration: Duration::from_secs(2),
        medium_term_duration: Duration::from_secs(5),
        flush_interval: Duration::from_secs(1),
        write_buffer_bytes: 8 * 1024,
        long_term_max_bytes: 0,
        minimum_free_bytes: 0,
        maximum_used_percent: None,
        warning_free_bytes: 0,
        critical_free_bytes: 0,
        cleanup_hysteresis_bytes: 0,
        ..StorageConfig::default()
    }
}

fn max_valid_cameras() -> Vec<CameraInfo> {
    let mut cameras = Vec::with_capacity(MAX_CAMERA_GROUPS);
    let codecs = ["H264", "H265", "MJPEG", "VP8", "VP9", "AV1"];
    let transports = ["tcp", "udp"];
    let resolutions = ["640x480", "1280x720", "1920x1080", "2560x1440", "3840x2160"];
    let backends = ["retina", "reo-proto", "auto"];
    'outer: for codec in codecs {
        for transport in transports {
            for resolution in resolutions {
                for backend in backends {
                    for ptz in [false, true] {
                        for audio in [false, true] {
                            cameras.push(CameraInfo {
                                id: format!("bench-cam-{}", cameras.len()),
                                ip: "192.0.2.10".to_owned(),
                                name: None,
                                manufacturer: Some("Reolink".to_owned()),
                                model: Some("RLC-810A".to_owned()),
                                firmware_version: None,
                                serial_number: None,
                                hardware_id: None,
                                hostname: None,
                                mac_address: None,
                                is_reolink: true,
                                backend: backend.to_owned(),
                                transport: transport.to_owned(),
                                web_url: String::new(),
                                ports: CameraPorts::default(),
                                capabilities: CameraCapabilities {
                                    ptz,
                                    audio,
                                    events: true,
                                    recording: true,
                                    ..CameraCapabilities::default()
                                },
                                profiles: vec![ProfileSummary {
                                    name: "main".to_owned(),
                                    stream: "main".to_owned(),
                                    encoding: Some(codec.to_owned()),
                                    resolution: Some(resolution.to_owned()),
                                    framerate: Some(25.0),
                                    bitrate_kbps: Some(4096),
                                    gop: Some(50),
                                    h264_profile: None,
                                    audio: None,
                                }],
                            });
                            if cameras.len() == MAX_CAMERA_GROUPS {
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
    }
    cameras
}

fn sample_health() -> ProjectionHealthInput {
    ProjectionHealthInput {
        version: "0.1.0-alpha.1".to_owned(),
        os_name: Some("Linux".to_owned()),
        os_version: Some("24.04".to_owned()),
        architecture: std::env::consts::ARCH.to_owned(),
        cpu_brand: Some("Intel(R) Core(TM) i7-13700K".to_owned()),
        physical_cores: Some(4),
        logical_cores: 8,
        memory_total_bytes: 32 * 1024 * 1024 * 1024,
        configured_cameras: MAX_CAMERA_GROUPS,
        connected_cameras: MAX_CAMERA_GROUPS,
        decodable_cameras: MAX_CAMERA_GROUPS,
        recording_cameras: MAX_CAMERA_GROUPS,
        long_term_max_bytes: 4 * 1024 * 1024 * 1024 * 1024,
    }
}

fn generate_encoded_frames(count: usize) -> Vec<EncodedFrame> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<EncodedFrame>(count + 8);
    let source = TestFrameSource::start(
        TestFrameConfig {
            width: 320,
            height: 240,
            fps: 30,
            codec: test_frame::Codec::H264,
            keyframe_interval: 15,
            bitrate_bps: 500_000,
        },
        move |frame| {
            let _ = tx.send(frame);
        },
    )
    .expect("start deterministic H.264 test source");
    let mut frames = Vec::with_capacity(count);
    while frames.len() < count {
        frames.push(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("encoded frame"),
        );
    }
    source.stop();
    frames
}

fn to_recording_frame(frame: &EncodedFrame) -> RecordingFrame {
    RecordingFrame {
        source_rtp: None,
        received_at: Instant::now(),
        timestamp: None,
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: frame.is_keyframe,
            width: frame.width,
            height: frame.height,
            data: annexb_to_avcc(&frame.data).into(),
        }),
    }
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    let index = ((sorted.len() as f64 * quantile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[index]
}

#[test]
#[ignore = "Release-build AC-10 usage reporting isolation benchmark"]
fn usage_reporting_ingest_isolation_30_trials() {
    let frames = generate_encoded_frames(FRAMES_PER_TRIAL);
    let cameras = Arc::new(max_valid_cameras());
    let config = Arc::new(Config::default());
    let health = Arc::new(sample_health());

    // Measure standalone projection + serialization duration and exact bytes for max valid payload.
    let mut projection_ms = Vec::with_capacity(MEASURED_TRIALS);
    let mut payload_bytes = 0usize;
    let selection = ConsentSelection {
        basic: true,
        camera_compatibility: true,
        feature_adoption: true,
        consented_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
    };
    let sample_uuid = uuid::Uuid::new_v4().to_string();
    for trial in 0..(WARMUP_TRIALS + MEASURED_TRIALS) {
        let started = Instant::now();
        let snapshot = ProjectionSnapshot {
            config: &config,
            health: &health,
            cameras: &cameras,
            camera_database: None,
            estimated_retention_days: Some(30.0),
            is_container: false,
            video_accelerations: &["vaapi"],
            has_notifications_pushover: true,
            has_notifications_webhook: true,
            has_external_api_events: true,
        };
        let report = project_report(&snapshot, selection, &sample_uuid, 1).unwrap();
        let json = report.to_bounded_json().unwrap();
        payload_bytes = json.len();
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        if trial >= WARMUP_TRIALS {
            projection_ms.push(elapsed_ms);
        }
    }
    assert!(payload_bytes <= MAX_PAYLOAD_BYTES);

    let run_trials = |enabled: bool| -> (Vec<f64>, u64, u64) {
        let dir = bench_dir(if enabled { "enabled" } else { "baseline" });
        let engine = StorageEngine::start(storage_config(dir.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        let transport = Arc::new(CountingTransport(AtomicU64::new(0)));

        let worker = enabled.then(|| {
            let stop_flag = Arc::clone(&stop);
            let cfg_ref = Arc::clone(&config);
            let health_ref = Arc::clone(&health);
            let cams_ref = Arc::clone(&cameras);
            let transport_ref = Arc::clone(&transport);
            let worker_uuid = sample_uuid.clone();
            let state_path = dir.join("usage-state.json");
            std::thread::spawn(move || {
                let controller = UsageReportingController::open(
                    state_path,
                    Arc::new(FixedClock(1_790_000_000_000)),
                    Arc::new(ZeroJitter),
                    transport_ref,
                )
                .unwrap();
                let usage_cfg = UsageReportingConfig {
                    basic: true,
                    camera_compatibility: true,
                    feature_adoption: true,
                    consented_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
                    receiver_origin: Some("https://usage.example".to_owned()),
                };
                let _ = controller.apply_consent_transition(&usage_cfg);
                while !stop_flag.load(Ordering::Relaxed) {
                    let snapshot = ProjectionSnapshot {
                        config: &cfg_ref,
                        health: &health_ref,
                        cameras: &cams_ref,
                        camera_database: None,
                        estimated_retention_days: Some(30.0),
                        is_container: false,
                        video_accelerations: &["vaapi"],
                        has_notifications_pushover: true,
                        has_notifications_webhook: false,
                        has_external_api_events: true,
                    };
                    let _ = project_report(&snapshot, selection, &worker_uuid, 1)
                        .and_then(|report| report.to_bounded_json());
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        });

        let mut trial_us = Vec::with_capacity(MEASURED_TRIALS);
        let mut gaps = 0u64;
        for trial in 0..(WARMUP_TRIALS + MEASURED_TRIALS) {
            let started = Instant::now();
            let mut previous = started;
            for frame in &frames {
                let now = Instant::now();
                if now.duration_since(previous) > Duration::from_millis(250) {
                    gaps += 1;
                }
                previous = now;
                engine.ingest(CAMERA_ID, to_recording_frame(frame));
            }
            let elapsed_us = started.elapsed().as_secs_f64() * 1_000_000.0 / frames.len() as f64;
            if trial >= WARMUP_TRIALS {
                trial_us.push(elapsed_us);
            }
        }
        stop.store(true, Ordering::Relaxed);
        if let Some(handle) = worker {
            handle.join().expect("usage benchmark worker join");
        }
        engine.shutdown();
        let _ = std::fs::remove_dir_all(dir);
        (trial_us, gaps, transport.0.load(Ordering::Acquire))
    };

    let (baseline, baseline_gaps, baseline_requests) = run_trials(false);
    let (enabled, enabled_gaps, _) = run_trials(true);

    let baseline_p50 = percentile(&baseline, 0.50);
    let baseline_p95 = percentile(&baseline, 0.95);
    let enabled_p50 = percentile(&enabled, 0.50);
    let enabled_p95 = percentile(&enabled, 0.95);
    let delta_p50_pct = ((enabled_p50 - baseline_p50) / baseline_p50.max(0.001)) * 100.0;
    let delta_p95_pct = ((enabled_p95 - baseline_p95) / baseline_p95.max(0.001)) * 100.0;

    let report = json!({
        "runs": MEASURED_TRIALS,
        "warmups": WARMUP_TRIALS,
        "frames_per_trial": FRAMES_PER_TRIAL,
        "max_camera_groups": MAX_CAMERA_GROUPS,
        "max_payload_bytes": payload_bytes,
        "payload_budget_bytes": MAX_PAYLOAD_BYTES,
        "projection_p50_ms": (percentile(&projection_ms, 0.50) * 1000.0).round() / 1000.0,
        "projection_p95_ms": (percentile(&projection_ms, 0.95) * 1000.0).round() / 1000.0,
        "unit": "us_per_frame",
        "baseline_p50_us": (baseline_p50 * 100.0).round() / 100.0,
        "enabled_p50_us": (enabled_p50 * 100.0).round() / 100.0,
        "delta_p50_pct": (delta_p50_pct * 100.0).round() / 100.0,
        "baseline_p95_us": (baseline_p95 * 100.0).round() / 100.0,
        "enabled_p95_us": (enabled_p95 * 100.0).round() / 100.0,
        "delta_p95_pct": (delta_p95_pct * 100.0).round() / 100.0,
        "budget_max_regression_pct": 1.0,
        "baseline_gaps": baseline_gaps,
        "enabled_gaps": enabled_gaps,
        "baseline_requests": baseline_requests,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert_eq!(baseline_requests, 0);
    assert_eq!(enabled_gaps, 0);
}
