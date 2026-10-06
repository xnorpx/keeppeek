use super::*;
use keeppeek::storage::playback::export_fragment_ranges;

struct Clip {
    id: String,
    path: PathBuf,
    bytes: Vec<u8>,
    start_ms: i64,
}

fn seed_clips(root: &Path, codec: VideoCodec) -> Result<Vec<Clip>> {
    let database = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&database)?;
    let handle = catalog.handle();
    let mut clips = Vec::with_capacity(3);
    let mut starts = Vec::with_capacity(3);
    for index in 0..3 {
        let (id, path) = write_media(&root.join(format!("clip-{index}")), handle.clone(), codec)?;
        let bytes = std::fs::read(&path)?;
        decode(&path)?;
        let (start, _) = media_interval(&handle, &id, bytes.len())?;
        starts.push(start);
        clips.push(Clip {
            id,
            path,
            bytes,
            start_ms: FUTURE_EPOCH_MS + index * 2000,
        });
    }
    catalog.shutdown();
    for (clip, start) in clips.iter().zip(starts) {
        pin_fixture_epoch(&database, &clip.id, start, clip.start_ms)?;
    }
    commit_initial_floors(root)?;
    std::fs::remove_file(&clips[1].path)?;
    ensure!(
        !clips[1].path.exists(),
        "missing-media fixture still exists"
    );
    Ok(clips)
}

fn commit_initial_floors(root: &Path) -> Result<()> {
    let mut catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    catalog.wait_for_maintenance();
    let settings: Settings = toml::from_str("[default]\ncontinuous_days=1.0")?;
    reconcile_settings(&catalog.handle(), &settings)?;
    catalog.shutdown();
    Ok(())
}

fn verify_clip(handle: &RecordingCatalogHandle, clip: &Clip, days: i64) -> Result<()> {
    let decision = handle
        .retention_decision(&clip.id)?
        .context("missing decision")?;
    ensure!(
        decision.deadline_ms == Some(clip.start_ms + 2000 + days * 86_400_000),
        "absent producer fabricated or omitted retention evidence"
    );
    ensure!(
        std::fs::read(&clip.path)? == clip.bytes,
        "unavailable source changed neighboring media"
    );
    ensure!(
        media_interval(handle, &clip.id, clip.bytes.len())?
            == (clip.start_ms, clip.start_ms + 2000),
        "missing source fabricated neighboring coverage"
    );
    decode(&clip.path)?;
    Ok(())
}

fn qualify_missing(root: &Path, codec: VideoCodec, kind: Option<&str>, days: i64) -> Result<()> {
    let clips = seed_clips(root, codec)?;
    let mut catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    catalog.wait_for_maintenance();
    let handle = catalog.handle();
    if let Some(kind) = kind {
        handle.insert_event(event(kind, clips[0].start_ms))?;
    }
    let mut gap_event = event("person", clips[1].start_ms);
    gap_event.id = "missing-source-person".into();
    handle.insert_event(gap_event)?;
    let settings: Settings = toml::from_str(
        "[default]\ncontinuous_days=1.0\nmotion_days=7.0\n[default.events]\nperson=30.0",
    )?;
    reconcile_settings(&handle, &settings)?;
    verify_clip(&handle, &clips[0], days)?;
    verify_clip(&handle, &clips[2], 1)?;
    let missing = handle.media_fragments_in_range(
        "front/main",
        clips[1].start_ms,
        clips[1].start_ms + 2000,
    )?;
    ensure!(
        !missing.is_empty()
            && missing
                .iter()
                .all(|fragment| fragment.recording_id == clips[1].id),
        "missing source replaced the original recording identity"
    );
    let export = export_fragment_ranges(
        &missing,
        clips[1].start_ms + 2000,
        &root.join("missing-export.mp4"),
        || false,
    );
    ensure!(
        export.is_err(),
        "missing source produced a successful export"
    );
    ensure!(
        !clips[1].path.exists(),
        "late evidence fabricated missing media"
    );
    catalog.shutdown();
    println!(
        "missing-source codec={codec} available_producer={kind:?} surviving_files=2 exact_bytes=true missing_export_rejected=true"
    );
    Ok(())
}

#[test]
fn missing_source_and_absent_motion_or_object_producers_do_not_fabricate_media_or_evidence()
-> Result<()> {
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        for (kind, days) in [(None, 1), (Some("motion"), 7), (Some("person"), 30)] {
            let root =
                std::env::temp_dir().join(format!("retention-missing-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root)?;
            qualify_missing(&root, codec, kind, days)?;
            std::fs::remove_dir_all(root)?;
        }
    }
    Ok(())
}
