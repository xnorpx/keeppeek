//! Measures real H.264 ingest and flush into a fresh catalog, separately from archive sweeps.
use anyhow::{Result, ensure};
use hdrhistogram::Histogram;
use keeppeek::{
    config::StorageToml,
    storage::{
        MediaFrame, RecordingCatalog, RecordingFrame, StorageConfig, StorageEngine, VideoCodec,
        VideoFrame, nal::annexb_to_avcc,
    },
};
use serde_json::json;
use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};
use test_frame::{TestFrameConfig, TestFrameSource};

fn main() -> Result<()> {
    let enabled = match std::env::args().nth(1).as_deref() {
        Some("enabled") => true,
        Some("disabled") => false,
        _ => anyhow::bail!("use enabled or disabled"),
    };
    let root = std::env::temp_dir().join(format!("retention-ingest-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    std::thread::Builder::new()
        .name("retention-ingest".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || measure(&root, enabled))?
        .join()
        .map_err(|_| anyhow::anyhow!("ingest measurement panicked"))?
}

fn frames() -> Result<Vec<VideoFrame>> {
    let (sent, received) = mpsc::sync_channel(32);
    let source = TestFrameSource::start(
        TestFrameConfig {
            width: 320,
            height: 240,
            fps: 15,
            codec: test_frame::Codec::H264,
            keyframe_interval: 15,
            bitrate_bps: 500_000,
        },
        move |frame| {
            let _ = sent.send(frame);
        },
    )?;
    let mut frames = Vec::with_capacity(15);
    for _ in 0..15 {
        let frame = received.recv_timeout(Duration::from_secs(5))?;
        frames.push(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: frame.is_keyframe,
            width: frame.width,
            height: frame.height,
            data: annexb_to_avcc(&frame.data).into(),
        });
    }
    source.stop();
    ensure!(
        frames[0].is_keyframe,
        "generated GOP has no initial keyframe"
    );
    Ok(frames)
}

fn config(root: &Path, enabled: bool) -> Result<StorageConfig> {
    let mut value = toml::Table::new();
    for (key, path) in [
        ("medium_term_path", root.to_path_buf()),
        ("long_term_path", root.to_path_buf()),
        ("recording_catalog_path", root.join("catalog.db")),
        ("event_thumbnail_path", root.join("images")),
    ] {
        value.insert(
            key.into(),
            toml::Value::String(path.to_string_lossy().into_owned()),
        );
    }
    for key in [
        "short_term_secs",
        "medium_term_secs",
        "flush_interval_secs",
        "long_term_max_gb",
    ] {
        value.insert(key.into(), toml::Value::Integer(0));
    }
    let mut source = toml::to_string(&value)?;
    if enabled {
        source.push_str("\n[retention.default]\ncontinuous_days=1.0\nmotion_days=7.0\n[retention.default.events]\nperson=30.0\n");
    }
    let settings: StorageToml = toml::from_str(&source)?;
    Ok(StorageConfig::from_toml(&settings))
}

fn sample(root: &Path, enabled: bool, frames: &[VideoFrame]) -> Result<(u64, u64, Option<bool>)> {
    std::fs::create_dir(root)?;
    let config = config(root, enabled)?;
    let catalog = RecordingCatalog::open(&config.recording_catalog_path)?;
    let catalog_path = config.recording_catalog_path.clone();
    let engine = StorageEngine::start_with_catalog(config, catalog.handle());
    let activated = await_active(&catalog_path, enabled)?;
    let started = Instant::now();
    for second in 0..16_u64 {
        for (index, frame) in frames.iter().enumerate() {
            let timestamp =
                Duration::from_secs(second) + Duration::from_secs_f64(index as f64 / 15.0);
            engine.ingest(
                "benchmark/sub",
                RecordingFrame {
                    received_at: started + timestamp,
                    timestamp: Some(timestamp),
                    frame: MediaFrame::Video(VideoFrame {
                        codec: frame.codec,
                        is_keyframe: frame.is_keyframe,
                        width: frame.width,
                        height: frame.height,
                        data: frame.data.clone(),
                    }),
                },
            );
        }
    }
    engine.shutdown();
    let elapsed_us = u64::try_from(started.elapsed().as_micros())?;
    catalog.shutdown();
    let mut count = 0_u64;
    let mut bytes = 0_u64;
    for path in media_paths(root)? {
        let reader = mp4::read_mp4(std::fs::File::open(&path)?)?;
        for track in reader.tracks().values() {
            count += u64::from(reader.sample_count(track.track_id())?);
        }
        bytes += std::fs::metadata(path)?.len();
    }
    ensure!(
        count == 240,
        "ingest lost frames: expected240 observed{count}"
    );
    Ok((elapsed_us, bytes, activated))
}

fn measure(root: &Path, enabled: bool) -> Result<()> {
    let frames = frames()?;
    let mut samples = Histogram::<u64>::new(3)?;
    let mut raw = Vec::with_capacity(30);
    let mut bytes = Vec::with_capacity(35);
    let mut activations = Vec::with_capacity(35);
    for round in 0..35 {
        let (elapsed, written, activated) =
            sample(&root.join(format!("sample-{round}")), enabled, &frames)?;
        bytes.push(written);
        activations.push(activated);
        if round >= 5 {
            samples.record(elapsed.max(1))?;
            raw.push(elapsed);
        }
        eprintln!("completed_sample={round} enabled={enabled}");
    }
    let report = json!({"retention_requested":enabled,"retention_activated":activations,"warmup_runs":5,"runs":30,
        "frames_per_run":240,"codec":"H264","dimensions":[320,240],"logical_duration_seconds":16,
        "median_us":samples.value_at_quantile(0.5),"p95_us":samples.value_at_quantile(0.95),
        "max_us":samples.max(),"raw_us":raw,"written_bytes":bytes,
        "scope":"fresh catalog; accelerated ingest plus engine shutdown flush; frame generation, catalog startup, verified runtime activation and sample verification excluded; no archive contention or live pacing"});
    std::fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    eprintln!("artifact_directory={}", root.display());
    Ok(())
}

fn media_paths(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut directories = vec![root.to_path_buf()];
    let mut paths = Vec::with_capacity(16);
    let mut visited = 0;
    while let Some(directory) = directories.pop() {
        visited += 1;
        ensure!(visited <= 128, "fixture directory limit exceeded");
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                ensure!(
                    visited + directories.len() < 128,
                    "fixture pending directory limit exceeded"
                );
                directories.push(entry.path());
            } else if kind.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "mp4")
            {
                paths.push(entry.path());
                ensure!(paths.len() <= 32, "fixture file limit exceeded");
            }
        }
    }
    Ok(paths)
}

fn await_active(path: &Path, enabled: bool) -> Result<Option<bool>> {
    let database = pollster::block_on(
        turso::Builder::new_local(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 catalog path"))?,
        )
        .build(),
    )?;
    let connection = database.connect()?;
    let mut rows = pollster::block_on(connection.query(
        "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='recording_retention_runtime'",
        (),
    ))?;
    if pollster::block_on(rows.next())?.is_none() {
        return Ok(None);
    }
    drop(rows);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut rows = pollster::block_on(connection.query(
            "SELECT settings_json IS NOT NULL,request_pending,complete FROM recording_retention_runtime WHERE singleton=1", ()))?;
        let row = pollster::block_on(rows.next())?
            .ok_or_else(|| anyhow::anyhow!("runtime state missing"))?;
        let active = row.get::<i64>(0)? != 0;
        if active == enabled && row.get::<i64>(1)? == 0 && row.get::<i64>(2)? == 1 {
            return Ok(Some(active));
        }
        drop(rows);
        ensure!(
            Instant::now() < deadline,
            "retention activation exceeded fixture deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
