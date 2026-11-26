//! Release-build AC-11 ingest and recording isolation benchmark for bug reporting.
//!
//! Run with:
//! `cargo test --release --test reporting_ingest_benchmark -- --ignored --nocapture`

use keeppeek::{
    logging::{LogHub, LogLevel},
    storage::{
        MediaFrame, RecordingFrame, StorageConfig, StorageEngine, VideoCodec, VideoFrame,
        nal::annexb_to_avcc,
    },
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use test_frame::{EncodedFrame, TestFrameConfig, TestFrameSource};

const WARMUP_TRIALS: usize = 3;
const MEASURED_TRIALS: usize = 30;
const FRAMES_PER_TRIAL: usize = 30;
const CAMERA_ID: &str = "bench-cam-1";

fn bench_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-reporting-bench-{name}-{}",
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

fn spawn_reporting_worker(hub: LogHub, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut iteration = 0u64;
        while !stop.load(Ordering::Relaxed) {
            hub.record(
                LogLevel::Info,
                "keeppeek::reporting",
                format!("report tick {iteration}"),
                BTreeMap::new(),
                None,
                None,
            );
            if iteration.is_multiple_of(50) {
                let snapshot = hub.snapshot(None, 10_000);
                assert!(snapshot.generation.is_some());
            }
            iteration = iteration.saturating_add(1);
            std::thread::sleep(Duration::from_millis(2));
        }
    })
}

fn run_ingest_trials(frames: &[EncodedFrame], active_reporting: bool) -> (Vec<f64>, u64) {
    let dir = bench_dir(if active_reporting {
        "active"
    } else {
        "baseline"
    });
    let engine = StorageEngine::start(storage_config(dir.clone()));
    let hub = LogHub::default();
    for sequence in 0..2_000usize {
        hub.record(
            LogLevel::Info,
            "keeppeek::bench",
            format!("retained log entry {sequence}"),
            BTreeMap::new(),
            None,
            None,
        );
    }

    let stop = Arc::new(AtomicBool::new(false));
    let worker = active_reporting.then(|| spawn_reporting_worker(hub.clone(), Arc::clone(&stop)));

    let mut trial_us = Vec::with_capacity(MEASURED_TRIALS);
    let mut gaps = 0u64;
    for trial in 0..(WARMUP_TRIALS + MEASURED_TRIALS) {
        let started = Instant::now();
        let mut previous = started;
        for frame in frames {
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
        handle.join().expect("reporting worker join");
    }
    engine.shutdown();
    let _ = std::fs::remove_dir_all(dir);
    (trial_us, gaps)
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
#[ignore = "Release-build AC-11 ingest isolation benchmark"]
fn reporting_ingest_isolation_30_trials() {
    let frames = generate_encoded_frames(FRAMES_PER_TRIAL);
    let (baseline, baseline_gaps) = run_ingest_trials(&frames, false);
    let (active, active_gaps) = run_ingest_trials(&frames, true);

    let baseline_p50 = percentile(&baseline, 0.50);
    let baseline_p95 = percentile(&baseline, 0.95);
    let active_p50 = percentile(&active, 0.50);
    let active_p95 = percentile(&active, 0.95);
    let delta_p50_pct = ((active_p50 - baseline_p50) / baseline_p50.max(0.001)) * 100.0;
    let delta_p95_pct = ((active_p95 - baseline_p95) / baseline_p95.max(0.001)) * 100.0;

    let report = json!({
        "runs": MEASURED_TRIALS,
        "warmups": WARMUP_TRIALS,
        "frames_per_trial": FRAMES_PER_TRIAL,
        "unit": "us_per_frame",
        "baseline_p50_us": (baseline_p50 * 100.0).round() / 100.0,
        "active_p50_us": (active_p50 * 100.0).round() / 100.0,
        "delta_p50_pct": (delta_p50_pct * 100.0).round() / 100.0,
        "baseline_p95_us": (baseline_p95 * 100.0).round() / 100.0,
        "active_p95_us": (active_p95 * 100.0).round() / 100.0,
        "delta_p95_pct": (delta_p95_pct * 100.0).round() / 100.0,
        "baseline_gaps": baseline_gaps,
        "active_gaps": active_gaps,
        "budget_max_regression_pct": 5.0,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert_eq!(active_gaps, 0, "reporting must not induce recording gaps");
}
