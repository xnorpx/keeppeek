//! Compares the same paced multi-camera workload with and without local verified transfers.

use super::*;
use crate::storage::{archive, test_media::video_fixture};
use anyhow::{Context as _, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    thread,
};

const CAMERAS: usize = 8;
const FPS: u32 = 15;
const SECONDS: u32 = 30;

fn fixture_sample(template: &RecordingFrame, start: Instant, offset: Duration) -> RecordingFrame {
    let MediaFrame::Video(video) = &template.frame else {
        unreachable!("video fixture")
    };
    RecordingFrame {
        received_at: start + offset,
        timestamp: Some(offset),
        frame: MediaFrame::Video(VideoFrame {
            codec: video.codec,
            width: video.width,
            height: video.height,
            is_keyframe: video.is_keyframe,
            data: video.data.clone(),
        }),
    }
}

#[test]
fn failed_relocation_processes_the_remaining_drained_frames_and_reports_the_error()
-> anyhow::Result<()> {
    let (root, catalog, mut config) = named_fixture()?;
    config.volume_runtime = None;
    config.medium_term_duration = Duration::from_secs(2);
    config.short_term_duration = Duration::from_secs(60);
    config.flush_interval = Duration::from_secs(60);
    let destination = config.long_term_path.clone();
    std::fs::write(&destination, b"unrelated occupant")?;
    let camera = RecordingStreamIdentity::new("relocation", "main", "relocation");
    let source = video_fixture(VideoCodec::H264, "640x360");
    let start = Instant::now() - Duration::from_secs(8);
    let mut worker = WriterWorker::new(
        config,
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    for index in 0..8 {
        worker.ingest(
            camera.clone(),
            fixture_sample(
                &source[(index % 2) as usize],
                start,
                Duration::from_millis(index * 500),
            ),
        );
    }
    worker.flush_all(true);
    let snapshot = worker.health.snapshot();
    let stream = snapshot
        .streams
        .iter()
        .find(|stream| stream.stream_id == camera.storage_key)
        .context("missing recording health")?;
    ensure!(
        stream.last_error.is_some() && stream.recorded_duration_ms > 0,
        "relocation failure and surviving writer progress must both be visible"
    );
    assert_eq!(std::fs::read(&destination)?, b"unrelated occupant");
    std::fs::remove_file(&destination)?;
    std::fs::create_dir(&destination)?;
    worker.finalize_all();
    let files: BTreeSet<_> = catalog
        .handle()
        .media_fragments_in_range(&camera.storage_key, 0, i64::MAX)?
        .into_iter()
        .map(|fragment| fragment.path)
        .collect();
    assert_eq!(files.len(), 2);
    for file in files {
        assert_eq!(samples(Path::new(&file)), 4);
    }
    drop(worker);
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn late_no_viewer_flush_delivers_predictive_frames_before_buffer_expiry() -> anyhow::Result<()> {
    let (root, catalog, mut config) = named_fixture()?;
    config.short_term_duration = Duration::from_secs(2);
    config.flush_interval = Duration::from_secs(60);
    config.medium_term_duration = Duration::from_secs(300);
    let camera = RecordingStreamIdentity::new("late-buffer", "main", "late-buffer");
    let mut worker = WriterWorker::new(
        config,
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    let source = video_fixture(VideoCodec::H264, "640x360");
    let start = Instant::now() - Duration::from_secs(90);
    for (index, (template, ms)) in [
        (0, 0),
        (1, 100),
        (2, 200),
        (3, 300),
        (4, 400),
        (5, 63_500),
        (0, 63_600),
        (1, 63_700),
    ]
    .into_iter()
    .enumerate()
    {
        worker.ingest(
            camera.clone(),
            fixture_sample(&source[template], start, Duration::from_millis(ms)),
        );
        if index == 1 {
            worker.flush_all(false);
            assert!(worker.pipelines[&camera.storage_key].medium_term.is_some());
        }
    }
    worker.flush_all(true);
    worker.finalize_all();
    let paths: BTreeSet<_> = catalog
        .handle()
        .media_fragments_in_range(&camera.storage_key, 0, i64::MAX)?
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(paths.len(), 1);
    assert_eq!(samples(Path::new(paths.first().unwrap())), 8);
    drop(worker);
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn scheduled_flush_preserves_predictive_frames_with_or_without_prior_viewer_demand()
-> anyhow::Result<()> {
    let source = video_fixture(VideoCodec::H264, "640x360");
    let start = Instant::now() - Duration::from_secs(10);
    for starts_with_viewer in [false, true] {
        let (root, catalog, mut config) = named_fixture()?;
        config.medium_term_duration = Duration::from_secs(60);
        config.short_term_duration = Duration::from_secs(2);
        config.flush_interval = Duration::from_secs(60);
        let camera = RecordingStreamIdentity::new("scheduled", "main", "scheduled");
        let demand = RecordingDemand::new(Duration::ZERO);
        let mut viewer = starts_with_viewer.then(|| demand.acquire(&camera.storage_key));
        let mut worker = WriterWorker::new(config, demand, Some(catalog.handle()));
        for (index, template) in [0, 1, 2, 3, 0, 1].into_iter().enumerate() {
            assert_eq!(source[template].is_video_keyframe(), template == 0);
            worker.ingest(
                camera.clone(),
                fixture_sample(
                    &source[template],
                    start,
                    Duration::from_millis(index as u64 * 100),
                ),
            );
            if index == 1 {
                worker.flush_all(false);
                drop(viewer.take());
            }
        }
        worker.flush_all(false);
        worker.flush_all(true);
        worker.finalize_all();
        let paths: BTreeSet<_> = catalog
            .handle()
            .media_fragments_in_range(&camera.storage_key, 0, i64::MAX)?
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(paths.len(), 1);
        assert_eq!(
            samples(Path::new(paths.first().unwrap())),
            6,
            "initial viewer={starts_with_viewer}"
        );
        drop(worker);
        catalog.shutdown();
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn queued_keyframes_rotate_at_media_boundaries_in_direct_and_buffered_writes() -> anyhow::Result<()>
{
    let start = Instant::now() - Duration::from_secs(60);
    let fixture = video_fixture(VideoCodec::H264, "640x360");
    for direct in [true, false] {
        let (root, catalog, mut config) = named_fixture()?;
        let camera = RecordingStreamIdentity::new("backlogged", "main", "backlogged");
        config.medium_term_duration = Duration::from_secs(3);
        let buffer = if direct {
            Duration::ZERO
        } else {
            Duration::from_secs(60)
        };
        config.short_term_duration = buffer;
        config.flush_interval = buffer;
        let mut writer = WriterWorker::new(
            config,
            RecordingDemand::new(Duration::ZERO),
            Some(catalog.handle()),
        );
        for tick in 0..12 {
            let template = &fixture[(tick % 2) as usize];
            assert_eq!(template.is_video_keyframe(), tick % 2 == 0);
            writer.ingest(
                camera.clone(),
                fixture_sample(template, start, Duration::from_millis(tick * 500)),
            );
        }
        writer.flush_all(true);
        writer.finalize_all();
        let paths: BTreeSet<_> = catalog
            .handle()
            .media_fragments_in_range(&camera.storage_key, 0, i64::MAX)?
            .into_iter()
            .map(|fragment| fragment.path)
            .collect();
        assert_eq!(paths.len(), 2, "direct={direct}");
        for path in paths {
            assert_eq!(samples(Path::new(&path)), 6, "direct={direct}");
        }
        drop(writer);
        catalog.shutdown();
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn buffered_rotation_keeps_inter_frames_until_the_next_keyframe() -> anyhow::Result<()> {
    let (root, catalog, mut config) = named_fixture()?;
    config.flush_interval = Duration::from_secs(60);
    let demand = RecordingDemand::new(Duration::ZERO);
    let _viewer = demand.acquire("front/main");
    let mut writer = WriterWorker::new(config, demand, Some(catalog.handle()));
    let camera = RecordingStreamIdentity::new("front", "main", "front");
    let mut gop = video_fixture(VideoCodec::H264, "640x360").into_iter();
    writer.ingest(camera.clone(), gop.next().unwrap());
    let active = writer.pipelines[&camera.storage_key]
        .medium_term
        .as_ref()
        .unwrap();
    let first = active.active_path().to_owned();
    writer.config.medium_term_duration = Duration::ZERO;
    for frame in gop.by_ref().take(2) {
        assert!(!frame.is_video_keyframe());
        writer.ingest(camera.clone(), frame);
    }
    writer.ingest(
        camera.clone(),
        gop.find(RecordingFrame::is_video_keyframe).unwrap(),
    );
    let next = writer.pipelines[&camera.storage_key]
        .medium_term
        .as_ref()
        .unwrap()
        .active_path()
        .to_owned();
    assert_ne!(first, next);
    writer.finalize_all();
    assert_eq!(samples(&first), 3);
    assert_eq!(samples(&next), 1);
    drop(writer);
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn missing_keyframe_bounds_prepared_and_active_gops_and_recovers_at_the_next_keyframe()
-> anyhow::Result<()> {
    let (root, catalog, manager) = runtime::tests::fixture(64 * MEBIBYTE_BYTES)?;
    let mut config = storage_config("no-next-keyframe");
    config.volume_runtime = Some(Arc::new(manager));
    config.medium_term_path = root.join("primary");
    config.long_term_path = root.join("primary");
    config.recording_catalog_path = root.join("catalog.db");
    config.short_term_duration = Duration::from_secs(2);
    config.flush_interval = Duration::from_secs(60);
    let demand = RecordingDemand::new(Duration::ZERO);
    let mut writer = WriterWorker::new(config, demand.clone(), Some(catalog.handle()));
    let gop = video_fixture(VideoCodec::H264, "640x360");
    let (MediaFrame::Video(key), MediaFrame::Video(predicted)) = (&gop[0].frame, &gop[1].frame)
    else {
        unreachable!("video fixture");
    };
    assert!(key.is_keyframe && !predicted.is_keyframe);
    // A length-prefixed H.264 filler NAL makes two ordinary samples exceed the 16 MiB bound.
    let mut filled = predicted.data.to_vec();
    let filler_size = 9_u32 * 1024 * 1024;
    filled.extend(filler_size.to_be_bytes());
    filled.push(0x0c);
    filled.resize(filled.len() + filler_size as usize - 2, 0xff);
    filled.push(0x80);
    let filled = bytes::Bytes::from(filled);
    let start = Instant::now() - Duration::from_secs(15);
    let sample = |video: &VideoFrame, data, second| RecordingFrame {
        received_at: start + Duration::from_secs(second),
        timestamp: Some(Duration::from_secs(second)),
        frame: MediaFrame::Video(VideoFrame {
            codec: video.codec,
            width: video.width,
            height: video.height,
            is_keyframe: video.is_keyframe,
            data,
        }),
    };
    for (source, active) in [("first-gop", false), ("later-gop", true)] {
        let camera = RecordingStreamIdentity::new(source, "main", source);
        let _viewer = demand.acquire(&camera.storage_key);
        writer.config.medium_term_duration = Duration::from_secs(3_600);
        writer.ingest(camera.clone(), sample(key, key.data.clone(), 0));
        if active {
            writer.ingest(camera.clone(), sample(key, key.data.clone(), 1));
        }
        let current = &writer.pipelines[&camera.storage_key].medium_term;
        let first_path = current.as_ref().unwrap().active_path().to_owned();
        if !active {
            writer.config.medium_term_duration = Duration::ZERO;
        }
        writer.ingest(camera.clone(), sample(predicted, filled.clone(), 2));
        writer.ingest(camera.clone(), sample(predicted, filled.clone(), 3));
        writer.ingest(camera.clone(), sample(predicted, predicted.data.clone(), 4));
        let snapshot = writer.health.snapshot();
        let health = snapshot
            .streams
            .iter()
            .find(|s| s.stream_id == camera.storage_key)
            .unwrap();
        assert!(
            health
                .last_error
                .as_deref()
                .is_some_and(|reason| reason.contains("keyframe limit"))
        );
        let current = writer.pipelines[&camera.storage_key]
            .medium_term
            .as_ref()
            .unwrap();
        assert!(current.awaiting_recovery_keyframe());
        let mut resumed = sample(key, key.data.clone(), 9);
        let mut following = sample(predicted, predicted.data.clone(), 10);
        if active {
            resumed.timestamp = Some(Duration::ZERO);
            following.timestamp = Some(Duration::from_secs(1));
        }
        writer.ingest(camera.clone(), resumed);
        writer.ingest(camera.clone(), following);
        let recovered = writer.pipelines[&camera.storage_key]
            .medium_term
            .as_ref()
            .unwrap();
        assert!(!recovered.awaiting_recovery_keyframe());
        let second_path = recovered.active_path().to_owned();
        writer.finalize_all();
        if active {
            assert_eq!(first_path, second_path);
            assert_eq!(samples(&second_path), 5);
        } else {
            assert_ne!(first_path, second_path);
            assert_eq!(samples(&first_path), 2);
            assert_eq!(samples(&second_path), 2);
        }
        let fragments =
            catalog
                .handle()
                .media_fragments_in_range(&camera.storage_key, 0, i64::MAX)?;
        let during_gap = fragments[0].start_ms + 3_000;
        assert!(fragments.iter().all(|f| {
            !(f.start_ms..f.start_ms.saturating_add_unsigned(f.duration_ms)).contains(&during_gap)
        }));
        let recovered_at = fragments.last().unwrap().start_ms - fragments[0].start_ms;
        if active {
            assert_eq!(recovered_at, 9_000);
        } else {
            assert!(recovered_at.abs_diff(9_000) <= 2);
        }
    }
    let oversized = RecordingStreamIdentity::new("oversized-first", "main", "oversized-first");
    let _viewer = demand.acquire(&oversized.storage_key);
    writer.config.medium_term_duration = Duration::from_secs(3_600);
    writer.ingest(
        oversized.clone(),
        sample(key, vec![0_u8; 16 * 1024 * 1024].into(), 0),
    );
    writer.ingest(oversized.clone(), sample(key, key.data.clone(), 9));
    writer.ingest(
        oversized.clone(),
        sample(predicted, predicted.data.clone(), 10),
    );
    writer.finalize_all();
    let fragments =
        catalog
            .handle()
            .media_fragments_in_range(&oversized.storage_key, 0, i64::MAX)?;
    assert_eq!(fragments.len(), 1);
    assert_eq!(
        fragments[0].start_ms - fragments[0].recording_started_at_ms,
        9_000
    );
    assert_eq!(samples(Path::new(&fragments[0].path)), 2);
    drop(writer);
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[ignore = "paced local disk qualification; run alone with --ignored --nocapture"]
fn verified_archive_under_paced_recording_and_review() -> anyhow::Result<()> {
    let frames = video_fixture(VideoCodec::H264, "3840x2160");
    assert!(frames[0].is_video_keyframe());
    assert_eq!(
        frames
            .iter()
            .take(FPS as usize)
            .filter(|frame| frame.is_video_keyframe())
            .count(),
        1
    );
    for run in 0..3 {
        for enabled in [run % 2 == 1, run % 2 == 0] {
            qualify(&frames, enabled, run + 1).with_context(|| {
                format!("paired repetition {}, archive enabled={enabled}", run + 1)
            })?;
        }
    }
    Ok(())
}

fn qualify(frames: &[RecordingFrame], enabled: bool, run: usize) -> anyhow::Result<()> {
    let archive_parent = std::env::var_os("KEEPPEEK_TEST_ARCHIVE_PARENT").map(PathBuf::from);
    if let Some(parent) = &archive_parent {
        ensure!(
            parent.is_absolute() && parent.is_dir(),
            "archive fixture parent must be an existing absolute directory"
        );
    }
    let (root, catalog, manager) = runtime::tests::fixture(256 * MEBIBYTE_BYTES)?;
    let result = with_archive_target(&root, archive_parent.as_deref(), |target| {
        let worker = if enabled {
            Some(start_archive(target.to_owned(), catalog.handle())?)
        } else {
            None
        };
        run_fixture(&root, catalog.handle(), manager, frames, worker)
    });
    catalog.shutdown();
    let cleanup = std::fs::remove_dir_all(root);
    let mut report = result?;
    cleanup?;
    report["archive_parent_override"] = archive_parent.is_some().into();
    report["run"] = run.into();
    println!("{report}");
    Ok(())
}

fn with_archive_target<T>(
    root: &Path,
    archive_parent: Option<&Path>,
    run: impl FnOnce(&Path) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let target = archive_parent.map_or_else(
        || root.join("verified"),
        |parent| parent.join(format!("keeppeek-archive-trial-{}", uuid::Uuid::new_v4())),
    );
    // Only successful exclusive creation grants permission to clean up an external child.
    std::fs::create_dir(&target)?;
    let result = runtime::tests::protect_created_root(&target).and_then(|()| run(&target));
    if archive_parent.is_none() {
        return result;
    }
    let cleanup = std::fs::remove_dir_all(&target);
    let report = result?;
    cleanup?;
    Ok(report)
}

#[test]
fn qualification_reports_the_run_error_before_a_secondary_cleanup_error() -> anyhow::Result<()> {
    let parent = std::fs::canonicalize(std::env::temp_dir())?;
    let failed = with_archive_target(&parent, Some(&parent), |target| -> anyhow::Result<()> {
        std::fs::remove_dir(target)?;
        anyhow::bail!("fixture run failed")
    });
    assert_eq!(failed.unwrap_err().to_string(), "fixture run failed");
    let completed = with_archive_target(&parent, Some(&parent), |target| {
        std::fs::remove_dir(target)?;
        Ok(())
    });
    assert_eq!(
        completed.unwrap_err().downcast::<std::io::Error>()?.kind(),
        std::io::ErrorKind::NotFound
    );
    Ok(())
}

#[test]
fn qualification_creates_one_private_target_in_each_location_and_preserves_occupants()
-> anyhow::Result<()> {
    let base = std::env::temp_dir();
    #[cfg(unix)]
    let base = std::fs::canonicalize(base)?;
    let root = base.join(format!("keeppeek-archive-targets-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let external = root.join("separate-archive-parent");
    std::fs::create_dir(&external)?;
    std::fs::write(external.join("existing"), b"keep")?;
    for parent in [None, Some(external.as_path())] {
        with_archive_target(&root, parent, |target| {
            assert!(target.is_dir());
            assert_eq!(target.parent(), Some(parent.unwrap_or(&root)));
            let root = crate::storage::volumes::root::Root::open(target)?;
            archive::filesystem::FilesystemArchiveTarget::open(target, root.identity())?;
            Ok(())
        })?;
    }
    assert!(with_archive_target(&root, None, |_| Ok(())).is_err());
    assert!(root.join("verified").is_dir());
    assert_eq!(std::fs::read(external.join("existing"))?, b"keep");
    assert_eq!(std::fs::read_dir(&external)?.count(), 1);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

fn run_fixture(
    root: &Path,
    catalog_handle: RecordingCatalogHandle,
    manager: runtime::Manager,
    frames: &[RecordingFrame],
    worker: Option<archive::Worker>,
) -> anyhow::Result<serde_json::Value> {
    let mut config = storage_config("verified-archive");
    config.volume_runtime = Some(Arc::new(manager));
    config.medium_term_path = root.join("primary");
    config.long_term_path = root.join("primary");
    config.recording_catalog_path = root.join("catalog.db");
    config.medium_term_duration = Duration::from_secs(5);
    config.short_term_duration = Duration::from_secs(2);
    config.flush_interval = Duration::from_millis(250);
    let archive = worker.as_ref().map(archive::Worker::handle);
    config.archive_runtime.clone_from(&archive);
    let engine = StorageEngine::start_with_catalog(config, catalog_handle.clone());
    let storage = engine.handle();
    let cameras: Vec<_> = (0..CAMERAS)
        .map(|id| {
            let source = format!("camera-{id}");
            storage.configure_camera_recording(&source, CameraRecordingMode::Main, Duration::ZERO);
            RecordingStreamIdentity::new(&source, "main", &source)
        })
        .collect();
    let _viewers: Vec<_> = cameras
        .iter()
        .map(|camera| engine.demand().acquire(camera.storage_key.clone()))
        .collect();
    let start = Instant::now();
    let mut report = thread::scope(|scope| -> anyhow::Result<_> {
        let producer = scope.spawn(|| ingest(&storage, &cameras, frames, start));
        let mut report = observe(
            &engine,
            &catalog_handle,
            archive.as_ref(),
            &frames[1],
            start,
            || producer.is_finished(),
        )?;
        let (ingest_us, lateness_us) = producer
            .join()
            .map_err(|_| anyhow::anyhow!("recording producer panicked"))?;
        report["ingest_us"] = quantiles(ingest_us);
        report["frame_deadline_lateness_us"] = quantiles(lateness_us);
        Ok(report)
    })?;
    let health = engine.health();
    engine.shutdown();
    let recordings = check_recordings(&catalog_handle, &cameras)
        .with_context(|| format!("writers after queue drain: {:?}", health.snapshot().streams))?;
    if let Some(handle) = archive {
        wait_for_replicas(&handle, &recordings)?;
    }
    report["archive"] = worker.is_some().into();
    report["primary_recordings"] = recordings.len().into();
    if let Some(worker) = worker {
        worker.shutdown()?;
    }
    Ok(report)
}

fn wait_for_replicas(
    handle: &archive::Handle,
    recordings: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let jobs = archive_jobs(handle)?;
        ensure!(
            jobs.iter().all(|j| j.state != "failed"),
            "archive job failed"
        );
        let ready: BTreeSet<_> = jobs
            .iter()
            .filter(|j| j.state == "ready")
            .map(|j| j.recording_id.clone())
            .collect();
        if ready == *recordings {
            ensure!(
                jobs.len() == recordings.len(),
                "unexpected extra archive job"
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "archive finished {} of {} recordings",
            ready.len(),
            recordings.len()
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn start_archive(
    root: PathBuf,
    catalog: RecordingCatalogHandle,
) -> anyhow::Result<archive::Worker> {
    let settings = archive::settings::Settings {
        target_id: "local".into(),
        root,
        enabled: true,
        automatic: true,
        source_ids: vec![],
        minimum_age_secs: 0,
        evict_after_archive: false,
        retention_days: None,
        maximum_object_bytes: 8 * MEBIBYTE_BYTES,
        maximum_pending_count: 64,
        maximum_pending_bytes: 128 * MEBIBYTE_BYTES,
        maximum_attempts: 3,
        operation_timeout_secs: 30,
    };
    archive::Worker::start(settings, catalog)
}

#[derive(Clone, Debug, serde::Serialize)]
struct Progress {
    elapsed_ms: u64,
    ready: usize,
    writer_ms: BTreeMap<String, u64>,
    completed_seeks: usize,
}

fn observe(
    engine: &StorageEngine,
    catalog: &RecordingCatalogHandle,
    archive: Option<&archive::Handle>,
    expected: &RecordingFrame,
    start: Instant,
    finished: impl Fn() -> bool,
) -> anyhow::Result<serde_json::Value> {
    let mut meter = drain_performance::ProcessMeter::new();
    let cpu_before = meter.sample();
    let mut review_us = Vec::new();
    let (mut ready, mut ready_bytes) = (0, 0);
    let (mut first, mut last) = (None, None);
    let mut window = Duration::ZERO;
    for tick in 1..SECONDS * 4 {
        let due = start + Duration::from_millis(u64::from(tick) * 250);
        thread::sleep(due.saturating_duration_since(Instant::now()));
        if finished() {
            break;
        }
        let sample = Instant::now();
        let read_us =
            review_primary(catalog, expected)?.then(|| sample.elapsed().as_secs_f64() * 1e6);
        let copies = archive.map(verified).transpose()?;
        let streams = if copies.is_some_and(|(count, _)| count > ready) {
            Some(engine.health().snapshot().streams)
        } else {
            None
        };
        let elapsed = start.elapsed();
        // A slow disk poll can finish after the last frame was submitted. Commit only a
        // complete in-ingest observation, including its byte, read and writer counters.
        if finished() {
            break;
        }
        window = elapsed;
        review_us.extend(read_us);
        if let Some((count, bytes)) = copies {
            if let Some(streams) = streams.filter(|streams| streams.len() == CAMERAS) {
                let observed = Progress {
                    elapsed_ms: elapsed.as_millis().try_into()?,
                    ready: count,
                    writer_ms: streams
                        .into_iter()
                        .map(|s| (s.stream_id, s.recorded_duration_ms))
                        .collect(),
                    completed_seeks: review_us.len(),
                };
                first.get_or_insert_with(|| observed.clone());
                last = Some(observed);
            }
            (ready, ready_bytes) = (count, bytes);
        }
        meter.sample();
    }
    check_progress(
        engine,
        review_us.len(),
        archive.is_some(),
        first.as_ref(),
        last.as_ref(),
    )?;
    Ok(serde_json::json!({
        "camera_count": CAMERAS,
        "samples_per_camera": FPS * SECONDS,
        "window_seconds": window.as_secs_f64(),
        "primary_seek_us": quantiles(review_us),
        "ready_during_ingest": ready,
        "first_overlap": first,
        "last_overlap": last,
        "verified_bytes_during_ingest": ready_bytes,
        "verified_mib_per_second_over_window": ready_bytes as f64 / MEBIBYTE_BYTES as f64 / window.as_secs_f64(),
        "process_cpu_ms": meter.sample() - cpu_before,
        "sampled_peak_process_rss_bytes": meter.resident,
    }))
}

fn check_progress(
    engine: &StorageEngine,
    seeks: usize,
    enabled: bool,
    first: Option<&Progress>,
    last: Option<&Progress>,
) -> anyhow::Result<()> {
    let streams = engine.health().snapshot().streams;
    ensure!(
        streams.len() == CAMERAS
            && streams
                .iter()
                .all(|stream| stream.last_failure_at_ms.is_none()
                    && stream.recorded_duration_ms >= 1000),
        "writers did not make healthy progress: {streams:?}"
    );
    ensure!(seeks > 0, "no primary sample was reviewed during ingest");
    if enabled {
        let first = first.context("no verified replica observed during ingest")?;
        let last = last.context("no verified replica observed during ingest")?;
        ensure!(
            last.ready > first.ready && last.completed_seeks > first.completed_seeks,
            "verified replicas and primary seeks must both advance during ingest: first={first:?}, last={last:?}"
        );
        for (id, before) in &first.writer_ms {
            ensure!(
                last.writer_ms
                    .get(id)
                    .is_some_and(|after| *after >= before + 1000),
                "no sustained writer progress on {id} between archive checkpoints: first={first:?}, last={last:?}"
            );
        }
    }
    Ok(())
}

fn ingest(
    storage: &StorageHandle,
    cameras: &[RecordingStreamIdentity],
    frames: &[RecordingFrame],
    start: Instant,
) -> (Vec<f64>, Vec<f64>) {
    let mut ingest_us = Vec::new();
    let mut lateness = Vec::new();
    for index in 0..FPS * SECONDS {
        let offset = Duration::from_secs_f64(f64::from(index) / f64::from(FPS));
        let due = start + offset;
        thread::sleep(due.saturating_duration_since(Instant::now()));
        lateness.push(due.elapsed().as_secs_f64() * 1e6);
        let MediaFrame::Video(video) = &frames[(index % FPS) as usize].frame else {
            unreachable!("video fixture")
        };
        for camera in cameras {
            let frame = RecordingFrame {
                received_at: due,
                timestamp: Some(offset),
                frame: MediaFrame::Video(VideoFrame {
                    codec: video.codec,
                    width: video.width,
                    height: video.height,
                    is_keyframe: video.is_keyframe,
                    data: video.data.clone(),
                }),
            };
            let now = Instant::now();
            storage.ingest_stream(camera.clone(), frame);
            ingest_us.push(now.elapsed().as_secs_f64() * 1e6);
        }
    }
    (ingest_us, lateness)
}

fn review_primary(
    catalog: &RecordingCatalogHandle,
    expected: &RecordingFrame,
) -> anyhow::Result<bool> {
    let (fragments, _lease) =
        catalog.leased_media_fragments_in_range("camera-0/main", 0, i64::MAX)?;
    let Some(first) = fragments.first() else {
        return Ok(false);
    };
    let Reply::Location(Some(_)) = catalog.volume_location(Request::Lookup(Object {
        kind: Kind::Recording,
        id: first.recording_id.clone(),
    }))?
    else {
        return Ok(false);
    };
    let mut reader = mp4::read_mp4(File::open(&first.path)?)?;
    let track = *reader.tracks().keys().next().context("no video track")?;
    let sample = reader
        .read_sample(track, 2)?
        .context("no second video frame")?;
    let MediaFrame::Video(video) = &expected.frame else {
        unreachable!("video fixture")
    };
    ensure!(
        sample.bytes == video.data,
        "primary seek returned a different frame"
    );
    Ok(true)
}

fn archive_jobs(
    handle: &archive::Handle,
) -> anyhow::Result<Vec<crate::storage::catalog::archive::Job>> {
    let mut jobs = Vec::new();
    // At most one new recording per emitted keyframe.
    for _ in 0..=(CAMERAS * SECONDS as usize) / 64 {
        let after = jobs
            .last()
            .map_or("", |j: &crate::storage::catalog::archive::Job| {
                j.id.as_str()
            });
        let page = handle.list(after, 64)?;
        let done = page.len() < 64;
        jobs.extend(page);
        if done {
            return Ok(jobs);
        }
    }
    anyhow::bail!("archive created more jobs than the camera workload permits")
}

fn verified(handle: &archive::Handle) -> anyhow::Result<(usize, u64)> {
    let jobs = archive_jobs(handle)?;
    ensure!(
        jobs.iter().all(|job| job.state != "failed"),
        "archive job failed"
    );
    Ok(jobs
        .iter()
        .filter(|job| job.state == "ready")
        .fold((0, 0), |(count, bytes), job| (count + 1, bytes + job.bytes)))
}

fn check_recordings(
    catalog: &RecordingCatalogHandle,
    cameras: &[RecordingStreamIdentity],
) -> anyhow::Result<BTreeSet<String>> {
    let mut recordings = BTreeSet::new();
    for camera in cameras {
        let (fragments, _lease) =
            catalog.leased_media_fragments_in_range(&camera.storage_key, 0, i64::MAX)?;
        let paths: BTreeMap<_, _> = fragments
            .into_iter()
            .map(|f| (f.recording_id, f.path))
            .collect();
        let per_segment: Vec<_> = paths
            .values()
            .map(|path| samples(Path::new(path)))
            .collect();
        let recorded_samples: u32 = per_segment.iter().sum();
        ensure!(
            recorded_samples == FPS * SECONDS,
            "{} retained {recorded_samples} samples across segments {per_segment:?}",
            camera.storage_key
        );
        recordings.extend(paths.into_keys());
    }
    Ok(recordings)
}

fn quantiles(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    serde_json::json!({
        "n": values.len(),
        "p50": values[values.len().div_ceil(2) - 1],
        "p95": values[(values.len() * 95).div_ceil(100) - 1],
    })
}
