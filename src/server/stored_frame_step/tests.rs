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
    recording_with_width(path, times, 320)
}

fn recording_with_width(
    path: &Path,
    times: &[Option<i64>],
    width: u16,
) -> Vec<CatalogMediaFragment> {
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
            width,
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
    let decoded = plan(next, &request, &work()).unwrap();
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
fn first_last_gap_codec_and_retired_fragment_have_explicit_outcomes() {
    let dir = RecordingDirectory::new();
    let path = dir.0.join("first.mp4");
    let times = [Some(0), Some(40), Some(80)];
    let first = recording(&path, &times);
    assert_eq!(
        target_window(&first, 10_000_000_000, None, false, &work()).err(),
        Some(Unavailable::Boundary)
    );
    assert_eq!(
        target_window(&first, 10_080_000_000, None, true, &work()).err(),
        Some(Unavailable::Boundary)
    );
    let next_path = dir.0.join("next.mp4");
    let mut next = recording_with_width(&next_path, &times, 640).remove(0);
    next.recording_id = "next".into();
    next.recording_started_at_ms = 10_120;
    next.start_ms = 10_120;
    let mut window = vec![first[0].clone(), next];
    assert_eq!(
        target_window(&window, 10_080_000_000, None, true, &work()).err(),
        Some(Unavailable::Discontinuity)
    );
    assert_eq!(
        target_window(&window, 10_120_000_000, None, false, &work()).err(),
        Some(Unavailable::Discontinuity)
    );
    window[1].start_ms += 500;
    assert_eq!(
        target_window(&window, 10_080_000_000, None, true, &work()).err(),
        Some(Unavailable::Gap)
    );
    std::fs::remove_file(path).unwrap();
    assert_eq!(
        target_window(&first, 10_000_000_000, None, true, &work()).err(),
        Some(Unavailable::Stale)
    );
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
fn frame_step_catalog_window_reports_the_first_excess_fragment_as_a_limit() {
    let root = RecordingDirectory::new();
    let (state, catalog) = routed_fixture(&root.0);
    let handle = state.catalog.as_ref().unwrap();
    let read = || {
        handle.frame_step_window(
            "front-door/sub",
            10_001,
            Instant::now() + Duration::from_secs(2),
        )
    };
    let (_, initial, leases) = read().unwrap();
    drop(leases);
    assert_eq!(initial.len(), 2);
    for i in 2_u32..33 {
        handle
            .insert_fragment(crate::storage::CatalogFragment {
                recording_id: initial[0].recording_id.clone(),
                sequence: initial[0].sequence + u64::from(i),
                start_ms: 10_000 + 120 * i64::from(i),
                duration_ms: 120,
                byte_offset: initial[0].byte_offset,
                byte_len: initial[0].byte_len,
                random_access: true,
            })
            .unwrap();
        if i == 31 {
            assert_eq!(read().unwrap().1.len(), 32);
        }
    }
    let Err(error) = read() else {
        panic!("33 fragments exceeded the bounded frame window");
    };
    assert_eq!(window_failure(error), Unavailable::Limit);
    drop(state);
    catalog.shutdown();
}

#[test]
fn paused_cursor_steps_under_historical_access_rules_and_invalidates_previous_tokens() {
    use proto::stored_media_command::Action;
    let root = RecordingDirectory::new();
    let (state, catalog) = routed_fixture(&root.0);
    assert!(
        super::super::server_capability_ids(&state)
            .iter()
            .any(|id| id == CAPABILITY_ID)
    );
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
    // EventBoost recorded the substream for this main-stream cursor. Only the
    // logical stream selected at open may address the cursor.
    let wrong_stream = proto::StepStoredFrame {
        stream_id: "sub".into(),
        ..request.clone()
    };
    assert!(dispatch(session, Action::StepFrame(wrong_stream)).is_err());
    for (source, stream) in [
        ("", "main"),
        ("invalid source", "main"),
        ("127.0.0.1", ""),
        ("127.0.0.1", "main/unsafe"),
    ] {
        let invalid = proto::StepStoredFrame {
            source_id: source.into(),
            stream_id: stream.into(),
            ..request.clone()
        };
        assert_eq!(
            dispatch(session, Action::StepFrame(invalid))
                .err()
                .unwrap()
                ._http_status,
            400
        );
    }
    let oversized = proto::StepStoredFrame {
        source_id: "x".repeat(65),
        ..request.clone()
    };
    assert_eq!(
        dispatch(session, Action::StepFrame(oversized))
            .err()
            .unwrap()
            ._http_status,
        400
    );
    let timed_out = step_until(&state, session, request.clone(), Instant::now())
        .err()
        .unwrap();
    assert_eq!(timed_out.code, proto::ErrorCode::Timeout);
    assert_eq!(state.frame_step_workers.load(Ordering::Acquire), 0);
    let delivery = dispatch(session, Action::StepFrame(request.clone())).unwrap();
    let Some(control_ok::Result::StoredFrameStepResult(first)) = delivery.result else {
        panic!("missing selected picture")
    };
    assert_eq!(first.status(), proto::StoredFrameStepStatus::Ready);
    assert_eq!(first.source_ticks, 40);
    assert!(delivery.messages.is_empty());
    let transfer = delivery
        .frame_transfer
        .expect("ready reply precedes GOP production");
    // The first response has already arrived. A transfer can still deliver after its
    // original two-second control budget, inside the independently enforced decode budget.
    let decode_deadline = Instant::now() + DECODE_WINDOW;
    std::thread::sleep(FIRST_RESPONSE + Duration::from_millis(25));
    let mut chunks = Vec::new();
    transfer
        .deliver(decode_deadline, |message| {
            chunks.push(message);
            Ok(())
        })
        .unwrap();
    assert!(!chunks.is_empty());
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
    let expired = dispatch(session, Action::StepFrame(previous.clone())).unwrap();
    assert!(
        expired
            .frame_transfer
            .unwrap()
            .deliver(Instant::now(), |_| panic!("expired delivery sent data"))
            .is_err()
    );
    let dropped = dispatch(session, Action::StepFrame(previous.clone())).unwrap();
    let transfer = dropped.frame_transfer.unwrap();
    let last = transfer.plan.result.sample_count;
    let mut sent = 0;
    assert!(
        transfer
            .deliver(Instant::now() + DECODE_WINDOW, |_| {
                sent += 1;
                anyhow::ensure!(sent != last, "fixture transport rejected final chunk");
                Ok(())
            })
            .is_err()
    );
    assert_eq!(sent, last);
    let mut ready = dispatch(session, Action::StepFrame(previous.clone())).unwrap();
    ready
        .frame_transfer
        .take()
        .unwrap()
        .deliver(Instant::now() + DECODE_WINDOW, |_| Ok(()))
        .unwrap();
    let second = step_result(ready);
    assert_eq!(second.source_ticks, 0);
    assert_eq!(second.status(), proto::StoredFrameStepStatus::Ready);
    assert_ne!(second.sample_token, first.sample_token);
    // The browser may keep the previous picture if decoding the delivered window failed.
    // Retry from that displayed token without a disruptive seek.
    let mut retry = dispatch(session, Action::StepFrame(previous.clone())).unwrap();
    retry
        .frame_transfer
        .take()
        .unwrap()
        .deliver(Instant::now() + DECODE_WINDOW, |_| Ok(()))
        .unwrap();
    let retried = step_result(retry);
    assert_eq!(retried.source_ticks, second.source_ticks);
    assert_ne!(retried.sample_token, second.sample_token);
    let from = |token| proto::StepStoredFrame {
        anchor: Some(proto::step_stored_frame::Anchor::SampleToken(token)),
        direction: proto::StoredFrameStepDirection::Next as i32,
        ..previous.clone()
    };
    let superseded = dispatch(session, Action::StepFrame(from(second.sample_token))).unwrap();
    assert_eq!(
        step_result(superseded).status(),
        proto::StoredFrameStepStatus::Stale
    );
    let accepted = dispatch(
        session,
        Action::StepFrame(from(retried.sample_token.clone())),
    )
    .unwrap();
    assert!(accepted.frame_transfer.is_some());
    drop(accepted);
    let stale = dispatch(session, Action::StepFrame(previous.clone())).unwrap();
    assert!(stale.messages.is_empty());
    assert_eq!(
        step_result(stale).status(),
        proto::StoredFrameStepStatus::Stale
    );
    let ready = dispatch(session, Action::StepFrame(from(retried.sample_token))).unwrap();
    let queued = ready.frame_transfer.as_ref().unwrap().op.queued.clone();
    dispatch(
        session,
        Action::Close(proto::CloseStoredMedia {
            stored_media_id: "playback".into(),
        }),
    )
    .unwrap();
    assert!(
        ready
            .frame_transfer
            .unwrap()
            .deliver(Instant::now() + DECODE_WINDOW, |_| panic!(
                "closed cursor sent a chunk"
            ))
            .is_err()
    );
    assert!(queued.load(Ordering::Acquire));
    assert_eq!(state.frame_step_workers.load(Ordering::Acquire), 0);
    drop(state);
    catalog.shutdown();
}

#[test]
fn duplicate_source_timestamps_step_in_codec_display_order_in_both_directions() {
    let data = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/exact_frame_step/duplicate-pts.mp4"
    ));
    let mut input =
        mp4::Mp4Reader::read_header(std::io::Cursor::new(data), data.len() as u64).unwrap();
    let input_track = &input.tracks()[&1];
    let track = mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: 1_000,
        language: "und".into(),
        media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
            width: input_track.width(),
            height: input_track.height(),
            seq_param_set: input_track.sequence_parameter_set().unwrap().to_vec(),
            pic_param_set: input_track.picture_parameter_set().unwrap().to_vec(),
        }),
    };
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap()],
        timescale: 1_000,
    };
    let directory = RecordingDirectory::new();
    let path = directory.0.join("reordered.mp4");
    let mut writer = mp4::FragmentedMp4Writer::write_start(
        std::fs::File::create(&path).unwrap(),
        &config,
        &[track],
    )
    .unwrap();
    // The recorder may assign monotonic stored times while retaining original RTP timestamps.
    for ordinal in 7..=12 {
        let mut sample = input.read_sample(1, ordinal).unwrap().unwrap();
        let timestamp = i64::try_from(
            input.tracks()[&1]
                .sample_timing(ordinal)
                .unwrap()
                .unwrap()
                .presentation_time,
        )
        .unwrap();
        sample.start_time = u64::from(ordinal - 7) * 40;
        sample.duration = 40;
        sample.rendering_offset = 0;
        writer
            .write_sample_with_source_timing(
                1,
                1,
                sample,
                Some(mp4::SourceRtpPacket {
                    session_id: [1; 16],
                    epoch_id: [2; 16],
                    clock_rate: 1000.try_into().unwrap(),
                    timestamp,
                }),
            )
            .unwrap();
    }
    let fragment = writer.flush_fragment().unwrap().unwrap();
    writer.write_end().unwrap();
    let fragments = [CatalogMediaFragment {
        recording_id: "reordered".into(),
        recording_started_at_ms: 10_000,
        path: path.to_str().unwrap().into(),
        init_offset: 0,
        init_len: fragment.range.offset,
        sequence: u64::from(fragment.sequence_number),
        start_ms: 10_000,
        duration_ms: 240,
        byte_offset: fragment.range.offset,
        byte_len: fragment.range.size,
        archive: None,
    }];
    let request = proto::StepStoredFrame {
        generation: 1,
        step_id: "duplicate".into(),
        stored_media_id: "playback".into(),
        ..Default::default()
    };
    let mut previous = None;
    let mut anchor = 10_000_000_000;
    // Ordinal 6 (B picture 10) displays before ordinal 5 (reference picture 11).
    for (forward, ordinal, ticks, rank) in [
        (true, 3, 440, 1),
        (true, 4, 480, 2),
        (true, 2, 600, 3),
        (true, 6, 640, 4),
        (true, 5, 640, 5),
        (false, 6, 640, 4),
        (false, 2, 600, 3),
    ] {
        let window =
            target_window(&fragments, anchor, previous.as_ref(), forward, &work()).unwrap();
        let step = plan(window, &request, &work()).unwrap();
        assert_eq!(
            (
                step.selected.ordinal,
                step.result.source_ticks,
                step.result.target_presentation_rank
            ),
            (ordinal, ticks, rank)
        );
        anchor = step.selected.stored_ns;
        previous = Some(step.selected);
    }
}
