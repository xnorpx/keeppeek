use super::*;
use crate::storage::{
    CatalogFragment, CatalogRecording, RecordingCatalog, timelapse::TimelapseManifest,
};

fn fixture_h264() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/test-camera/testdata/cc-4k-640x360-h264.mp4")
}

fn seed_fragmented_h264(root: &Path, dest: &Path, start_ms: i64) -> (u64, u64, u64, u64) {
    let _ = root;
    let mut reader = mp4::read_mp4(File::open(fixture_h264()).unwrap()).unwrap();
    let (&video_id, track) = reader
        .tracks()
        .iter()
        .find(|(_, t)| t.track_type().ok() == Some(mp4::TrackType::Video))
        .unwrap();
    let media_conf = track.media_config_for_description(1).unwrap();
    let timescale = track.timescale();
    let count = track.sample_count().min(8);
    let mut samples = Vec::new();
    for n in 1..=count {
        samples.push(reader.read_sample(video_id, n).unwrap().unwrap());
    }
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
        timescale: 1_000,
    };
    let track_cfg = mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale,
        language: "und".to_owned(),
        media_conf,
    };
    let file = File::create(dest).unwrap();
    let mut writer = mp4::FragmentedMp4Writer::write_start(file, &config, &[track_cfg]).unwrap();
    let mut cursor = 0_u64;
    let step = u64::from(timescale) / 4;
    for mut s in samples {
        s.start_time = cursor;
        s.duration = u32::try_from(step).unwrap();
        s.rendering_offset = 0;
        cursor += step;
        writer.write_sample(1, s).unwrap();
    }
    writer.write_end().unwrap();
    writer.into_writer().sync_all().unwrap();
    let bytes = std::fs::read(dest).unwrap();
    let mut pos = 0usize;
    let mut init_len = 0u64;
    let mut moof_offset = 0u64;
    let mut frag_bytes = 0u64;
    while pos + 8 <= bytes.len() {
        let size = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let kind = &bytes[pos + 4..pos + 8];
        if kind == b"moov" {
            init_len = (pos + size) as u64;
        } else if kind == b"moof" {
            moof_offset = pos as u64;
            let mdat_pos = pos + size;
            let mdat_size = u32::from_be_bytes([
                bytes[mdat_pos],
                bytes[mdat_pos + 1],
                bytes[mdat_pos + 2],
                bytes[mdat_pos + 3],
            ]) as usize;
            frag_bytes = (size + mdat_size) as u64;
            break;
        }
        pos += size;
    }
    let dur_ms = (u128::from(cursor) * 1_000 / u128::from(timescale)) as u64;
    let _ = start_ms;
    (init_len, moof_offset, frag_bytes, dur_ms)
}

fn setup_timelapse_state(base_ms: i64) -> (PathBuf, RecordingCatalog, ServerState) {
    let root = std::env::temp_dir().join(format!("keeppeek-srv-tl-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
    let mut state = super::tests::media_test_state();
    state.storage_config.long_term_path = root.clone();
    state.export_history_path = Some(Arc::new(root.join(".exports/history.json")));
    let cam = state.camera("127.0.0.1").unwrap();
    let stream_id = recording_stream_id(&cam, "main");
    let rec_path = root.join("cam-h264.mp4");
    let (init_len, moof_off, frag_len, dur_ms) = seed_fragmented_h264(&root, &rec_path, base_ms);
    catalog
        .handle()
        .upsert_recording(CatalogRecording {
            id: "rec-tl-1".to_owned(),
            stream_id,
            source_id: Some("127.0.0.1".to_owned()),
            logical_stream_id: Some("main".to_owned()),
            started_at_ms: base_ms,
            ended_at_ms: Some(base_ms + dur_ms as i64),
            path: rec_path.to_string_lossy().into_owned(),
            init_offset: 0,
            init_len,
            finalized: true,
        })
        .unwrap();
    catalog
        .handle()
        .insert_fragment(CatalogFragment {
            recording_id: "rec-tl-1".to_owned(),
            sequence: 1,
            start_ms: base_ms,
            duration_ms: dur_ms,
            byte_offset: moof_off,
            byte_len: frag_len,
            random_access: true,
        })
        .unwrap();
    let state = state.with_recording_catalog(catalog.handle());
    (root, catalog, state)
}

#[test]
fn server_timelapse_plan_fetch_cancel_and_privacy_enforcement() {
    let base_ms = 1_772_962_200_000_i64;
    let (root, catalog, state) = setup_timelapse_state(base_ms);
    let session = SessionId::from_u64(42);
    assert!(
        server_capability_ids(&state)
            .iter()
            .any(|id| id == timelapse::CAPABILITY_ID)
    );

    let dispatch = timelapse::handle_plan(
        &state,
        session,
        proto::PlanTimelapse {
            source_id: "127.0.0.1".to_owned(),
            stream_id: "main".to_owned(),
            start_time: Some(millis_timestamp(base_ms)),
            end_time: Some(millis_timestamp(base_ms + 1_500)),
            cadence: Some(proto::plan_timelapse::Cadence::Interval(
                prost_types::Duration {
                    seconds: 0,
                    nanos: 500_000_000,
                },
            )),
            tolerance: Some(prost_types::Duration {
                seconds: 1,
                nanos: 0,
            }),
            page_size: 2,
            page_token: String::new(),
            output_preview: None,
        },
    )
    .unwrap();
    let Some(proto::ok::Result::TimelapsePlan(plan)) = dispatch.result else {
        panic!("expected timelapse plan result");
    };
    assert!(plan.total_samples >= 2);
    assert!(!plan.items.is_empty());
    let first_token = plan
        .items
        .iter()
        .find_map(|item| match &item.entry {
            Some(proto::timelapse_plan_item::Entry::Sample(s)) => Some(s.sample_token.clone()),
            _ => None,
        })
        .unwrap();

    // Fetch valid token + tampered token together.
    let mut bad_token = first_token.clone();
    bad_token[0] ^= 0xAA;
    let fetched = timelapse::handle_fetch_samples(
        &state,
        session,
        proto::FetchTimelapseSamples {
            transfer_id: "tl-fetch-1".to_owned(),
            source_id: "127.0.0.1".to_owned(),
            stream_id: "main".to_owned(),
            sample_tokens: vec![first_token.clone(), bad_token],
            channel: proto::DataChannelKind::ReliableData as i32,
        },
    )
    .unwrap();
    let Some(proto::ok::Result::TimelapseSampleDelivery(delivery)) = fetched.result else {
        panic!("expected timelapse sample delivery");
    };
    assert_eq!(delivery.accepted_count, 1);
    assert_eq!(delivery.rejected.len(), 1);
    assert!(fetched.messages.len() >= 2);

    // Cancel succeeds cleanly.
    timelapse::handle_cancel_samples(
        &state,
        session,
        proto::CancelTimelapseSamples {
            transfer_id: "tl-fetch-1".to_owned(),
        },
    )
    .unwrap();

    // Activate privacy on camera -> plan reports Privacy gap and fetch is rejected.
    state
        .privacy
        .replace_profile_overrides(std::collections::BTreeMap::from([(
            "127.0.0.1".to_owned(),
            true,
        )]))
        .unwrap();
    let private_plan = timelapse::handle_plan(
        &state,
        session,
        proto::PlanTimelapse {
            source_id: "127.0.0.1".to_owned(),
            stream_id: "main".to_owned(),
            start_time: Some(millis_timestamp(base_ms)),
            end_time: Some(millis_timestamp(base_ms + 1_500)),
            cadence: Some(proto::plan_timelapse::Cadence::TargetSampleCount(2)),
            tolerance: None,
            page_size: 8,
            page_token: String::new(),
            output_preview: None,
        },
    )
    .unwrap();
    let Some(proto::ok::Result::TimelapsePlan(private_res)) = private_plan.result else {
        panic!("expected private plan result");
    };
    assert_eq!(private_res.total_samples, 0);
    assert!(private_res.items.iter().all(|item| matches!(
        item.entry,
        Some(proto::timelapse_plan_item::Entry::Gap(ref g))
            if g.reason == proto::TimelapseGapReason::Privacy as i32
    )));
    let Err(err) = timelapse::handle_fetch_samples(
        &state,
        session,
        proto::FetchTimelapseSamples {
            transfer_id: "tl-fetch-private".to_owned(),
            source_id: "127.0.0.1".to_owned(),
            stream_id: "main".to_owned(),
            sample_tokens: vec![first_token],
            channel: proto::DataChannelKind::ReliableData as i32,
        },
    ) else {
        panic!("expected privacy rejection");
    };
    assert_eq!(err.code, proto::ErrorCode::Rejected);

    drop(state);
    catalog.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn server_timelapse_export_lifecycle_manifest_download_dedup_and_recovery() {
    let base_ms = 1_772_962_200_000_i64;
    let (root, catalog, state) = setup_timelapse_state(base_ms);
    let options = proto::TimelapseExportOptions {
        cadence: Some(proto::timelapse_export_options::Cadence::TargetSampleCount(
            2,
        )),
        tolerance: Some(prost_types::Duration {
            seconds: 1,
            nanos: 0,
        }),
        output_fps: 2,
        max_width: 320,
        max_height: 180,
        timestamp_overlay: true,
        confirmed_expensive: false,
        expected_source_fingerprint: None,
    };
    let req = proto::CreateExportJob {
        job_id: "tl-export-1".to_owned(),
        source_id: "127.0.0.1".to_owned(),
        stream_id: "main".to_owned(),
        start_time: Some(millis_timestamp(base_ms)),
        end_time: Some(millis_timestamp(base_ms + 1_500)),
        allow_partial: true,
        burn_in_timestamp: false,
        event_seed: None,
        guest_range_preparation: None,
        timelapse: Some(options),
    };
    let created = create_export_job(&state, "owner", req).unwrap();
    assert_eq!(created.status, proto::ExportJobStatus::Running as i32);
    assert!(created.timelapse.as_ref().is_some_and(|s| s.silent));

    // Wait for worker completion.
    let deadline = Instant::now() + Duration::from_secs(30);
    let ready = loop {
        let current = export_job(&state, "owner", "tl-export-1").unwrap();
        if current.status != proto::ExportJobStatus::Running as i32 {
            break current;
        }
        assert!(Instant::now() < deadline, "timelapse export timed out");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(ready.status, proto::ExportJobStatus::Ready as i32);
    let summary = ready.timelapse.as_ref().unwrap();
    assert!(summary.silent);
    assert!(summary.manifest_sha256.is_some());

    // Download silent MP4 + download verified JSON manifest.
    let (mp4_res, mp4_msgs) = download_export(
        &state,
        "owner",
        proto::DownloadExport {
            job_id: "tl-export-1".to_owned(),
            channel: proto::DataChannelKind::ReliableData as i32,
            artifact: proto::ExportArtifactKind::Media as i32,
        },
    )
    .unwrap();
    assert_eq!(mp4_res.artifact, proto::ExportArtifactKind::Media as i32);
    assert!(!mp4_msgs.is_empty());

    let (man_res, man_msgs) = download_export(
        &state,
        "owner",
        proto::DownloadExport {
            job_id: "tl-export-1".to_owned(),
            channel: proto::DataChannelKind::ReliableData as i32,
            artifact: proto::ExportArtifactKind::Manifest as i32,
        },
    )
    .unwrap();
    assert_eq!(man_res.artifact, proto::ExportArtifactKind::Manifest as i32);
    let mut man_bytes = Vec::new();
    for msg in man_msgs {
        if let Some(proto::message::Message::Export(proto::ExportMessage {
            message: Some(proto::export_message::Message::FileChunk(chunk)),
        })) = msg.message.message
        {
            man_bytes.extend_from_slice(&chunk.payload);
        }
    }
    let parsed: TimelapseManifest = serde_json::from_slice(&man_bytes).unwrap();
    assert_eq!(parsed.schema_version, 1);
    assert!(parsed.generation.silent);
    assert_eq!(parsed.output_sha256, ready.sha256.clone().unwrap());

    // Existing 2-minute evidence export guard remains enforced when timelapse is None.
    let too_long = create_export_job(
        &state,
        "owner",
        proto::CreateExportJob {
            job_id: "evidence-too-long".to_owned(),
            source_id: "127.0.0.1".to_owned(),
            stream_id: "main".to_owned(),
            start_time: Some(millis_timestamp(base_ms)),
            end_time: Some(millis_timestamp(base_ms + 5 * 60_000)),
            allow_partial: true,
            burn_in_timestamp: false,
            event_seed: None,
            guest_range_preparation: None,
            timelapse: None,
        },
    )
    .unwrap_err();
    assert_eq!(too_long.code, proto::ErrorCode::InvalidRequest);

    // Restart recovery fails ready timelapse job if its companion manifest is missing.
    let history_bytes = export_history_bytes(&state.export_jobs.lock().unwrap()).unwrap();
    let export_root = root.join(".exports");
    let artifact_id = state.export_jobs.lock().unwrap()["tl-export-1"]
        .artifact_id
        .clone();
    let manifest_path = export_root
        .join("tl-export-1")
        .join(&artifact_id)
        .join(summary.manifest_file_name.as_deref().unwrap());
    std::fs::remove_file(&manifest_path).unwrap();
    let recovered =
        restore_export_history(&history_bytes, &export_root, Some(&catalog.handle())).unwrap();
    assert_eq!(
        recovered["tl-export-1"].job.status,
        proto::ExportJobStatus::Failed as i32
    );
    assert!(recovered["tl-export-1"].job.retryable);

    drop(state);
    catalog.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
