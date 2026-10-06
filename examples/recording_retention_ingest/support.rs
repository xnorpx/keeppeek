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
use serde::Serialize;
use serde_json::json;
use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};
use test_frame::{TestFrameConfig, TestFrameSource};

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

fn config(root: &Path, enabled: bool, existing: Option<&Path>) -> Result<StorageConfig> {
    let mut value = toml::Table::new();
    for (key, path) in [
        ("medium_term_path", root.to_path_buf()),
        ("long_term_path", root.to_path_buf()),
        (
            "recording_catalog_path",
            existing.map_or_else(|| root.join("catalog.db"), Path::to_path_buf),
        ),
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
        source.push_str(&format!("\n[retention.default]\ncontinuous_days=1.0\nmotion_days=7.0\n[retention.default.events]\nperson={}\n", if existing.is_some() { 31 } else { 30 }));
    }
    let settings: StorageToml = toml::from_str(&source)?;
    Ok(StorageConfig::from_toml(&settings))
}

fn sample(
    root: &Path,
    enabled: bool,
    frames: &[VideoFrame],
    existing: Option<&Path>,
) -> Result<Sample> {
    std::fs::create_dir(root)?;
    let config = config(root, enabled, existing)?;
    let catalog = RecordingCatalog::open(&config.recording_catalog_path)?;
    let catalog_path = config.recording_catalog_path.clone();
    let engine = StorageEngine::start_with_catalog(config, catalog.handle());
    let activated = await_active(&catalog_path, enabled)?;
    let before = catalog_stats(&catalog_path)?;
    let started = Instant::now();
    for second in 0..16_u64 {
        for (index, frame) in frames.iter().enumerate() {
            let timestamp =
                Duration::from_secs(second) + Duration::from_secs_f64(index as f64 / 15.0);
            engine.ingest(
                if existing.is_some() {
                    "cam-000/sub"
                } else {
                    "benchmark/sub"
                },
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
    let after = catalog_stats(&catalog_path)?;
    ensure!(
        after.recordings == before.recordings + 16,
        "ingest did not publish exactly sixteen records"
    );
    ensure!(
        after.historical == before.historical,
        "ingest changed historical metadata count"
    );
    catalog.shutdown();
    let bytes = verify_media(root)?;
    Ok(Sample {
        elapsed_us,
        bytes,
        activated,
        before,
        after,
    })
}

pub fn measure(root: &Path, enabled: bool, existing: Option<&Path>) -> Result<serde_json::Value> {
    std::fs::create_dir_all(root)?;
    let frames = frames()?;
    let mut samples = Histogram::<u64>::new(3)?;
    let mut raw = Vec::with_capacity(30);
    let mut bytes = Vec::with_capacity(35);
    let mut activations = Vec::with_capacity(35);
    let mut catalog_states = Vec::with_capacity(35);
    for round in 0..35 {
        let sample = sample(
            &root.join(format!("sample-{round}")),
            enabled,
            &frames,
            existing,
        )?;
        bytes.push(sample.bytes);
        activations.push(sample.activated);
        catalog_states.push(json!({"before":sample.before,"after":sample.after}));
        if round >= 5 {
            samples.record(sample.elapsed_us.max(1))?;
            raw.push(sample.elapsed_us);
        }
        eprintln!("completed_sample={round} enabled={enabled}");
    }
    let report = json!({"existing_archive":existing.is_some(),"retention_requested":enabled,"retention_activated":activations,"warmup_runs":5,"runs":30,
        "frames_per_run":240,"codec":"H264","dimensions":[320,240],"logical_duration_seconds":16,
        "median_us":samples.value_at_quantile(0.5),"p95_us":samples.value_at_quantile(0.95),
        "max_us":samples.max(),"raw_us":raw,"written_bytes":bytes,"catalog_states":catalog_states,
        "scope":"accelerated ingest plus engine shutdown flush; fresh or caller-owned existing catalog as reported; frame generation, catalog startup, verified runtime activation and sample verification excluded; no live pacing; existing archive mode retains its historical metadata and reuses its catalog authority"});
    std::fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    eprintln!("artifact_directory={}", root.display());
    Ok(report)
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

#[derive(Serialize)]
struct Stats {
    recordings: u64,
    historical: u64,
    pending: Option<u64>,
}

struct Sample {
    elapsed_us: u64,
    bytes: u64,
    activated: Option<bool>,
    before: Stats,
    after: Stats,
}

fn catalog_stats(path: &Path) -> Result<Stats> {
    let database = pollster::block_on(
        turso::Builder::new_local(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 catalog path"))?,
        )
        .build(),
    )?;
    let connection = database.connect()?;
    let mut rows = pollster::block_on(connection.query(
        "SELECT count(*),sum(CASE WHEN id GLOB 'cam-*' THEN 1 ELSE 0 END) FROM recording_files",
        (),
    ))?;
    let row = pollster::block_on(rows.next())?
        .ok_or_else(|| anyhow::anyhow!("recording counts missing"))?;
    let recordings = u64::try_from(row.get::<i64>(0)?)?;
    let historical = u64::try_from(row.get::<Option<i64>>(1)?.unwrap_or(0))?;
    drop(rows);
    let mut rows = pollster::block_on(connection.query(
        "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='recording_retention_runtime'",
        (),
    ))?;
    let runtime = pollster::block_on(rows.next())?.is_some();
    drop(rows);
    let pending = if runtime {
        let mut rows = pollster::block_on(connection.query(
            "SELECT count(*) FROM recording_files INDEXED BY recording_retention_pending_files WHERE retention_pending=1", ()))?;
        let row = pollster::block_on(rows.next())?
            .ok_or_else(|| anyhow::anyhow!("pending count missing"))?;
        Some(u64::try_from(row.get::<i64>(0)?)?)
    } else {
        None
    };
    Ok(Stats {
        recordings,
        historical,
        pending,
    })
}

fn verify_media(root: &Path) -> Result<u64> {
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
    Ok(bytes)
}
