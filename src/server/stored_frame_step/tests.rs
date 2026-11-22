use super::*;
use crate::storage::archive::target::Work;

struct RecordingDirectory(std::path::PathBuf);

impl RecordingDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("keeppeek-frame-step-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for RecordingDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn recording(path: &Path, times: &[Option<i64>]) -> Vec<CatalogMediaFragment> {
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap()],
        timescale: 1_000,
    };
    let track = mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: 1_000,
        language: "und".into(),
        media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
            width: 320,
            height: 240,
            seq_param_set: vec![0x67, 0x42, 0, 0x1e, 0xe9, 1, 0x40, 0x7b, 0x20],
            pic_param_set: vec![0x68, 0xce, 6, 0xe2],
        }),
    };
    let mut writer = mp4::FragmentedMp4Writer::write_start(
        std::fs::File::create(path).unwrap(),
        &config,
        &[track],
    )
    .unwrap();
    let mut fragments = Vec::new();
    for (index, &stamp) in times.iter().enumerate() {
        let sync = index % 3 == 0;
        let sample = mp4::Mp4Sample {
            start_time: index as u64 * 40,
            duration: 40,
            rendering_offset: 0,
            is_sync: sync,
            bytes: bytes::Bytes::from(vec![
                0,
                0,
                0,
                2,
                if sync { 0x65 } else { 0x41 },
                index as u8,
            ]),
        };
        let packet = stamp.map(|timestamp| mp4::SourceRtpPacket {
            session_id: [1; 16],
            epoch_id: [2; 16],
            clock_rate: 1_000.try_into().unwrap(),
            timestamp,
        });
        writer
            .write_sample_with_source_timing(1, 1, sample, packet)
            .unwrap();
        if index % 3 == 2 {
            fragments.push(writer.flush_fragment().unwrap().unwrap());
        }
    }
    writer.write_end().unwrap();
    let init = fragments[0].range.offset;
    fragments
        .into_iter()
        .enumerate()
        .map(|(i, f)| CatalogMediaFragment {
            recording_id: "first".into(),
            recording_started_at_ms: 10_000,
            path: path.to_str().unwrap().into(),
            init_offset: 0,
            init_len: init,
            sequence: u64::from(f.sequence_number),
            start_ms: 10_000 + i as i64 * 120,
            duration_ms: 120,
            byte_offset: f.range.offset,
            byte_len: f.range.size,
            archive: None,
        })
        .collect()
}

fn work() -> Work<'static> {
    Work {
        deadline: Instant::now() + Duration::from_secs(2),
        cancelled: &|| false,
    }
}

#[test]
fn returns_the_source_adjacent_sample_and_reverses_across_a_fragment() {
    let dir = RecordingDirectory::new();
    let fragments = recording(
        &dir.0.join("recording.mp4"),
        &[Some(0), Some(80), Some(40), Some(120), Some(180), Some(160)],
    );
    let request = proto::StepStoredFrame {
        generation: 1,
        step_id: "step".into(),
        stored_media_id: "playback".into(),
        ..Default::default()
    };
    let next = target_window(&fragments, 10_000_000_000, None, true, &work()).unwrap();
    assert_eq!(next.index.pictures[next.target].ordinal, 3);
    let decoded = encode(next, &request, &work()).unwrap();
    assert_eq!(decoded.result.source_ticks, 40);
    assert_eq!(decoded.result.target_presentation_rank, 1);
    assert_eq!(decoded.result.sample_count, 3);
    let reverse = target_window(
        &fragments,
        decoded.selected.stored_ns,
        Some(&decoded.selected),
        false,
        &work(),
    )
    .unwrap();
    assert_eq!(reverse.index.pictures[reverse.target].ordinal, 1);
    let crossing = target_window(&fragments, 10_120_000_000, None, false, &work()).unwrap();
    assert_eq!(crossing.fragment.sequence, fragments[0].sequence);
    assert_eq!(crossing.index.pictures[crossing.target].ordinal, 2);
    let mut changed = fragments.clone();
    changed[0].recording_id = "replacement".into();
    assert!(matches!(
        target_window(
            &changed,
            decoded.selected.stored_ns,
            Some(&decoded.selected),
            true,
            &work()
        ),
        Err(Unavailable::Stale)
    ));
    changed[1].start_ms += 1_000;
    assert!(matches!(
        target_window(&changed, 10_040_000_000, None, true, &work()),
        Err(Unavailable::Gap)
    ));
}

#[test]
fn never_uses_storage_decode_order_to_guess_an_unknown_picture() {
    let dir = RecordingDirectory::new();
    let path = dir.0.join("recording.mp4");
    for (times, reason) in [
        ([Some(0), Some(40), Some(40)], Unavailable::Ambiguous),
        ([Some(0), None, Some(80)], Unavailable::Timing),
    ] {
        let fragments = recording(&path, &times);
        assert_eq!(
            target_window(&fragments, 10_000_000_000, None, true, &work()).err(),
            Some(reason)
        );
    }
}

fn routed_fixture(path: &Path) -> (ServerState, crate::storage::RecordingCatalog) {
    use crate::storage::{CatalogFragment, CatalogRecording, RecordingCatalog};
    let catalog = RecordingCatalog::open(&path.join("catalog.db")).unwrap();
    let parts = recording(
        &path.join("recording.mp4"),
        &[Some(0), Some(80), Some(40), Some(120), Some(200), Some(160)],
    );
    let first = &parts[0];
    let handle = catalog.handle();
    handle
        .upsert_recording(CatalogRecording {
            id: first.recording_id.clone(),
            stream_id: "front-door/sub".into(),
            source_id: Some("127.0.0.1".into()),
            logical_stream_id: Some("sub".into()),
            started_at_ms: first.recording_started_at_ms,
            ended_at_ms: Some(10_240),
            path: first.path.clone(),
            init_offset: first.init_offset,
            init_len: first.init_len,
            finalized: true,
        })
        .unwrap();
    for part in parts {
        handle
            .insert_fragment(CatalogFragment {
                recording_id: part.recording_id,
                sequence: part.sequence,
                start_ms: part.start_ms,
                duration_ms: part.duration_ms,
                byte_offset: part.byte_offset,
                byte_len: part.byte_len,
                random_access: true,
            })
            .unwrap();
    }
    let mut state = crate::server::tests::media_test_state();
    state.catalog = Some(handle);
    (state, catalog)
}

#[test]
fn paused_cursor_steps_under_historical_access_rules_and_invalidates_previous_tokens() {
    use proto::stored_media_command::Action;
    let root = RecordingDirectory::new();
    let (state, catalog) = routed_fixture(&root.0);
    let session = SessionId::from_u64(479);
    state
        .api_session_owners
        .lock()
        .unwrap()
        .insert(session, crate::server::tests::local_test_session());
    let dispatch = |owner, action| {
        super::super::stored_media::dispatch(
            &state,
            owner,
            proto::StoredMediaCommand {
                action: Some(action),
            },
        )
    };
    let opened = dispatch(
        session,
        Action::Open(proto::OpenStoredMedia {
            stored_media_id: "playback".into(),
            source_id: "127.0.0.1".into(),
            stream_id: "main".into(),
            timestamp: Some(millis_timestamp(10_000)),
            mode: proto::StoredMediaMode::Playback as i32,
            playing: false,
            playback_rate: 1.0,
            media_channel: proto::DataChannelKind::ReliableData as i32,
            ..Default::default()
        }),
    )
    .unwrap();
    assert!(!opened.messages.is_empty());
    let Some(control_ok::Result::StoredMediaState(open)) = opened.result else {
        panic!("expected open cursor")
    };
    state
        .privacy
        .replace_profile_overrides(BTreeMap::from([("127.0.0.1".into(), true)]))
        .unwrap();
    let request = proto::StepStoredFrame {
        stored_media_id: "playback".into(),
        generation: open.generation,
        step_id: "first".into(),
        direction: proto::StoredFrameStepDirection::Next as i32,
        source_id: "127.0.0.1".into(),
        stream_id: "main".into(),
        anchor: Some(proto::step_stored_frame::Anchor::DisplayedTime(
            millis_timestamp(10_000),
        )),
    };
    assert!(dispatch(SessionId::from_u64(480), Action::StepFrame(request.clone())).is_err());
    let delivery = dispatch(session, Action::StepFrame(request.clone())).unwrap();
    let Some(control_ok::Result::StoredFrameStepResult(first)) = delivery.result else {
        panic!("missing selected picture")
    };
    assert_eq!(first.status(), proto::StoredFrameStepStatus::Ready);
    assert_eq!(first.source_ticks, 40);
    assert!(!delivery.messages.is_empty());
    let previous = proto::StepStoredFrame {
        step_id: "reverse".into(),
        direction: proto::StoredFrameStepDirection::Previous as i32,
        anchor: Some(proto::step_stored_frame::Anchor::SampleToken(
            first.sample_token.clone(),
        )),
        ..request
    };
    let step_result = |output: StoredMediaDispatch| match output.result {
        Some(control_ok::Result::StoredFrameStepResult(result)) => result,
        _ => panic!("missing step result"),
    };
    let second = step_result(dispatch(session, Action::StepFrame(previous.clone())).unwrap());
    assert_eq!(second.source_ticks, 0);
    assert_eq!(second.status(), proto::StoredFrameStepStatus::Ready);
    assert_ne!(second.sample_token, first.sample_token);
    let stale = dispatch(session, Action::StepFrame(previous)).unwrap();
    assert!(stale.messages.is_empty());
    assert_eq!(
        step_result(stale).status(),
        proto::StoredFrameStepStatus::Stale
    );
    drop(state);
    catalog.shutdown();
}
