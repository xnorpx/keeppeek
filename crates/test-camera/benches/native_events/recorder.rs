use super::Condition;
use anyhow::{Context as _, ensure};
use keeppeek::{
    cameras::{CameraRecordingMode, configured_cameras, events::EventMode},
    config::load_cameras,
    keeppeek::KeepPeekLoop,
    shutdown::Shutdown,
    stats::HealthRegistry,
    storage::{
        RecordingCatalog, StorageConfig, StorageEngine,
        events::EventStore,
        metadata::{EventSource, TimelineEvent},
    },
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use test_camera::{TestCamera, TestCameraBuilder};
use test_hikvision::onvif::notification;

const DOCUMENT_COUNT: usize = 30;
const OBSERVATION_WINDOW: Duration = Duration::from_millis(3150);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const STREAMS: [&str; 2] = ["main", "sub"];

pub fn camera() -> anyhow::Result<TestCamera> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/cc-4k-640x360-h264.mp4");
    let documents = documents();
    ensure!(documents.len() == DOCUMENT_COUNT, "wrong document count");
    ensure!(
        documents.iter().all(|document| document.len() <= 4096),
        "document exceeds 4 KiB"
    );
    TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", documents)
        .realtime_start_at(Duration::ZERO)
        .start()
}

pub fn measure(root: &Path, condition: Condition) -> anyhow::Result<Value> {
    let mut recorder = Recorder::start(root, condition)?;
    let observation = recorder.observe()?;
    let events = recorder.check_events(condition)?;
    let finalizing = Instant::now();
    recorder.stop()?;
    let finalize_ms = finalizing.elapsed().as_secs_f64() * 1000.0;
    let recordings = recorder.check_recordings()?;
    ensure!(
        recorder.events()? == events,
        "shutdown changed persisted events"
    );
    Ok(json!({
        "condition": condition.label(),
        "camera_id": recorder.camera_id,
        "first_both_fragment_ms": observation.first.iter().max().context("two streams")?.as_secs_f64() * 1000.0,
        "max_fragment_gap_ms": observation.gaps.iter().max().context("two streams")?.as_secs_f64() * 1000.0,
        "first_fragment_ms": observation.first.map(|duration| duration.as_secs_f64() * 1000.0),
        "fragment_gap_ms": observation.gaps.map(|duration| duration.as_secs_f64() * 1000.0),
        "observed_fragments": observation.fragments,
        "shutdown_finalize_ms": finalize_ms,
        "event_count": events.len(),
        "documents_per_stream": DOCUMENT_COUNT,
        "metadata_bytes_per_stream": recorder.metadata_bytes,
        "observation_window_ms": OBSERVATION_WINDOW.as_millis(),
        "recordings": recordings,
    }))
}

struct Observation {
    first: [Duration; 2],
    gaps: [Duration; 2],
    fragments: [usize; 2],
}

struct Recorder {
    shutdown: Shutdown,
    worker: Option<JoinHandle<()>>,
    storage: Option<StorageEngine>,
    catalog: RecordingCatalog,
    store: EventStore,
    camera_id: String,
    started: Instant,
    metadata_bytes: usize,
}

impl Recorder {
    fn start(root: &Path, condition: Condition) -> anyhow::Result<Self> {
        let config_path = root.join("config.toml");
        let mut configs = load_cameras(&config_path)?;
        let mut entries = configs
            .remove("test-camera")
            .context("fixture camera group")?;
        ensure!(
            configs.is_empty() && entries.len() == 1,
            "exactly one camera required"
        );
        let mut config = entries.pop().context("fixture camera config")?;
        ensure!(config.ip.is_loopback(), "benchmark camera must be loopback");
        ensure!(
            config.ip == std::env::var(super::CHILD_IP)?.parse::<std::net::Ipv4Addr>()?,
            "camera identity mismatch"
        );
        config.events.mode = match condition {
            Condition::Disabled => EventMode::Disabled,
            Condition::Enabled => EventMode::RtspMetadata,
        };
        config.events.snapshots = false;
        config.events.source_tokens = vec!["source-2".to_owned()];
        config.record_generic_motion_events = true;
        config.recording_mode = CameraRecordingMode::Both;
        let camera_ip = config.ip;
        let configured =
            configured_cameras(&HashMap::from([("test-camera".to_owned(), vec![config])]))
                .remove(&camera_ip)
                .context("configured fixture camera")?;
        let catalog = RecordingCatalog::open(&root.join("recordings.db"))?;
        let store = EventStore::new(catalog.handle(), &root.join("thumbnails"), 1024 * 1024)?;
        let storage = StorageEngine::start_with_catalog(storage_config(root), catalog.handle());
        let shutdown = Shutdown::new();
        let mut recorder = KeepPeekLoop::new(shutdown.clone(), Some(storage.handle()));
        recorder.set_event_store(store.clone());
        recorder.set_health_registry(HealthRegistry::new());
        let started = Instant::now();
        recorder.add_camera(&configured, true, true)?;
        let worker = thread::Builder::new()
            .name("native-perf-recorder".to_owned())
            .spawn(move || recorder.run())?;
        Ok(Self {
            shutdown,
            worker: Some(worker),
            storage: Some(storage),
            catalog,
            store,
            camera_id: camera_ip.to_string(),
            started,
            metadata_bytes: documents().iter().map(Vec::len).sum(),
        })
    }

    fn observe(&self) -> anyhow::Result<Observation> {
        let mut first = [None; 2];
        let mut previous_time = [Duration::ZERO; 2];
        let mut previous_start = [0; 2];
        let mut gaps = [Duration::ZERO; 2];
        let mut counts = [0; 2];
        let deadline = self.started + OBSERVATION_WINDOW;
        for _ in 0..400 {
            if Instant::now() >= deadline {
                break;
            }
            for (index, stream) in STREAMS.iter().enumerate() {
                let fragments = self.catalog.handle().fragments_in_range(
                    &format!("native-perf/{stream}"),
                    0,
                    i64::MAX,
                )?;
                ensure!(fragments.len() <= 8, "fragment observation bound exceeded");
                let Some(fragment) = fragments.last() else {
                    continue;
                };
                ensure!(
                    fragment.random_access && fragment.duration_ms > 0 && fragment.byte_len > 0,
                    "invalid durable {stream} fragment"
                );
                if fragment.start_ms > previous_start[index] {
                    let observed = self.started.elapsed();
                    if first[index].is_some() {
                        gaps[index] = gaps[index].max(observed - previous_time[index]);
                    } else {
                        first[index] = Some(observed);
                    }
                    previous_start[index] = fragment.start_ms;
                    previous_time[index] = observed;
                    counts[index] += 1;
                }
            }
            thread::park_timeout(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        ensure!(
            Instant::now() >= deadline,
            "observation exhausted its poll bound"
        );
        ensure!(
            counts.iter().all(|count| *count >= 3),
            "both streams need three progressing fragments: {counts:?}"
        );
        Ok(Observation {
            first: [
                first[0].context("main never recorded")?,
                first[1].context("sub never recorded")?,
            ],
            gaps,
            fragments: counts,
        })
    }

    fn events(&self) -> anyhow::Result<Vec<TimelineEvent>> {
        self.store.events_in_range(&self.camera_id, 0, i64::MAX)
    }

    fn check_events(&self, condition: Condition) -> anyhow::Result<Vec<TimelineEvent>> {
        let events = self.events()?;
        let expected = match condition {
            Condition::Disabled => 0,
            Condition::Enabled => DOCUMENT_COUNT / 2,
        };
        ensure!(
            events.len() == expected,
            "{} expected {expected} events, got {}",
            condition.label(),
            events.len()
        );
        for event in &events {
            ensure!(
                event.source == EventSource::Camera && event.kind == "motion",
                "wrong native event source or kind"
            );
            ensure!(
                event.camera_id == self.camera_id,
                "wrong event camera identity"
            );
            ensure!(
                event.revision == 2,
                "each event must contain one start and one clear"
            );
            ensure!(
                event
                    .end_time_ms
                    .is_some_and(|end| end >= event.start_time_ms),
                "event did not clear before shutdown"
            );
            ensure!(
                event.attachments.is_empty() && event.thumbnail_filename.is_none(),
                "unexpected snapshot work"
            );
        }
        Ok(events)
    }

    fn check_recordings(&self) -> anyhow::Result<Vec<Value>> {
        let mut result = Vec::with_capacity(STREAMS.len());
        for stream in STREAMS {
            let fragments = self.catalog.handle().media_fragments_in_range(
                &format!("native-perf/{stream}"),
                0,
                i64::MAX,
            )?;
            ensure!(
                (3..=8).contains(&fragments.len()),
                "unexpected finalized {stream} fragment count"
            );
            let last = fragments.last().context("finalized media fragment")?;
            let file = fs::File::open(&last.path)?;
            let file_bytes = file.metadata()?.len();
            for fragment in &fragments {
                ensure!(fragment.path == last.path, "unexpected recording rollover");
                let end = fragment
                    .byte_offset
                    .checked_add(fragment.byte_len)
                    .context("fragment offset overflow")?;
                ensure!(end <= file_bytes, "catalog range exceeds finalized file");
            }
            let recording = mp4::read_mp4(file)?;
            ensure!(recording.is_fragmented(), "recording is not fragmented MP4");
            ensure!(recording.tracks().len() == 1, "unexpected recording tracks");
            let samples = recording.sample_count(1)?;
            ensure!(samples >= 45, "{stream} recorded only {samples} samples");
            result.push(json!({"stream": stream, "samples": samples,
                "fragments": fragments.len(), "file_bytes": file_bytes}));
        }
        Ok(result)
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.shutdown.cancel();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("KeepPeek recorder thread panicked"))?;
        }
        if let Some(storage) = self.storage.take() {
            storage.shutdown();
        }
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("native-event recorder cleanup failed: {error:#}");
        }
    }
}

fn documents() -> Vec<Vec<u8>> {
    (0..DOCUMENT_COUNT).map(|index| {
        let timestamp = format!("2026-09-05T12:00:{:02}.{:03}Z", index / 10, (index % 10) * 100);
        let message = notification("VideoSource/MotionAlarm", index % 2 == 0, "Changed", &timestamp, "source-2");
        format!(r#"<tt:MetadataStream xmlns:tt="http://www.onvif.org/ver10/schema"><tt:Event>{message}</tt:Event></tt:MetadataStream>"#).into_bytes()
    }).collect()
}

fn storage_config(root: &Path) -> StorageConfig {
    StorageConfig {
        medium_term_path: root.join("recordings"),
        long_term_path: root.join("recordings"),
        recording_catalog_path: root.join("recordings.db"),
        event_thumbnail_path: root.join("thumbnails"),
        event_thumbnail_max_bytes: 1024 * 1024,
        short_term_duration: Duration::ZERO,
        medium_term_duration: Duration::from_secs(30),
        flush_interval: Duration::ZERO,
        write_buffer_bytes: 8 * 1024,
        long_term_max_bytes: 0,
        minimum_free_bytes: 0,
        maximum_used_percent: None,
        warning_free_bytes: 0,
        critical_free_bytes: 0,
        cleanup_hysteresis_bytes: 0,
    }
}
