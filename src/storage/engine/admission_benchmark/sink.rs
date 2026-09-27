use super::*;
use crate::storage::catalog::RecordingCatalog;
use anyhow::{Context as _, Result};
use std::thread;

#[derive(Debug, Serialize)]
pub struct SinkReport {
    pub cameras: usize,
    pub event_to_first_committed_frame_ms: f64,
    pub flush_ms: f64,
    pub committed_media_bytes: u64,
    pub flush_mib_per_second: f64,
    pub media_files: usize,
    pub producer_wait_ms: f64,
    pub final_pending_channel_bytes: usize,
}

pub fn measure_sink(
    frames: &[FixtureFrame],
    cameras: usize,
    directory: &Path,
) -> Result<SinkReport> {
    anyhow::ensure!(!frames.is_empty() && (1..=127).contains(&cameras));
    anyhow::ensure!(!directory.exists(), "benchmark output must be new");
    std::fs::create_dir_all(directory)?;
    let config = sink_config(directory);
    let catalog = RecordingCatalog::open(&directory.join("catalog.sqlite"))?;
    let catalog_handle = catalog.handle();
    let engine = StorageEngine::start_with_catalog(config, catalog_handle.clone());
    let handle = engine.handle();
    let health = engine.health();
    let sources = (0..cameras)
        .map(|camera| format!("camera-{camera:03}"))
        .collect::<Vec<_>>();
    let (origin, split, mut producer_wait_ms) = configure_history(&handle, &sources, frames)?;
    let triggered = Instant::now();
    let commit_monitor = thread::spawn({
        let health = health.clone();
        let catalog = catalog_handle.clone();
        move || wait_for_commit(&health, &catalog, triggered)
    });
    for source in &sources {
        handle
            .admission
            .note_event(&handle.tx, source, origin + Duration::from_secs(6));
    }
    producer_wait_ms += feed_live(&handle, &sources, &frames[split..], origin)?;
    let first_commit_ms = commit_monitor
        .join()
        .map_err(|_| anyhow::anyhow!("commit observer panicked"))??;
    engine.shutdown();
    let flush_ms = triggered.elapsed().as_secs_f64() * 1000.0;
    anyhow::ensure!(
        health.benchmark_failure_count() == 0,
        "writer or queue failed: {:?}",
        health.benchmark_first_failure()
    );
    let (media_files, bytes) = verify_sources(directory, &sources, &catalog_handle)?;
    catalog.shutdown();
    let pending = handle.tx.queued_media_bytes.load(Ordering::Relaxed);
    anyhow::ensure!(pending == 0, "pending input remained after shutdown");
    let report = SinkReport {
        cameras,
        event_to_first_committed_frame_ms: first_commit_ms,
        flush_ms,
        committed_media_bytes: bytes,
        flush_mib_per_second: bytes as f64 / 1_048_576.0 / (flush_ms / 1000.0),
        media_files,
        producer_wait_ms,
        final_pending_channel_bytes: pending,
    };
    remove_output(directory)?;
    Ok(report)
}

pub fn output_root() -> PathBuf {
    std::env::var_os("KEEPPEEK_PREROLL_SINK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/event-preroll-sink")
        })
}

fn remove_output(directory: &Path) -> Result<()> {
    let allowed = output_root().canonicalize()?;
    let resolved = directory.canonicalize()?;
    anyhow::ensure!(
        resolved != allowed && resolved.starts_with(allowed),
        "benchmark cleanup escaped its output root"
    );
    std::fs::remove_dir_all(resolved)?;
    Ok(())
}

fn configure_history(
    handle: &StorageHandle,
    sources: &[String],
    frames: &[FixtureFrame],
) -> Result<(Instant, usize, f64)> {
    let origin = Instant::now() - Duration::from_secs(12);
    let split = frames.partition_point(|frame| frame.offset < Duration::from_secs(6));
    anyhow::ensure!(
        split > 0 && split < frames.len(),
        "fixture must straddle the event time"
    );
    let mut producer_wait_ms = 0.0;
    for source in sources {
        handle.configure_camera_event_recording(
            source,
            CameraRecordingMode::EventOnly,
            crate::cameras::EventRecordingStream::Main,
            Duration::from_secs(30),
            Duration::from_secs(60),
        );
        feed(handle, source, &frames[..split], origin);
        producer_wait_ms += drain_channel(handle)?;
    }
    Ok((origin, split, producer_wait_ms))
}

fn feed_live(
    handle: &StorageHandle,
    sources: &[String],
    frames: &[FixtureFrame],
    origin: Instant,
) -> Result<f64> {
    let mut producer_wait_ms = 0.0;
    for source in sources {
        feed(handle, source, frames, origin);
        producer_wait_ms += drain_channel(handle)?;
    }
    Ok(producer_wait_ms)
}

fn sink_config(directory: &Path) -> StorageConfig {
    let settings = crate::config::StorageToml {
        medium_term_path: Some(directory.to_string_lossy().into_owned()),
        long_term_path: Some(directory.to_string_lossy().into_owned()),
        short_term_secs: 0,
        medium_term_secs: 1800,
        flush_interval_secs: 0,
        minimum_free_gb: 0,
        warning_free_gb: 0,
        critical_free_gb: 0,
        cleanup_hysteresis_gb: 0,
        long_term_max_gb: 0,
        ..Default::default()
    };
    StorageConfig::from_toml(&settings)
}

fn feed(handle: &StorageHandle, source: &str, frames: &[FixtureFrame], start: Instant) {
    for fixture in frames {
        handle.ingest_stream(
            RecordingStreamIdentity::new(source, "main", source),
            fixture.recording_frame(start),
        );
    }
}

fn drain_channel(handle: &StorageHandle) -> Result<f64> {
    let started = Instant::now();
    while handle.tx.queued_media_bytes.load(Ordering::Relaxed) > 0 {
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(30),
            "writer did not drain its bounded queue"
        );
        thread::sleep(Duration::from_millis(1));
    }
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

fn wait_for_commit(
    health: &RecordingHealthRegistry,
    catalog: &RecordingCatalogHandle,
    triggered: Instant,
) -> Result<f64> {
    loop {
        anyhow::ensure!(
            health.benchmark_failure_count() == 0,
            "writer or queue failed before first commit: {:?}",
            health.benchmark_first_failure()
        );
        if catalog.stats()?.fragments > 0 {
            return Ok(triggered.elapsed().as_secs_f64() * 1000.0);
        }
        anyhow::ensure!(
            triggered.elapsed() < Duration::from_secs(30),
            "no frame committed within 30 seconds"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn verify_sources(
    directory: &Path,
    sources: &[String],
    catalog: &RecordingCatalogHandle,
) -> Result<(usize, u64)> {
    let stats = catalog.stats()?;
    anyhow::ensure!(
        stats.active_files == 0 && stats.finalized_files == stats.recording_files,
        "catalog contains unfinished recordings"
    );
    let mut files = 0;
    let mut bytes = 0;
    for source in sources {
        let identity = RecordingStreamIdentity::new(source, "main", source);
        let fragments = catalog.fragments_in_range(&identity.storage_key, 0, i64::MAX)?;
        anyhow::ensure!(
            !fragments.is_empty(),
            "camera has no committed catalog fragments: {source}"
        );
        let (source_files, source_bytes) = count_media(&directory.join(&identity.storage_key))?;
        anyhow::ensure!(
            source_files > 0 && source_bytes > 0,
            "camera has no final media: {source}"
        );
        files += source_files;
        bytes += source_bytes;
    }
    Ok((files, bytes))
}

fn count_media(directory: &Path) -> Result<(usize, u64)> {
    let mut pending = vec![directory.to_owned()];
    let mut count = 0;
    let mut bytes = 0;
    while let Some(directory) = pending.pop() {
        anyhow::ensure!(
            pending.len() < 1024 && count < 10_000,
            "unexpected benchmark output count"
        );
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "active")
            {
                anyhow::bail!("unfinished media remained after shutdown");
            } else if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "mp4")
            {
                let reader = mp4::read_mp4(std::fs::File::open(entry.path())?)?;
                anyhow::ensure!(
                    reader
                        .tracks()
                        .values()
                        .any(|track| track.sample_count() > 0),
                    "final media contains no samples"
                );
                count += 1;
                bytes += entry
                    .metadata()
                    .context("read committed media metadata")?
                    .len();
            }
        }
    }
    Ok((count, bytes))
}
