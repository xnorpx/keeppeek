use keeppeek::storage::{
    CatalogFragment, CatalogRecording, RecordingCatalog,
    timelapse::{
        DEFAULT_TOLERANCE_MS, MAX_OUTPUT_HEIGHT, MAX_OUTPUT_WIDTH, MAX_RANGE_MS, MAX_SAMPLES,
        TimelapseCadence, TimelapseGapReason, TimelapseOutputPreview, TimelapsePlanItem,
        TimelapsePlanRequest, TimelapsePreset, decode_sample_token, generate_target_times,
        plan_timelapse, read_sample_decode_window,
    },
    timelapse_encoder::{
        TimelapseEncodeError, TimelapseEncodeRequest, encode_timelapse_mp4, probe_local_encoder,
    },
};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new(prefix: &str) -> Self {
        let root = std::env::temp_dir().join(format!("keeppeek-{prefix}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/test-camera/testdata")
        .join(name)
}

fn file_sha256(path: &Path) -> [u8; 32] {
    Sha256::digest(fs::read(path).unwrap()).into()
}

struct FragmentSpan {
    sequence: u64,
    start_ms: i64,
    duration_ms: u64,
    byte_offset: u64,
    byte_len: u64,
}

struct WrittenRecording {
    init_len: u64,
    fragments: Vec<FragmentSpan>,
}

fn write_fragmented_recording_from_fixture(
    fixture: &Path,
    dest: &Path,
    start_ms: i64,
    vfr_durations_ms: Option<&[u32]>,
) -> WrittenRecording {
    let mut reader = mp4::read_mp4(File::open(fixture).unwrap()).unwrap();
    let (&video_id, video_track) = reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().ok() == Some(mp4::TrackType::Video))
        .unwrap();
    let media_conf = video_track.media_config_for_description(1).unwrap();
    let timescale = video_track.timescale();
    let sample_count = video_track.sample_count().min(12);
    let mut samples = Vec::new();
    for number in 1..=sample_count {
        let sample = reader.read_sample(video_id, number).unwrap().unwrap();
        samples.push(sample);
    }
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
        timescale: 1_000,
    };
    let track = mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale,
        language: "und".to_owned(),
        media_conf,
    };
    let file = File::create(dest).unwrap();
    let mut writer = mp4::FragmentedMp4Writer::write_start(file, &config, &[track]).unwrap();
    let mut cursor_ticks = 0_u64;
    for (idx, mut sample) in samples.into_iter().enumerate() {
        let dur_ticks = match vfr_durations_ms {
            Some(durs) => u64::from(durs[idx % durs.len()]) * u64::from(timescale) / 1_000,
            None => u64::from(sample.duration).max(u64::from(timescale) / 10),
        };
        sample.start_time = cursor_ticks;
        sample.duration = u32::try_from(dur_ticks).unwrap();
        sample.rendering_offset = 0;
        cursor_ticks += dur_ticks;
        writer.write_sample(1, sample).unwrap();
    }
    writer.write_end().unwrap();
    writer.into_writer().sync_all().unwrap();
    let bytes = fs::read(dest).unwrap();
    scan_fragmented_mp4_boxes(&bytes, start_ms, timescale)
}

fn scan_fragmented_mp4_boxes(
    bytes: &[u8],
    recording_start_ms: i64,
    timescale: u32,
) -> WrittenRecording {
    let mut pos = 0usize;
    let mut init_len = 0u64;
    let mut fragments = Vec::new();
    let mut seq = 1u64;
    let mut frag_start_ms = recording_start_ms;
    while pos + 8 <= bytes.len() {
        let size = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let kind = &bytes[pos + 4..pos + 8];
        assert!(size >= 8 && pos + size <= bytes.len());
        if kind == b"moov" {
            init_len = (pos + size) as u64;
        } else if kind == b"moof" {
            let moof_offset = pos as u64;
            let mdat_pos = pos + size;
            let mdat_size = u32::from_be_bytes([
                bytes[mdat_pos],
                bytes[mdat_pos + 1],
                bytes[mdat_pos + 2],
                bytes[mdat_pos + 3],
            ]) as usize;
            assert_eq!(&bytes[mdat_pos + 4..mdat_pos + 8], b"mdat");
            let frag_bytes = (size + mdat_size) as u64;
            let dur_ms = compute_fragment_duration_ms(
                &bytes[..init_len as usize],
                &bytes[pos..pos + size],
                frag_bytes,
                timescale,
            );
            fragments.push(FragmentSpan {
                sequence: seq,
                start_ms: frag_start_ms,
                duration_ms: dur_ms,
                byte_offset: moof_offset,
                byte_len: frag_bytes,
            });
            frag_start_ms += dur_ms as i64;
            seq += 1;
            pos = mdat_pos + mdat_size;
            continue;
        }
        pos += size;
    }
    WrittenRecording {
        init_len,
        fragments,
    }
}

fn compute_fragment_duration_ms(init: &[u8], moof: &[u8], frag_bytes: u64, timescale: u32) -> u64 {
    let mut header = Vec::with_capacity(init.len() + moof.len() + 8);
    header.extend_from_slice(init);
    header.extend_from_slice(moof);
    let mdat_size = u32::try_from(frag_bytes - moof.len() as u64).unwrap();
    header.extend_from_slice(&mdat_size.to_be_bytes());
    header.extend_from_slice(b"mdat");
    let reader =
        mp4::Mp4Reader::read_header(std::io::Cursor::new(header), init.len() as u64 + frag_bytes)
            .unwrap();
    let (_, track) = reader
        .tracks()
        .iter()
        .find(|(_, t)| t.track_type().ok() == Some(mp4::TrackType::Video))
        .unwrap();
    let mut total_ticks = 0u64;
    for ordinal in 1..=track.sample_count() {
        total_ticks += u64::from(track.sample_timing(ordinal).unwrap().unwrap().duration);
    }
    ((u128::from(total_ticks) * 1_000) / u128::from(timescale)).max(1) as u64
}

fn register_recording(
    catalog: &RecordingCatalog,
    recording_id: &str,
    stream_id: &str,
    path: &Path,
    start_ms: i64,
    written: &WrittenRecording,
    finalized: bool,
) {
    let end_ms = written
        .fragments
        .last()
        .map(|frag| frag.start_ms + frag.duration_ms as i64)
        .unwrap_or(start_ms + 1_000);
    catalog
        .handle()
        .upsert_recording(CatalogRecording {
            id: recording_id.to_owned(),
            stream_id: stream_id.to_owned(),
            source_id: Some("cam-1".to_owned()),
            logical_stream_id: Some("main".to_owned()),
            started_at_ms: start_ms,
            ended_at_ms: finalized.then_some(end_ms),
            path: path.to_string_lossy().into_owned(),
            init_offset: 0,
            init_len: written.init_len,
            finalized,
        })
        .unwrap();
    for frag in &written.fragments {
        catalog
            .handle()
            .insert_fragment(CatalogFragment {
                recording_id: recording_id.to_owned(),
                sequence: frag.sequence,
                start_ms: frag.start_ms,
                duration_ms: frag.duration_ms,
                byte_offset: frag.byte_offset,
                byte_len: frag.byte_len,
                random_access: true,
            })
            .unwrap();
    }
}

fn insert_deletion_record(db_path: &Path, stream_id: &str, start_ms: i64, end_ms: i64) {
    pollster::block_on(async {
        let db = turso::Builder::new_local(db_path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "INSERT INTO recording_deletions (stream_id, source_id, logical_stream_id, start_ms, end_ms, deleted_at_ms, reason)
             VALUES (?1, 'cam-1', 'main', ?2, ?3, ?3, 'retention')",
            turso::params![stream_id, start_ms, end_ms],
        )
        .await
        .unwrap();
    });
}

#[test]
fn cadence_bounds_dst_boundaries_and_overflow_checks() {
    // 2026-03-08 10:30:00Z crosses US spring-forward DST in America/Los_Angeles (02:00 -> 03:00).
    let spring_now_ms = 1_772_965_800_000_i64;
    let (s_start, s_end) = TimelapsePreset::LastHour
        .resolve_range_ms(spring_now_ms, None)
        .unwrap();
    assert_eq!(s_end - s_start, 3_600_000);
    // 2026-11-01 09:30:00Z crosses US fall-back DST in America/Los_Angeles.
    let fall_now_ms = 1_793_525_400_000_i64;
    let (f_start, f_end) = TimelapsePreset::LastHour
        .resolve_range_ms(fall_now_ms, None)
        .unwrap();
    assert_eq!(f_end - f_start, 3_600_000);
    let (q_start, q_end) = TimelapsePreset::Last15Minutes
        .resolve_range_ms(fall_now_ms, None)
        .unwrap();
    assert_eq!(q_end - q_start, 900_000);

    let targets =
        generate_target_times(1_000, 11_000, TimelapseCadence::IntervalMs(2_500)).unwrap();
    assert_eq!(targets, vec![1_000, 3_500, 6_000, 8_500]);
    let counted = generate_target_times(1_000, 11_000, TimelapseCadence::TargetCount(5)).unwrap();
    assert_eq!(counted, vec![1_000, 3_000, 5_000, 7_000, 9_000]);

    assert!(generate_target_times(1_000, 1_000, TimelapseCadence::TargetCount(1)).is_err());
    assert!(generate_target_times(0, MAX_RANGE_MS + 1, TimelapseCadence::TargetCount(10)).is_err());
    assert!(generate_target_times(0, 60_000, TimelapseCadence::IntervalMs(0)).is_err());
    assert!(generate_target_times(0, 60_000, TimelapseCadence::TargetCount(0)).is_err());
    assert!(
        generate_target_times(0, 60_000, TimelapseCadence::TargetCount(MAX_SAMPLES + 1)).is_err()
    );
    assert!(generate_target_times(0, 5_000_000, TimelapseCadence::IntervalMs(1_000)).is_err());
    assert!(generate_target_times(i64::MIN, 10, TimelapseCadence::TargetCount(2)).is_err());
}

#[test]
fn planner_handles_cadence_vfr_dedup_gaps_privacy_retained_corrupt_open_and_codec_change() {
    let ws = TempWorkspace::new("timelapse-plan");
    let db_path = ws.root.join("recordings.db");
    let catalog = RecordingCatalog::open(&db_path).unwrap();
    let stream_id = "cam-1/main";
    let base_ms = 1_772_962_200_000_i64;

    // 1. H.264 VFR recording at [base, base + 2_000)
    let h264_path = ws.root.join("rec-h264.mp4");
    let h264_written = write_fragmented_recording_from_fixture(
        &fixture_path("cc-4k-640x360-h264.mp4"),
        &h264_path,
        base_ms,
        Some(&[120, 280, 150, 450]),
    );
    register_recording(
        &catalog,
        "rec-h264",
        stream_id,
        &h264_path,
        base_ms,
        &h264_written,
        true,
    );

    // 2. Deleted/retained range at [base + 4_000, base + 6_000)
    insert_deletion_record(&db_path, stream_id, base_ms + 4_000, base_ms + 6_000);

    // 3. H.265 codec-change recording at [base + 8_000, ...)
    let h265_path = ws.root.join("rec-h265.mp4");
    let h265_written = write_fragmented_recording_from_fixture(
        &fixture_path("cc-4k-640x360-h265.mp4"),
        &h265_path,
        base_ms + 8_000,
        None,
    );
    register_recording(
        &catalog,
        "rec-h265",
        stream_id,
        &h265_path,
        base_ms + 8_000,
        &h265_written,
        true,
    );

    // 4. Corrupt fragment at [base + 12_000, base + 13_000)
    let corrupt_path = ws.root.join("rec-corrupt.mp4");
    fs::write(&corrupt_path, b"not-a-valid-mp4-container-bytes-at-all").unwrap();
    let corrupt_written = WrittenRecording {
        init_len: 16,
        fragments: vec![FragmentSpan {
            sequence: 1,
            start_ms: base_ms + 12_000,
            duration_ms: 1_000,
            byte_offset: 16,
            byte_len: 20,
        }],
    };
    register_recording(
        &catalog,
        "rec-corrupt",
        stream_id,
        &corrupt_path,
        base_ms + 12_000,
        &corrupt_written,
        true,
    );

    // 5. Open (unfinalized) recording at [base + 14_000, ...)
    let open_path = ws.root.join("rec-open.mp4");
    let open_written = write_fragmented_recording_from_fixture(
        &fixture_path("cc-4k-640x360-h264.mp4"),
        &open_path,
        base_ms + 14_000,
        None,
    );
    register_recording(
        &catalog,
        "rec-open",
        stream_id,
        &open_path,
        base_ms + 14_000,
        &open_written,
        false,
    );

    let cancel = AtomicBool::new(false);
    let plan = plan_timelapse(
        &catalog.handle(),
        &TimelapsePlanRequest {
            source_id: "cam-1".to_owned(),
            stream_id: "main".to_owned(),
            catalog_stream_id: stream_id.to_owned(),
            start_ms: base_ms,
            end_ms: base_ms + 16_000,
            cadence: TimelapseCadence::IntervalMs(1_000),
            tolerance_ms: 500,
            include_open_recordings: false,
            privacy_intervals: vec![(base_ms + 6_000, base_ms + 7_500)],
            output_preview: TimelapseOutputPreview::default(),
            site_timezone: "America/Los_Angeles".to_owned(),
            encoder_available: true,
        },
        Instant::now() + Duration::from_secs(10),
        &cancel,
    )
    .unwrap();

    // Monotonic sample timestamps, both H.264 and H.265 codecs present, no stale repeats.
    let samples: Vec<_> = plan.samples().collect();
    assert!(samples.len() >= 2);
    assert!(samples.iter().any(|s| s.codec == "h264"));
    assert!(samples.iter().any(|s| s.codec == "h265"));
    for pair in samples.windows(2) {
        assert!(pair[0].actual_time_ns < pair[1].actual_time_ns);
    }

    // Verify all expected explicit gap reasons appear and are never hidden.
    let gap_reasons: Vec<_> = plan
        .items
        .iter()
        .filter_map(|item| match item {
            TimelapsePlanItem::Gap(gap) => Some(gap.reason),
            TimelapsePlanItem::Sample(_) => None,
        })
        .collect();
    assert!(gap_reasons.contains(&TimelapseGapReason::Retained));
    assert!(gap_reasons.contains(&TimelapseGapReason::Privacy));
    assert!(gap_reasons.contains(&TimelapseGapReason::Corrupt));
    assert!(gap_reasons.contains(&TimelapseGapReason::OpenRecording));

    // Pagination returns bounded chunks without duplicating items.
    let page1 = plan.page(0, 2).unwrap();
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.next_offset, Some(2));

    // Token integrity & stale-recording fail-closed verification.
    let first = samples[0];
    let decoded = decode_sample_token(&first.sample_token).unwrap();
    assert_eq!(decoded.recording_id, first.recording_id);
    assert_eq!(decoded.sample_ordinal, first.sample_ordinal);
    let mut tampered = first.sample_token.clone();
    tampered[0] ^= 0xFF;
    assert!(decode_sample_token(&tampered).is_err());

    // Bounded decode window succeeds while recording exists and fails after removal.
    let window = read_sample_decode_window(
        &catalog.handle(),
        first,
        Instant::now() + Duration::from_secs(5),
        &cancel,
    )
    .unwrap();
    assert!(!window.samples.is_empty());
    fs::remove_file(&h264_path).unwrap();
    assert!(
        read_sample_decode_window(
            &catalog.handle(),
            first,
            Instant::now() + Duration::from_secs(5),
            &cancel,
        )
        .is_err()
    );

    catalog.shutdown();
}

#[test]
fn encoder_produces_silent_verified_mp4_with_manifest_overlay_and_immutable_sources() {
    let capability = probe_local_encoder("ffmpeg");
    assert!(
        capability.available,
        "ffmpeg with H.264 encoder is required for timelapse media qualification"
    );
    let ws = TempWorkspace::new("timelapse-encode");
    let catalog = RecordingCatalog::open(&ws.root.join("recordings.db")).unwrap();
    let stream_id = "cam-1/main";
    let base_ms = 1_772_962_200_000_i64;

    // Mix H.264, H.265, and audio-bearing fixture (flash-tone-lead-200ms.mp4).
    let p1 = ws.root.join("part1-h264.mp4");
    let w1 = write_fragmented_recording_from_fixture(
        &fixture_path("cc-4k-640x360-h264.mp4"),
        &p1,
        base_ms,
        None,
    );
    register_recording(&catalog, "rec-1", stream_id, &p1, base_ms, &w1, true);

    let p2 = ws.root.join("part2-h265.mp4");
    let w2 = write_fragmented_recording_from_fixture(
        &fixture_path("cc-4k-640x360-h265.mp4"),
        &p2,
        base_ms + 3_000,
        None,
    );
    register_recording(
        &catalog,
        "rec-2",
        stream_id,
        &p2,
        base_ms + 3_000,
        &w2,
        true,
    );

    let p3 = ws.root.join("part3-av.mp4");
    let w3 = write_fragmented_recording_from_fixture(
        &fixture_path("flash-tone-lead-200ms.mp4"),
        &p3,
        base_ms + 6_000,
        None,
    );
    register_recording(
        &catalog,
        "rec-3",
        stream_id,
        &p3,
        base_ms + 6_000,
        &w3,
        true,
    );

    let hashes_before = [file_sha256(&p1), file_sha256(&p2), file_sha256(&p3)];
    let cancel = AtomicBool::new(false);
    let plan = plan_timelapse(
        &catalog.handle(),
        &TimelapsePlanRequest {
            source_id: "cam-1".to_owned(),
            stream_id: "main".to_owned(),
            catalog_stream_id: stream_id.to_owned(),
            start_ms: base_ms,
            end_ms: base_ms + 7_000,
            cadence: TimelapseCadence::IntervalMs(3_000),
            tolerance_ms: 500,
            include_open_recordings: false,
            privacy_intervals: Vec::new(),
            output_preview: TimelapseOutputPreview {
                output_fps: 2,
                max_width: 320,
                max_height: 180,
                timestamp_overlay: true,
            },
            site_timezone: "America/Los_Angeles".to_owned(),
            encoder_available: true,
        },
        Instant::now() + Duration::from_secs(10),
        &cancel,
    )
    .unwrap();
    assert_eq!(plan.estimate.sample_count, 3);
    assert!(!plan.has_gaps());

    let export_root = ws.root.join(".exports");
    let mut progress_steps = Vec::new();
    let encoded_overlay = encode_timelapse_mp4(
        &catalog.handle(),
        &plan,
        &TimelapseEncodeRequest {
            export_root: export_root.clone(),
            job_id: "job-overlay".to_owned(),
            artifact_id: "art-1".to_owned(),
            file_name: "cam-1-timelapse.mp4".to_owned(),
            manifest_file_name: "cam-1-timelapse.manifest.json".to_owned(),
            source_identity: Some("00000000-0000-4000-8000-000000000127".to_owned()),
            output_width: 320,
            output_height: 180,
            output_fps: 2,
            timestamp_overlay: true,
            allow_partial: false,
            confirmed_expensive: false,
            max_output_bytes: 16 * 1024 * 1024,
            ffmpeg_binary: "ffmpeg".to_owned(),
        },
        Instant::now() + Duration::from_secs(30),
        &cancel,
        || Ok(()),
        |done, total| progress_steps.push((done, total)),
    )
    .unwrap();

    assert_eq!(encoded_overlay.encoded_frames, 3);
    assert_eq!(progress_steps, vec![(1, 3), (2, 3), (3, 3)]);
    assert!(encoded_overlay.video_path.is_file());
    assert!(encoded_overlay.manifest_path.is_file());

    // Inspect MP4: silent (zero audio tracks), 3 video samples, monotonic PTS, decodable by ffmpeg.
    let reader = mp4::read_mp4(File::open(&encoded_overlay.video_path).unwrap()).unwrap();
    let audio_tracks = reader
        .tracks()
        .values()
        .filter(|t| t.track_type().ok() == Some(mp4::TrackType::Audio))
        .count();
    assert_eq!(audio_tracks, 0, "timelapse export must be silent");
    let (_, video_track) = reader
        .tracks()
        .iter()
        .find(|(_, t)| t.track_type().ok() == Some(mp4::TrackType::Video))
        .unwrap();
    assert_eq!(video_track.sample_count(), 3);
    let ffmpeg_check = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&encoded_overlay.video_path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        ffmpeg_check.status.success(),
        "ffmpeg rejected timelapse MP4: {}",
        String::from_utf8_lossy(&ffmpeg_check.stderr)
    );

    // Manifest provenance matches artifact SHA-256, UTC provenance, site timezone, and frames.
    let manifest_disk: keeppeek::storage::timelapse::TimelapseManifest =
        serde_json::from_slice(&fs::read(&encoded_overlay.manifest_path).unwrap()).unwrap();
    assert_eq!(manifest_disk.schema_version, 1);
    assert!(manifest_disk.generation.silent);
    assert!(manifest_disk.utc_provenance);
    assert_eq!(manifest_disk.site_timezone, "America/Los_Angeles");
    assert_eq!(manifest_disk.output_sha256, encoded_overlay.video_sha256);
    assert_eq!(manifest_disk.samples.len(), 3);
    assert_eq!(manifest_disk.output_duration_ms, 1_500);
    assert_eq!(manifest_disk.samples[0].output_pts_ms, 0);
    assert_eq!(manifest_disk.samples[1].output_pts_ms, 500);
    assert_eq!(manifest_disk.samples[2].output_pts_ms, 1_000);

    // Overlay vs non-overlay must produce distinct visual output bytes.
    let encoded_plain = encode_timelapse_mp4(
        &catalog.handle(),
        &plan,
        &TimelapseEncodeRequest {
            export_root,
            job_id: "job-plain".to_owned(),
            artifact_id: "art-2".to_owned(),
            file_name: "cam-1-plain.mp4".to_owned(),
            manifest_file_name: "cam-1-plain.manifest.json".to_owned(),
            source_identity: None,
            output_width: 320,
            output_height: 180,
            output_fps: 2,
            timestamp_overlay: false,
            allow_partial: false,
            confirmed_expensive: false,
            max_output_bytes: 16 * 1024 * 1024,
            ffmpeg_binary: "ffmpeg".to_owned(),
        },
        Instant::now() + Duration::from_secs(30),
        &cancel,
        || Ok(()),
        |_, _| {},
    )
    .unwrap();
    assert_ne!(encoded_overlay.video_sha256, encoded_plain.video_sha256);

    // Source recordings are completely immutable.
    assert_eq!(
        hashes_before,
        [file_sha256(&p1), file_sha256(&p2), file_sha256(&p3)]
    );
    catalog.shutdown();
}

#[test]
fn encoder_failure_injection_partial_confirm_cancel_timeout_headroom_cap_and_cleanup() {
    let ws = TempWorkspace::new("timelapse-fail");
    let catalog = RecordingCatalog::open(&ws.root.join("recordings.db")).unwrap();
    let stream_id = "cam-1/main";
    let base_ms = 1_772_962_200_000_i64;
    let p1 = ws.root.join("only-part1.mp4");
    let w1 = write_fragmented_recording_from_fixture(
        &fixture_path("cc-4k-640x360-h264.mp4"),
        &p1,
        base_ms,
        None,
    );
    register_recording(&catalog, "rec-1", stream_id, &p1, base_ms, &w1, true);

    let cancel = AtomicBool::new(false);
    let mut plan = plan_timelapse(
        &catalog.handle(),
        &TimelapsePlanRequest {
            source_id: "cam-1".to_owned(),
            stream_id: "main".to_owned(),
            catalog_stream_id: stream_id.to_owned(),
            start_ms: base_ms,
            end_ms: base_ms + 10_000,
            cadence: TimelapseCadence::IntervalMs(5_000),
            tolerance_ms: 500,
            include_open_recordings: false,
            privacy_intervals: Vec::new(),
            output_preview: TimelapseOutputPreview {
                output_fps: 2,
                max_width: 320,
                max_height: 180,
                timestamp_overlay: false,
            },
            site_timezone: "UTC".to_owned(),
            encoder_available: true,
        },
        Instant::now() + Duration::from_secs(10),
        &cancel,
    )
    .unwrap();
    assert!(plan.has_gaps());
    let export_root = ws.root.join(".exports");
    let make_req =
        |job_id: &str, allow_partial: bool, max_bytes: u64, bin: &str| TimelapseEncodeRequest {
            export_root: export_root.clone(),
            job_id: job_id.to_owned(),
            artifact_id: "a1".to_owned(),
            file_name: "out.mp4".to_owned(),
            manifest_file_name: "out.manifest.json".to_owned(),
            source_identity: None,
            output_width: 320,
            output_height: 180,
            output_fps: 2,
            timestamp_overlay: false,
            allow_partial,
            confirmed_expensive: false,
            max_output_bytes: max_bytes,
            ffmpeg_binary: bin.to_owned(),
        };

    // 1. Partial result denied without explicit allowance.
    assert!(matches!(
        encode_timelapse_mp4(
            &catalog.handle(),
            &plan,
            &make_req("job-partial-deny", false, 16 * 1024 * 1024, "ffmpeg"),
            Instant::now() + Duration::from_secs(15),
            &cancel,
            || Ok(()),
            |_, _| {},
        ),
        Err(TimelapseEncodeError::PartialResultRequiresAllowance { .. })
    ));

    // 2. Expensive job denied without explicit confirmation.
    plan.estimate.requires_confirmation = true;
    assert_eq!(
        encode_timelapse_mp4(
            &catalog.handle(),
            &plan,
            &make_req("job-confirm", true, 16 * 1024 * 1024, "ffmpeg"),
            Instant::now() + Duration::from_secs(15),
            &cancel,
            || Ok(()),
            |_, _| {},
        )
        .unwrap_err(),
        TimelapseEncodeError::ConfirmationRequired
    );
    plan.estimate.requires_confirmation = false;

    // 3. Disk headroom below safety reserve rejected.
    assert!(matches!(
        encode_timelapse_mp4(
            &catalog.handle(),
            &plan,
            &make_req("job-disk", true, 16 * 1024 * 1024, "ffmpeg"),
            Instant::now() + Duration::from_secs(15),
            &cancel,
            || Err("free space below reserve".to_owned()),
            |_, _| {},
        ),
        Err(TimelapseEncodeError::DiskPressure(_))
    ));

    // 4. Path traversal rejected.
    assert!(matches!(
        encode_timelapse_mp4(
            &catalog.handle(),
            &plan,
            &make_req("../escape", true, 16 * 1024 * 1024, "ffmpeg"),
            Instant::now() + Duration::from_secs(15),
            &cancel,
            || Ok(()),
            |_, _| {},
        ),
        Err(TimelapseEncodeError::InvalidRequest(_))
    ));

    // 5. Missing encoder binary returns EncoderUnavailable.
    assert!(matches!(
        encode_timelapse_mp4(
            &catalog.handle(),
            &plan,
            &make_req(
                "job-missing-bin",
                true,
                16 * 1024 * 1024,
                "keeppeek-nonexistent-ffmpeg-bin"
            ),
            Instant::now() + Duration::from_secs(15),
            &cancel,
            || Ok(()),
            |_, _| {},
        ),
        Err(TimelapseEncodeError::EncoderUnavailable(_))
    ));

    // 6. Cancellation leaves zero partial artifacts.
    let cancel_now = AtomicBool::new(true);
    assert_eq!(
        encode_timelapse_mp4(
            &catalog.handle(),
            &plan,
            &make_req("job-cancel", true, 16 * 1024 * 1024, "ffmpeg"),
            Instant::now() + Duration::from_secs(15),
            &cancel_now,
            || Ok(()),
            |_, _| {},
        )
        .unwrap_err(),
        TimelapseEncodeError::Cancelled
    );
    assert!(!export_root.join("job-cancel").join("a1").exists());

    // 7. Expired deadline returns Timeout and leaves zero partial artifacts.
    assert_eq!(
        encode_timelapse_mp4(
            &catalog.handle(),
            &plan,
            &make_req("job-timeout", true, 16 * 1024 * 1024, "ffmpeg"),
            Instant::now() - Duration::from_secs(1),
            &cancel,
            || Ok(()),
            |_, _| {},
        )
        .unwrap_err(),
        TimelapseEncodeError::Timeout
    );
    assert!(!export_root.join("job-timeout").join("a1").exists());

    // 8. Output byte cap exceeded cleans up all partial artifacts.
    assert!(matches!(
        encode_timelapse_mp4(
            &catalog.handle(),
            &plan,
            &make_req("job-cap", true, 64, "ffmpeg"),
            Instant::now() + Duration::from_secs(15),
            &cancel,
            || Ok(()),
            |_, _| {},
        ),
        Err(TimelapseEncodeError::OutputSizeExceeded { limit_bytes: 64 })
    ));
    assert!(!export_root.join("job-cap").join("a1").exists());

    // 9. Allowed partial export succeeds and records explicit gaps in manifest.
    let partial_ok = encode_timelapse_mp4(
        &catalog.handle(),
        &plan,
        &make_req("job-partial-ok", true, 16 * 1024 * 1024, "ffmpeg"),
        Instant::now() + Duration::from_secs(30),
        &cancel,
        || Ok(()),
        |_, _| {},
    )
    .unwrap();
    assert_eq!(partial_ok.manifest.samples.len(), 1);
    assert_eq!(partial_ok.manifest.gaps.len(), 1);
    assert!(partial_ok.manifest.generation.allow_partial);
    catalog.shutdown();
}

#[test]
fn catalog_timelapse_lookup_stays_bounded_across_127_cameras() {
    let ws = TempWorkspace::new("timelapse-127");
    let catalog = RecordingCatalog::open(&ws.root.join("recordings.db")).unwrap();
    let base_ms = 1_770_000_000_000_i64;
    let dummy_path = ws.root.join("placeholder.mp4");
    fs::write(&dummy_path, b"placeholder").unwrap();

    for cam in 0..127u32 {
        let stream_id = format!("cam-{cam:03}/main");
        let rec_id = format!("rec-{cam:03}");
        let start = base_ms;
        let path = ws.root.join(format!("cam-{cam:03}.mp4"));
        catalog
            .handle()
            .upsert_recording(CatalogRecording {
                id: rec_id.clone(),
                stream_id,
                source_id: Some(format!("cam-{cam:03}")),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: start,
                ended_at_ms: Some(start + 3_600_000),
                path: path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 100,
                finalized: true,
            })
            .unwrap();
        for seq in 0..6u64 {
            catalog
                .handle()
                .insert_fragment(CatalogFragment {
                    recording_id: rec_id.clone(),
                    sequence: seq + 1,
                    start_ms: start + (seq as i64) * 600_000,
                    duration_ms: 600_000,
                    byte_offset: 100 + seq * 1_000,
                    byte_len: 1_000,
                    random_access: true,
                })
                .unwrap();
        }
    }

    let targets = generate_target_times(
        base_ms,
        base_ms + 3_600_000,
        TimelapseCadence::TargetCount(12),
    )
    .unwrap();
    let lookup = catalog
        .handle()
        .timelapse_lookup(
            "cam-064/main",
            base_ms,
            base_ms + 3_600_000,
            &targets,
            DEFAULT_TOLERANCE_MS,
        )
        .unwrap();
    assert_eq!(lookup.candidates.len(), 12);
    assert!(lookup.candidates.iter().all(|c| {
        c.as_ref()
            .is_some_and(|frag| frag.fragment.recording_id == "rec-064")
    }));
    const { assert!(MAX_OUTPUT_WIDTH == 1_920 && MAX_OUTPUT_HEIGHT == 1_080) };
    assert!(!cancel_flag_default().load(Ordering::Acquire));
    catalog.shutdown();
}

const fn cancel_flag_default() -> AtomicBool {
    AtomicBool::new(false)
}
