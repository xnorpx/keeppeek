use super::*;

fn cursor(state: &ServerState) -> StoredMediaCursor {
    StoredMediaCursor {
        incarnation: Uuid::new_v4(),
        identifiers: StoredMediaIdentifiers::default(),
        recorded_audio: proto::RecordedAudioTiming::default(),
        archive: false,
        source_id: "camera".into(),
        stream_id: "main".into(),
        recording_stream_id: "camera/main".into(),
        requested_time_ms: 10_000,
        end_time_ms: None,
        mode: proto::StoredMediaMode::Playback,
        playing: true,
        playback_rate: 1.0,
        media_target: DataChannelTarget::Reliable,
        media_channel: proto::DataChannelKind::ReliableData,
        max_buffer_ms: 1_000,
        generation: 1,
        content_type: "video/mp4".into(),
        fragment_time_ms: 10_000,
        delivered_through_ms: 11_000,
        status: proto::StoredMediaStatus::Active,
        _demand: state.recording_demand.acquire("camera/main"),
    }
}

#[test]
fn close_and_reopen_with_the_same_public_id_rejects_an_old_inflight_delivery() {
    let state = ServerState::empty();
    let session = SessionId::from_u64(91);
    let key = (session, "cursor".to_owned());
    state
        .stored_media_cursors
        .lock()
        .unwrap()
        .insert(key.clone(), cursor(&state));
    let before = load(&state, session, "cursor").unwrap();
    let mut reopened = cursor(&state);
    reopened.recorded_audio.applied_offset_ms = 250;
    state
        .stored_media_cursors
        .lock()
        .unwrap()
        .insert(key, reopened);
    let error = commit(&state, session, "cursor", &before, before.clone(), None).unwrap_err();
    assert_eq!(error.code, proto::ErrorCode::Rejected);
    let current = load(&state, session, "cursor").unwrap();
    assert_ne!(current.incarnation, before.incarnation);
    assert_eq!(current.timing.applied_offset_ms, 250);
    assert_eq!(current.delivered_through_ms, before.delivered_through_ms);
}

#[test]
fn competing_delivery_cannot_reuse_or_roll_back_generation_scoped_identifiers() {
    let state = ServerState::empty();
    let session = SessionId::from_u64(92);
    let key = (session, "cursor".to_owned());
    state
        .stored_media_cursors
        .lock()
        .unwrap()
        .insert(key.clone(), cursor(&state));
    let before = load(&state, session, "cursor").unwrap();
    state
        .stored_media_cursors
        .lock()
        .unwrap()
        .get_mut(&key)
        .unwrap()
        .identifiers
        .sequence = 2;
    let error = commit(&state, session, "cursor", &before, before.clone(), None).unwrap_err();
    assert_eq!(error.code, proto::ErrorCode::Rejected);
    assert_eq!(
        load(&state, session, "cursor")
            .unwrap()
            .identifiers
            .sequence,
        2
    );
}
