use super::*;
use crate::storage::{
    AudioCodec, AudioFrame, MediaFrame, RecordingCatalog, RecordingFrame, VideoCodec, VideoFrame,
    medium_term::MediumTermWriter,
    volumes::runtime::{Manager, tests::fixture},
};

struct Fixture {
    root: PathBuf,
    catalog: RecordingCatalog,
    manager: Manager,
    state: ServerState,
    request: proto::CreateExportJob,
    source: PathBuf,
}

impl Fixture {
    fn new(named: bool) -> Self {
        let (root, catalog, manager) = fixture(16 * 1024 * 1024).unwrap();
        let source = write_source(&root.join("recordings"), catalog.handle());
        let fragments = catalog
            .handle()
            .media_fragments_in_range("front-door/sub", 0, i64::MAX)
            .unwrap();
        let mut state = super::super::tests::media_test_state();
        state.catalog = Some(catalog.handle());
        state.storage_config.long_term_path = root.join("legacy");
        state.storage_config.volume_runtime = named.then(|| Arc::new(manager.clone()));
        set_offsets(&state, 900, 250);
        let first = fragments.first().unwrap();
        let last = fragments.last().unwrap();
        let request = proto::CreateExportJob {
            job_id: "audio-export".to_owned(),
            source_id: "127.0.0.1".to_owned(),
            stream_id: "main".to_owned(),
            start_time: Some(millis_timestamp(first.start_ms)),
            end_time: Some(millis_timestamp(
                last.start_ms + i64::try_from(last.duration_ms).unwrap(),
            )),
            ..Default::default()
        };
        Self {
            root,
            catalog,
            manager,
            state,
            request,
            source,
        }
    }

    fn completed(&self) -> proto::ExportJob {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let job = export_job(&self.state, "owner", &self.request.job_id).unwrap();
            if job.status != proto::ExportJobStatus::Running as i32 {
                return job;
            }
            assert!(Instant::now() < deadline, "audio export did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(self) {
        drop(self.state);
        drop(self.manager);
        self.catalog.shutdown();
    }
}

fn set_offsets(state: &ServerState, main: i32, sub: i32) {
    let mut cameras = state.cameras.write().unwrap();
    cameras[0].configuration.recorded_audio = crate::cameras::RecordedAudioSettings {
        main_offset_ms: Some(main),
        sub_offset_ms: Some(sub),
        ..Default::default()
    };
}

fn write_source(root: &Path, catalog: RecordingCatalogHandle) -> PathBuf {
    let started = Instant::now();
    let mut writer =
        MediumTermWriter::create_with_catalog(root, "front-door/sub", started, 8192, catalog)
            .unwrap();
    let keyframe =
        super::super::tests::fixture_video_keyframe("cc-4k-640x360-h264.mp4", mp4::MediaType::H264);
    for millis in [0, 100, 200, 1000] {
        let offset = Duration::from_millis(millis);
        let frame = if matches!(millis, 0 | 1000) {
            MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H264,
                is_keyframe: true,
                width: 640,
                height: 368,
                data: keyframe.clone(),
            })
        } else {
            // ponytail: Encoded markers qualify remux preservation; decoded calibration needs valid AAC fixtures.
            MediaFrame::Audio(AudioFrame {
                codec: AudioCodec::Aac,
                sample_rate: 16_000,
                duration: Duration::from_millis(20),
                data: bytes::Bytes::from_static(&[0x11, 0x22, 0x33]),
            })
        };
        writer
            .append_one(RecordingFrame {
                received_at: started + offset,
                timestamp: Some(offset),
                frame,
            })
            .unwrap();
    }
    writer.finalize().unwrap()
}

fn first_audio(bytes: Vec<u8>) -> mp4::Mp4Sample {
    let len = u64::try_from(bytes.len()).unwrap();
    let mut reader = mp4::Mp4Reader::read_header(Cursor::new(bytes), len).unwrap();
    let id = *reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().unwrap() == mp4::TrackType::Audio)
        .unwrap()
        .0;
    reader.read_sample(id, 1).unwrap().unwrap()
}

fn download(state: &ServerState, id: &str) -> Vec<u8> {
    let (_, messages) = download_export(
        state,
        "owner",
        proto::DownloadExport {
            job_id: id.to_owned(),
            channel: proto::DataChannelKind::ReliableData as i32,
        },
    )
    .unwrap();
    messages
        .into_iter()
        .flat_map(|message| {
            let Some(proto::message::Message::Export(export)) = message.message.message else {
                panic!("export message expected")
            };
            let Some(proto::export_message::Message::FileChunk(chunk)) = export.message else {
                panic!("export file chunk expected")
            };
            chunk.payload
        })
        .collect()
}

#[test]
fn export_neighbors_follow_the_captured_stream_and_resolve_both_boundary_directions() {
    let fixture = Fixture::new(false);
    let catalog = fixture.catalog.handle();
    let video = catalog
        .media_fragments_in_range("front-door/sub", 0, i64::MAX)
        .unwrap();
    assert_eq!(video.len(), 2);
    fixture.state.cameras.write().unwrap()[0].recording_label = "replacement".into();
    let mut attempt = Attempt {
        state: fixture.state.clone(),
        job: "neighbors".into(),
        artifact: "artifact".into(),
        cancel: Arc::new(AtomicBool::new(false)),
        file_name: "neighbors.mp4".into(),
        audio_offset_ms: 200,
        recording_stream_id: Some("front-door/sub".into()),
    };
    for (index, offset) in [(1, 200), (0, -200)] {
        attempt.audio_offset_ms = offset;
        let end = video[index].start_ms + i64::try_from(video[index].duration_ms).unwrap();
        let context =
            audio_context::load(&attempt, &video[index..index + 1], end, &catalog).unwrap();
        assert_eq!(context.fragments.len(), 2);
        assert!(
            video
                .iter()
                .all(|fragment| context.fragments.contains(fragment))
        );
    }
    attempt.cancel.store(true, Ordering::Release);
    assert!(audio_context::load(&attempt, &video[..1], i64::MAX, &catalog).is_err());
    attempt.cancel.store(false, Ordering::Release);
    attempt.recording_stream_id = None;
    assert!(audio_context::load(&attempt, &video[..1], i64::MAX, &catalog).is_err());
    attempt.audio_offset_ms = 0;
    assert!(
        audio_context::load(&attempt, &video[..1], i64::MAX, &catalog)
            .unwrap()
            .fragments
            .is_empty()
    );
    drop(attempt);
    fixture.finish();
}

#[test]
fn both_worker_paths_apply_the_actual_recorded_stream_snapshot_without_changing_source() {
    for named in [false, true] {
        let fixture = Fixture::new(named);
        let source = std::fs::read(&fixture.source).unwrap();
        let original = first_audio(source.clone());
        create_export_job(&fixture.state, "owner", fixture.request.clone()).unwrap();
        let ready = fixture.completed();
        assert_eq!(
            ready.status,
            proto::ExportJobStatus::Ready as i32,
            "{:?}",
            ready.error
        );
        let timing = ready.recorded_audio.as_ref().unwrap();
        assert_eq!(timing.applied_offset_ms, 250);
        assert_eq!(timing.audio_present, Some(true));
        let exported = first_audio(download(&fixture.state, &fixture.request.job_id));
        assert_eq!(exported.start_time, original.start_time + 4_000);
        assert_eq!(exported.bytes, original.bytes);
        assert_eq!(exported.rendering_offset, original.rendering_offset);
        assert_eq!(std::fs::read(&fixture.source).unwrap(), source);
        fixture.finish();
    }
}

#[test]
fn legacy_captured_none_exports_source_timing_and_does_not_invent_metadata() {
    let fixture = Fixture::new(false);
    let original = first_audio(std::fs::read(&fixture.source).unwrap());
    creation::create(
        &fixture.state,
        "owner",
        fixture.request.clone(),
        creation::TimingSource::Captured(None),
    )
    .unwrap();
    let ready = fixture.completed();
    assert_eq!(ready.status, proto::ExportJobStatus::Ready as i32);
    assert!(ready.recorded_audio.is_none());
    assert_eq!(
        first_audio(download(&fixture.state, &fixture.request.job_id)).start_time,
        original.start_time
    );
    fixture.finish();
}

#[test]
fn retry_after_restart_preserves_original_policy_and_replaces_only_the_attempt() {
    let mut fixture = Fixture::new(true);
    fixture.request.burn_in_timestamp = true;
    let original = create_export_job(&fixture.state, "owner", fixture.request.clone()).unwrap();
    let before = {
        let mut jobs = fixture.state.export_jobs.lock().unwrap();
        let record = jobs.get_mut(&fixture.request.job_id).unwrap();
        record.job.retryable = true;
        record.artifact_id.clone()
    };
    let path = fixture.root.join("history.json");
    persist_export_jobs(&path, &fixture.state.export_jobs.lock().unwrap()).unwrap();
    fixture.state.export_history_path = Some(Arc::new(path));
    let mut recovered = fixture.state.clone();
    history::restore(&mut recovered);
    assert!(recovered.export_history_error.is_none());
    set_offsets(&recovered, -400, -500);
    let retried = creation::retry(&recovered, "owner", &fixture.request.job_id).unwrap();
    assert_eq!(retried.recorded_audio, original.recorded_audio);
    let jobs = recovered.export_jobs.lock().unwrap();
    assert_ne!(jobs[&fixture.request.job_id].artifact_id, before);
    drop(jobs);
    drop(recovered);
    fixture.finish();
}

#[test]
fn retry_rejects_a_changed_recording_stream_before_cancelling_or_removing_the_attempt() {
    let mut fixture = Fixture::new(true);
    fixture.request.burn_in_timestamp = true;
    create_export_job(&fixture.state, "owner", fixture.request.clone()).unwrap();
    let original = {
        let mut jobs = fixture.state.export_jobs.lock().unwrap();
        let record = jobs.get_mut(&fixture.request.job_id).unwrap();
        record.job.retryable = true;
        record.clone()
    };
    assert_eq!(
        original.recording_stream_id.as_deref(),
        Some("front-door/sub")
    );
    fixture.state.cameras.write().unwrap()[0]
        .configuration
        .recording_mode = CameraRecordingMode::Both;
    let error = creation::retry(&fixture.state, "owner", &fixture.request.job_id).unwrap_err();
    assert_eq!(error._http_status, 409);
    assert!(error.message.contains("recording stream changed"));
    let jobs = fixture.state.export_jobs.lock().unwrap();
    let retained = &jobs[&fixture.request.job_id];
    assert_eq!(retained.artifact_id, original.artifact_id);
    assert_eq!(retained.job, original.job);
    assert!(Arc::ptr_eq(&retained.cancel, &original.cancel));
    assert!(!retained.cancel.load(Ordering::Acquire));
    drop(jobs);
    fixture.finish();
}

#[test]
fn captured_worker_timing_rejects_an_old_artifact_handle_cancel_or_changed_request() {
    let mut fixture = Fixture::new(false);
    fixture.request.burn_in_timestamp = true;
    create_export_job(&fixture.state, "owner", fixture.request.clone()).unwrap();
    let record = {
        let mut jobs = fixture.state.export_jobs.lock().unwrap();
        let record = jobs.get_mut(&fixture.request.job_id).unwrap();
        record.job.status = proto::ExportJobStatus::Running as i32;
        record.clone()
    };
    let capture = |cancel: &Arc<AtomicBool>, artifact: &str| {
        captured_policy(&fixture.state, &fixture.request, cancel, artifact)
    };
    let timing = capture(&record.cancel, &record.artifact_id).unwrap();
    assert_eq!(timing.stream_id.as_deref(), Some("front-door/sub"));
    assert_eq!(timing.offset_ms, 250);
    assert!(capture(&record.cancel, "old-artifact").is_err());
    assert!(capture(&Arc::new(AtomicBool::new(false)), &record.artifact_id).is_err());
    let mut changed = fixture.request.clone();
    changed.allow_partial = true;
    assert!(
        captured_policy(
            &fixture.state,
            &changed,
            &record.cancel,
            &record.artifact_id
        )
        .is_err()
    );
    record.cancel.store(true, Ordering::Release);
    assert!(capture(&record.cancel, &record.artifact_id).is_err());
    fixture.finish();
}

#[test]
fn invalid_persisted_timing_is_rejected_before_recovery_changes_history() {
    let mut fixture = Fixture::new(false);
    fixture.request.burn_in_timestamp = true;
    create_export_job(&fixture.state, "owner", fixture.request.clone()).unwrap();
    {
        let mut jobs = fixture.state.export_jobs.lock().unwrap();
        jobs.get_mut(&fixture.request.job_id)
            .unwrap()
            .job
            .recorded_audio
            .as_mut()
            .unwrap()
            .preview = true;
    }
    let path = fixture.root.join("invalid-history.json");
    persist_export_jobs(&path, &fixture.state.export_jobs.lock().unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    fixture.state.export_history_path = Some(Arc::new(path.clone()));
    let mut recovered = fixture.state.clone();
    history::restore(&mut recovered);
    assert!(recovered.export_history_error.is_some());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(recovered);
    fixture.finish();
}
