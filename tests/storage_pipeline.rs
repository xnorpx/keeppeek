use keeppeek::storage::{
    MediaFrame, RecordingFrame, StorageConfig, StorageEngine, VideoCodec, VideoFrame,
    nal::annexb_to_avcc,
};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};
use test_frame::{EncodedFrame, TestFrameConfig, TestFrameSource};

const CAMERA_ID: &str = "test-cam-1";
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const FPS: u32 = 30;
const DURATION_SECS: u64 = 20;

fn test_output_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-output")
        .join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("failed to clean test output dir");
    }
    std::fs::create_dir_all(&dir).expect("failed to create test output dir");
    dir
}

fn to_recording_frame(ef: EncodedFrame) -> RecordingFrame {
    let avcc = annexb_to_avcc(&ef.data);
    RecordingFrame {
        received_at: Instant::now(),
        camera_dts_90k: None,
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: ef.is_keyframe,
            width: ef.width,
            height: ef.height,
            data: avcc.into(),
        }),
    }
}

#[test]
fn three_tier_storage_pipeline() {
    let out = test_output_dir("pipeline");

    let config = StorageConfig {
        medium_term_path: out.clone(),
        long_term_path: out.clone(),
        recording_catalog_path: out.join("recordings.db"),
        event_thumbnail_path: out.join(".event-thumbnails"),
        event_thumbnail_max_bytes: 1_024 * 1_048_576,
        short_term_duration: Duration::from_secs(2),
        medium_term_duration: Duration::from_secs(5),
        flush_interval: Duration::from_secs(1),
        write_buffer_bytes: 8 * 1024,
        long_term_max_bytes: 0,
    };
    let engine = StorageEngine::start(config);

    let (tx, rx) = mpsc::sync_channel::<EncodedFrame>(128);

    let source = TestFrameSource::start(
        TestFrameConfig {
            width: WIDTH,
            height: HEIGHT,
            fps: FPS,
            codec: test_frame::Codec::H264,
            keyframe_interval: FPS,
            bitrate_bps: 500_000,
        },
        move |frame| {
            let _ = tx.send(frame);
        },
    )
    .expect("failed to start test frame source");

    let deadline = Instant::now() + Duration::from_secs(DURATION_SECS);
    let mut total_ingested = 0u64;

    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ef) => {
                let rf = to_recording_frame(ef);
                engine.ingest(CAMERA_ID, rf);
                total_ingested += 1;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    source.stop();
    engine.shutdown();

    let mp4s = collect_mp4s(&out);
    println!("total frames ingested: {total_ingested}");
    println!("mp4 files found: {}", mp4s.len());
    for p in &mp4s {
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        println!("  {} ({size} bytes)", p.display());
    }

    assert!(
        total_ingested >= FPS as u64 * 10,
        "expected at least {} frames, got {total_ingested}",
        FPS as u64 * 10,
    );

    assert!(
        mp4s.len() >= 3,
        "expected at least 3 mp4 files but found {}",
        mp4s.len(),
    );

    for p in &mp4s {
        let size = std::fs::metadata(p).unwrap().len();
        assert!(
            size > 100,
            "mp4 {} is suspiciously small ({size} bytes)",
            p.display()
        );
        let reader = mp4::read_mp4(std::fs::File::open(p).unwrap()).unwrap();
        assert!(
            reader.is_fragmented(),
            "{} is not fragmented MP4",
            p.display()
        );
        assert_eq!(reader.tracks().len(), 1);
        assert_eq!(
            reader.tracks()[&1].track_type().unwrap(),
            mp4::TrackType::Video
        );
        assert!(reader.sample_count(1).unwrap() > 0);
    }
}

#[test]
fn segment_moves_from_medium_to_long_term() {
    let base = test_output_dir("move");

    let medium = base.join("medium");
    let long = base.join("long");
    std::fs::create_dir_all(&medium).unwrap();
    std::fs::create_dir_all(&long).unwrap();

    let config = StorageConfig {
        medium_term_path: medium.clone(),
        long_term_path: long.clone(),
        recording_catalog_path: long.join("recordings.db"),
        event_thumbnail_path: long.join(".event-thumbnails"),
        event_thumbnail_max_bytes: 1_024 * 1_048_576,
        short_term_duration: Duration::from_secs(2),
        medium_term_duration: Duration::from_secs(5),
        flush_interval: Duration::from_secs(1),
        write_buffer_bytes: 8 * 1024,
        long_term_max_bytes: 0,
    };
    let engine = StorageEngine::start(config);

    let (tx, rx) = mpsc::sync_channel::<EncodedFrame>(128);

    let source = TestFrameSource::start(
        TestFrameConfig {
            width: WIDTH,
            height: HEIGHT,
            fps: FPS,
            codec: test_frame::Codec::H264,
            keyframe_interval: FPS,
            bitrate_bps: 500_000,
        },
        move |frame| {
            let _ = tx.send(frame);
        },
    )
    .expect("start");

    let deadline = Instant::now() + Duration::from_secs(DURATION_SECS);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ef) => {
                engine.ingest(CAMERA_ID, to_recording_frame(ef));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    source.stop();
    engine.shutdown();

    let medium_mp4s = collect_mp4s(&medium);
    let long_mp4s = collect_mp4s(&long);

    println!("medium-term mp4s: {}", medium_mp4s.len());
    for p in &medium_mp4s {
        println!("  medium: {}", p.display());
    }
    println!("long-term mp4s: {}", long_mp4s.len());
    for p in &long_mp4s {
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        println!("  long: {} ({size} bytes)", p.display());
    }

    assert!(
        long_mp4s.len() >= 2,
        "expected at least 2 rotated segments in long-term, got {}",
        long_mp4s.len(),
    );

    assert!(
        medium_mp4s.is_empty()
            || (medium_mp4s.len() == 1
                && medium_mp4s[0].extension().and_then(|e| e.to_str()) == Some("mp4")),
        "rotated segments should not remain in medium-term dir; found {medium_mp4s:?}",
    );

    for p in &long_mp4s {
        let size = std::fs::metadata(p).unwrap().len();
        assert!(size > 100, "mp4 {} too small ({size} bytes)", p.display());
    }
}

#[test]
fn same_path_no_extra_copy() {
    let base = test_output_dir("same-path");

    let config = StorageConfig {
        medium_term_path: base.clone(),
        long_term_path: base.clone(),
        recording_catalog_path: base.join("recordings.db"),
        event_thumbnail_path: base.join(".event-thumbnails"),
        event_thumbnail_max_bytes: 1_024 * 1_048_576,
        short_term_duration: Duration::from_secs(2),
        medium_term_duration: Duration::from_secs(5),
        flush_interval: Duration::from_secs(1),
        write_buffer_bytes: 8 * 1024,
        long_term_max_bytes: 0,
    };
    let engine = StorageEngine::start(config);

    let (tx, rx) = mpsc::sync_channel::<EncodedFrame>(128);

    let source = TestFrameSource::start(
        TestFrameConfig {
            width: WIDTH,
            height: HEIGHT,
            fps: FPS,
            codec: test_frame::Codec::H264,
            keyframe_interval: FPS,
            bitrate_bps: 500_000,
        },
        move |frame| {
            let _ = tx.send(frame);
        },
    )
    .expect("start");

    let deadline = Instant::now() + Duration::from_secs(DURATION_SECS);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ef) => {
                engine.ingest(CAMERA_ID, to_recording_frame(ef));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    source.stop();
    engine.shutdown();

    let mp4s = collect_mp4s(&base);
    println!("same-path mp4s: {}", mp4s.len());
    for p in &mp4s {
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        println!("  {} ({size} bytes)", p.display());
    }

    assert!(
        mp4s.len() >= 3,
        "expected at least 3 mp4 files, got {}",
        mp4s.len(),
    );

    for p in &mp4s {
        let size = std::fs::metadata(p).unwrap().len();
        assert!(size > 100, "mp4 {} too small ({size} bytes)", p.display());
    }
}

#[test]
fn long_term_retention_limit() {
    let base = test_output_dir("retention");

    let max_bytes: u64 = 150_000;

    let config = StorageConfig {
        medium_term_path: base.clone(),
        long_term_path: base.clone(),
        recording_catalog_path: base.join("recordings.db"),
        event_thumbnail_path: base.join(".event-thumbnails"),
        event_thumbnail_max_bytes: 1_024 * 1_048_576,
        short_term_duration: Duration::ZERO,
        medium_term_duration: Duration::from_secs(2),
        flush_interval: Duration::ZERO,
        write_buffer_bytes: 8 * 1024,
        long_term_max_bytes: max_bytes,
    };
    let engine = StorageEngine::start(config);

    let (tx, rx) = mpsc::sync_channel::<EncodedFrame>(128);

    let source = TestFrameSource::start(
        TestFrameConfig {
            width: WIDTH,
            height: HEIGHT,
            fps: FPS,
            codec: test_frame::Codec::H264,
            keyframe_interval: FPS,
            bitrate_bps: 500_000,
        },
        move |frame| {
            let _ = tx.send(frame);
        },
    )
    .expect("start");

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ef) => {
                engine.ingest(CAMERA_ID, to_recording_frame(ef));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    source.stop();
    engine.shutdown();

    let before_mp4s = collect_mp4s(&base);
    let before_bytes: u64 = before_mp4s
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    println!(
        "before enforce_limit: {} files, {before_bytes} bytes",
        before_mp4s.len()
    );

    assert!(
        before_mp4s.len() >= 2,
        "expected at least 2 segments before pruning, got {}",
        before_mp4s.len(),
    );

    let store = keeppeek::storage::long_term::LongTermStore::new(base.clone());
    store.enforce_limit(max_bytes);

    let after_mp4s = collect_mp4s(&base);
    let after_bytes: u64 = after_mp4s
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    println!(
        "after enforce_limit: {} files, {after_bytes} bytes (limit {max_bytes})",
        after_mp4s.len()
    );
    for p in &after_mp4s {
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        println!("  {} ({size} bytes)", p.display());
    }

    assert!(
        after_bytes <= max_bytes,
        "total bytes after enforce_limit ({after_bytes}) should be <= limit ({max_bytes})",
    );

    assert!(
        after_mp4s.len() < before_mp4s.len(),
        "enforce_limit should have removed some segments ({} before, {} after)",
        before_mp4s.len(),
        after_mp4s.len(),
    );
}

fn collect_mp4s(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if !dir.exists() {
        return result;
    }
    if dir.is_file() {
        if dir.extension().and_then(|e| e.to_str()) == Some("mp4") {
            result.push(dir.to_path_buf());
        }
        return result;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                result.extend(collect_mp4s(&path));
            } else if path.extension().and_then(|e| e.to_str()) == Some("mp4") {
                result.push(path);
            }
        }
    }
    result
}
