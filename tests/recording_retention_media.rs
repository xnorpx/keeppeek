use anyhow::{Context, Result, ensure};
use keeppeek::storage::{
    MediaFrame, RecordingCatalog, RecordingCatalogHandle, RecordingFrame, RecordingStreamIdentity,
    StorageConfig, StorageEngine, VideoCodec, VideoFrame,
    medium_term::MediumTermWriter,
    metadata::{EventSource, TimelineEvent},
    retention::settings::Settings,
};
use std::{
    fs::File,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

// Use a future UTC epoch so the production clock separates matching lifetimes from disabled rules.
const FUTURE_EPOCH_MS: i64 = 3_000_000_000_000;

fn parameter_sets(track: &mp4::Mp4Track, codec: VideoCodec) -> Result<Vec<u8>> {
    let sets = match codec {
        VideoCodec::H264 => {
            let avc = track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .avc1()
                .context("missing AVC configuration")?;
            ensure!(
                avc.avcc.length_size_minus_one == 3,
                "fixture requires four-byte NAL lengths"
            );
            vec![
                track.sequence_parameter_set()?.to_vec(),
                track.picture_parameter_set()?.to_vec(),
            ]
        }
        VideoCodec::H265 => {
            let entry = track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .hev1()
                .or_else(|| track.trak.mdia.minf.stbl.stsd.hvc1())
                .context("missing HEVC configuration")?;
            let config = entry.hvcc.configuration()?;
            ensure!(
                config.nal_length_size == 4,
                "fixture requires four-byte NAL lengths"
            );
            config
                .vps
                .into_iter()
                .chain(config.sps)
                .chain(config.pps)
                .collect()
        }
    };
    let mut data = Vec::new();
    for set in sets {
        data.extend_from_slice(&u32::try_from(set.len())?.to_be_bytes());
        data.extend_from_slice(&set);
    }
    Ok(data)
}

fn write_media(
    root: &Path,
    handle: RecordingCatalogHandle,
    codec: VideoCodec,
) -> Result<(String, PathBuf)> {
    let name = format!("cc-4k-640x360-{codec}.mp4");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/test-camera/testdata")
        .join(name);
    let mut reader = mp4::read_mp4(File::open(source)?)?;
    let track = reader
        .tracks()
        .values()
        .find(|track| {
            matches!(
                track.media_type(),
                Ok(mp4::MediaType::H264 | mp4::MediaType::H265)
            )
        })
        .context("missing video track")?;
    let (id, scale, width, height) = (
        track.track_id(),
        track.timescale(),
        u32::from(track.width()),
        u32::from(track.height()),
    );
    let parameters = parameter_sets(track, codec)?;
    let count = reader.sample_count(id)?;
    let start = Instant::now();
    let mut writer = MediumTermWriter::create_with_catalog_identity(
        root,
        RecordingStreamIdentity::new("front", "main", "front"),
        start,
        8192,
        handle,
    )?;
    let recording_id = writer.recording_id().to_owned();
    for repeat in 0..2 {
        for index in 1..=count {
            let sample = reader
                .read_sample(id, index)?
                .context("missing fixture sample")?;
            let timestamp = Duration::from_secs(repeat)
                + Duration::from_secs_f64(sample.start_time as f64 / f64::from(scale));
            writer.append_one(frame(
                codec,
                (width, height),
                sample,
                &parameters,
                start,
                timestamp,
            ))?;
        }
    }
    Ok((recording_id, writer.finalize()?))
}

fn frame(
    codec: VideoCodec,
    dimensions: (u32, u32),
    sample: mp4::Mp4Sample,
    parameters: &[u8],
    start: Instant,
    timestamp: Duration,
) -> RecordingFrame {
    let mut data = if sample.is_sync {
        parameters.to_vec()
    } else {
        Vec::new()
    };
    data.extend_from_slice(&sample.bytes);
    RecordingFrame {
        received_at: start + timestamp,
        timestamp: Some(timestamp),
        frame: MediaFrame::Video(VideoFrame {
            codec,
            is_keyframe: sample.is_sync,
            width: dimensions.0,
            height: dimensions.1,
            data: data.into(),
        }),
    }
}

fn event(kind: &str, start: i64) -> TimelineEvent {
    TimelineEvent {
        id: kind.into(),
        revision: 1,
        camera_id: "front".into(),
        stream: Some("main".into()),
        source: EventSource::KeepPeek,
        kind: kind.into(),
        start_time_ms: start + 100,
        end_time_ms: Some(start + 200),
        confidence: None,
        bbox: None,
        bbox_attachment_id: None,
        zone: None,
        text: None,
        payload: None,
        attachments: vec![],
        canonical_attachment_id: None,
        icon_key: "event".into(),
        rejected_icon_key: None,
        thumbnail_filename: None,
    }
}

fn media_interval(handle: &RecordingCatalogHandle, id: &str, bytes: usize) -> Result<(i64, i64)> {
    let fragments = handle
        .media_fragments_in_range("front/main", i64::MIN, i64::MAX)?
        .into_iter()
        .filter(|fragment| fragment.recording_id == id)
        .collect::<Vec<_>>();
    ensure!(!fragments.is_empty(), "writer omitted catalog fragments");
    let start = fragments
        .iter()
        .map(|fragment| fragment.start_ms)
        .min()
        .unwrap();
    let end = fragments
        .iter()
        .map(|fragment| fragment.start_ms + i64::try_from(fragment.duration_ms).unwrap())
        .max()
        .unwrap();
    ensure!(
        end - start == 2000,
        "fixture retained an unexpected interval: {} ms",
        end - start
    );
    for fragment in &fragments {
        ensure!(fragment.recording_id == id, "unexpected recording identity");
        ensure!(
            fragment.byte_offset + fragment.byte_len <= bytes as u64,
            "fragment exceeds retained bytes"
        );
    }
    Ok((start, end))
}

fn pin_fixture_epoch(path: &Path, id: &str, original_start: i64, epoch: i64) -> Result<()> {
    let database = pollster::block_on(
        turso::Builder::new_local(path.to_str().context("fixture path is not UTF8")?).build(),
    )?;
    let connection = database.connect()?;
    let shift = epoch - original_start;
    // Map the writer's relative media timeline to a fixed UTC epoch after its catalog owner stops.
    pollster::block_on(async {
        connection.execute_batch("BEGIN IMMEDIATE").await?;
        connection.execute("UPDATE recording_files SET started_at_ms=started_at_ms+?1,ended_at_ms=ended_at_ms+?1 WHERE id=?2",turso::params![shift,id]).await?;
        connection
            .execute(
                "UPDATE recording_fragments SET start_ms=start_ms+?1 WHERE recording_id=?2",
                turso::params![shift, id],
            )
            .await?;
        connection
            .execute(
                "DELETE FROM recording_coverage_files WHERE recording_id=?1",
                [id],
            )
            .await?;
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(())
    })
}

fn decode(path: &Path) -> Result<()> {
    let decoded = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-xerror", "-i"])
        .arg(path)
        .args(["-f", "framehash", "-"])
        .output()?;
    ensure!(
        decoded.status.success(),
        "retained media failed decoding: {}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    let frames = std::str::from_utf8(&decoded.stdout)?
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .count();
    ensure!(
        frames == 30,
        "retained media decoded {frames} frames instead of 30"
    );
    Ok(())
}

fn qualify(codec: VideoCodec, kinds: &[&str], days: i64) -> Result<()> {
    let root = std::env::temp_dir().join(format!("retention-decoded-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let handle = catalog.handle();
    let (id, path) = write_media(&root, handle.clone(), codec)?;
    let before = std::fs::read(&path)?;
    let (original_start, _) = media_interval(&handle, &id, before.len())?;
    catalog.shutdown();
    pin_fixture_epoch(
        &root.join("catalog.db"),
        &id,
        original_start,
        1_700_000_000_000,
    )?;
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let handle = catalog.handle();
    let (start, end) = media_interval(&handle, &id, before.len())?;
    ensure!(start == 1_700_000_000_000, "fixture UTC epoch changed");
    for kind in kinds {
        handle.insert_event(event(kind, start))?;
    }
    let settings: Settings = toml::from_str(
        "[default]\ncontinuous_days=1.0\nmotion_days=7.0\n[default.events]\nperson=30.0",
    )?;
    reconcile_settings(&handle, &settings)?;
    let decision = handle
        .retention_decision(&id)?
        .context("retention decision missing")?;
    ensure!(
        decision.deadline_ms == Some(end + days * 86_400_000),
        "incorrect deadline: {:?}",
        decision.deadline_ms
    );
    ensure!(
        std::fs::read(&path)? == before,
        "retention modified encoded media"
    );
    println!(
        "codec={codec} classes={kinds:?} retained_utc=[{start},{end}) retained_bytes={} deadline_ms={:?}",
        before.len(),
        decision.deadline_ms
    );
    decode(&path)?;
    if kinds.contains(&"person") {
        verify_revised_event(&handle, &id, &path, start, &settings, &before)?;
    }
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn retention_examples_preserve_exact_whole_file_bytes_and_decodable_intervals() -> Result<()> {
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        qualify(codec, &[], 1)?;
        qualify(codec, &["motion"], 7)?;
        qualify(codec, &["motion", "person"], 30)?;
    }
    Ok(())
}

fn reconcile_settings(handle: &RecordingCatalogHandle, settings: &Settings) -> Result<()> {
    ensure!(
        handle.request_retention_settings(Some(settings))?,
        "fixture transition rejected"
    );
    for _ in 0..128 {
        if !handle.reconcile_retention_runtime(4)?.pending {
            return Ok(());
        }
    }
    anyhow::bail!("fixture retention activation did not finish")
}

fn fixture_config(root: &Path, settings: Settings) -> StorageConfig {
    StorageConfig {
        retention: Some(settings),
        medium_term_path: root.into(),
        long_term_path: root.into(),
        recording_catalog_path: root.join("catalog.db"),
        event_thumbnail_path: root.join("images"),
        long_term_max_bytes: 0,
        minimum_free_bytes: 0,
        maximum_used_percent: None,
        warning_free_bytes: 0,
        critical_free_bytes: 0,
        cleanup_hysteresis_bytes: 0,
        ..StorageConfig::default()
    }
}

fn qualify_selection(
    codec: VideoCodec,
    policy: &str,
    kind: Option<&str>,
    remove_unmatched: bool,
) -> Result<()> {
    let root = std::env::temp_dir().join(format!("retention-selection-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let database = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&database)?;
    let handle = catalog.handle();
    let (selected, selected_path) = write_media(&root.join("selected"), handle.clone(), codec)?;
    let (other, other_path) = write_media(&root.join("other"), handle.clone(), codec)?;
    ensure!(selected != other, "writer duplicated a recording identity");
    let bytes = std::fs::read(&selected_path)?;
    decode(&selected_path)?;
    decode(&other_path)?;
    let (selected_start, _) = media_interval(&handle, &selected, bytes.len())?;
    let (other_start, _) = media_interval(
        &handle,
        &other,
        std::fs::metadata(&other_path)?.len() as usize,
    )?;
    catalog.shutdown();
    let epoch = FUTURE_EPOCH_MS;
    pin_fixture_epoch(&database, &selected, selected_start, epoch)?;
    pin_fixture_epoch(&database, &other, other_start, epoch + 3000)?;
    let catalog = RecordingCatalog::open(&database)?;
    let handle = catalog.handle();
    if let Some(kind) = kind {
        handle.insert_event(event(kind, epoch))?;
    }
    let settings: Settings = toml::from_str(policy)?;
    reconcile_settings(&handle, &settings)?;
    let engine = StorageEngine::start_with_catalog(fixture_config(&root, settings), handle.clone());
    let deadline = Instant::now() + Duration::from_secs(8);
    while remove_unmatched && other_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    engine.shutdown();
    ensure!(
        other_path.exists() != remove_unmatched,
        "incorrect unmatched-media expiry"
    );
    ensure!(
        std::fs::read(&selected_path)? == bytes,
        "selected encoded media changed"
    );
    let (start, end) = media_interval(&handle, &selected, bytes.len())?;
    ensure!(
        (start, end) == (epoch, epoch + 2000),
        "selected interval changed"
    );
    let fragments = handle.media_fragments_in_range("front/main", i64::MIN, i64::MAX)?;
    let ids = fragments
        .iter()
        .map(|fragment| &fragment.recording_id)
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        ids.len() == if remove_unmatched { 1 } else { 2 },
        "catalog retained duplicate or expired media"
    );
    println!(
        "codec={codec} selected_kind={kind:?} retained_utc=[{start},{end}) retained_bytes={} unmatched_removed={remove_unmatched}",
        bytes.len()
    );
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn retention_examples_expire_only_unmatched_decodable_media_without_pressure() -> Result<()> {
    let conservative =
        "[default]\ncontinuous_days=1.0\nmotion_days=7.0\n[default.events]\nperson=30.0";
    let reduced = "[default]\ncontinuous_days=0.0\nmotion_days=7.0\n[default.events]\nperson=30.0";
    let alerts = "[default]\ncontinuous_days=0.0\nmotion_days=0.0\n[default.events]\nperson=30.0";
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        qualify_selection(codec, conservative, None, false)?;
        qualify_selection(codec, reduced, Some("motion"), true)?;
        qualify_selection(codec, alerts, Some("person"), true)?;
    }
    Ok(())
}

fn verify_revised_event(
    handle: &RecordingCatalogHandle,
    id: &str,
    path: &Path,
    start: i64,
    settings: &Settings,
    bytes: &[u8],
) -> Result<()> {
    let before = handle
        .retention_decision(id)?
        .context("prior decision missing")?;
    let mut revised = event("person", start);
    revised.revision = 2;
    revised.kind = "motion".into();
    handle.insert_event(revised)?;
    reconcile_settings(handle, settings)?;
    let after = handle
        .retention_decision(id)?
        .context("revised decision missing")?;
    ensure!(
        after.event_revision > before.event_revision,
        "event revision did not reevaluate media"
    );
    ensure!(
        after.deadline_ms == before.deadline_ms,
        "event revision shortened committed media"
    );
    ensure!(
        std::fs::read(path)? == bytes,
        "event revision changed encoded bytes"
    );
    let fragments = handle.media_fragments_in_range("front/main", i64::MIN, i64::MAX)?;
    ensure!(
        !fragments.is_empty() && fragments.iter().all(|fragment| fragment.recording_id == id),
        "event revision duplicated media"
    );
    ensure!(
        media_interval(handle, id, bytes.len())? == (start, start + 2000),
        "event revision changed retained coverage"
    );
    decode(path)?;
    Ok(())
}
