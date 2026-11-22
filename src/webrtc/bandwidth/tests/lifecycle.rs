use super::*;

#[test]
fn shared_feedback_unchanged_twcc_survives_a_long_event_gap() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, true);
    fixture.advance(3_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    let allocated = fixture.allocation(0);
    fixture.take_stream_states();
    // This fixture has no connected DTLS peer. Advance only the real controller clock.
    let now = fixture.start + Duration::from_secs(60);
    fixture.media.refresh_bandwidth(&mut fixture.rtc, now);
    let track = &mut fixture.media.tracks[0].runtime;
    track
        .subscription
        .select_source(&mut fixture.rtc, track.delivery.mid(), now);
    assert_eq!(fixture.allocation(0), allocated);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    assert!(fixture.take_stream_states().is_empty());
    fixture.close();
}

#[test]
fn shared_feedback_threshold_jitter_does_not_accumulate_upgrade_hold() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    for time_ms in (100..=6_000).step_by(100) {
        let bitrate_bps = if time_ms % 400 < 200 {
            5_100_000
        } else {
            4_900_000
        };
        fixture.feedback(bitrate_bps, time_ms);
        fixture.advance(time_ms, time_ms % 500 == 0);
        assert_eq!(fixture.active_stream(0), StreamKind::Sub);
        assert!(
            fixture
                .take_stream_states()
                .iter()
                .all(|state| state.active_variant_id == "sub")
        );
    }
    fixture.feedback(6_000_000, 6_100);
    fixture.advance(6_100, false);
    fixture.advance(9_100, false);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    fixture.advance(9_200, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.close();
}

#[test]
fn shared_feedback_only_substream_never_probes_or_arms_a_missing_main() {
    let mut fixture = SharedFixture::with_variants(&[(StreamQuality::Auto, false, true)]);
    fixture.prime();
    fixture.feedback(20_000_000, 100);
    fixture.advance(100, true);
    fixture.advance(4_000, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    assert_eq!(fixture.source_demand(0), 500_000);
    assert!(
        fixture.media.tracks[0]
            .runtime
            .subscription
            .pending_source
            .is_none()
    );
    assert!(
        fixture
            .media
            .bandwidth
            .desired_bitrate_bps
            .is_some_and(|desired| desired <= 1_000_000)
    );
    assert!(
        fixture
            .session
            .inner
            .sources
            .lock()
            .unwrap()
            .keys()
            .all(|source| source.stream == StreamKind::Sub)
    );
    fixture.close();
}

#[test]
fn shared_feedback_rtp_audio_reserves_capacity_but_reliable_data_audio_does_not() {
    let mut fixture = SharedFixture::new(&[StreamQuality::High, StreamQuality::Auto]);
    fixture.prime();
    fixture.feedback(6_000_000, 100);
    let without_audio = fixture.allocation(1);
    assert!(without_audio > 0);
    let rtp_audio = fixture.subscribe_audio(crate::api::proto::DeliveryTransport::Rtp);
    fixture.feedback(6_000_000, 200);
    let with_rtp_audio = fixture.allocation(1);
    assert!(
        with_rtp_audio
            .checked_add(128_000)
            .is_some_and(|reserved| reserved <= without_audio)
    );
    fixture.subscribe_audio(crate::api::proto::DeliveryTransport::ReliableData);
    fixture.feedback(6_000_000, 300);
    assert_eq!(fixture.allocation(1), with_rtp_audio);
    fixture.media.unsubscribe(&fixture.session, &[rtp_audio]);
    fixture.feedback(6_000_000, 400);
    assert_eq!(fixture.allocation(1), without_audio);
    fixture.close();
}

#[test]
fn shared_feedback_fixed_data_variant_change_fails_without_false_success_or_mutation() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    let first = fixture.subscribe_fixed_data(0, StreamKind::Main).unwrap();
    let before = fixture.media.control_notifications.clone();
    let before_bytes = fixture.media.control_notification_bytes;
    let error = fixture
        .subscribe_fixed_data(0, StreamKind::Sub)
        .expect_err("a fixed data subscription falsely accepted a different variant");
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert_eq!(fixture.active_stream(1), StreamKind::Main);
    assert_eq!(fixture.media.tracks.len(), 2);
    assert_eq!(fixture.media.control_notifications, before);
    assert_eq!(fixture.media.control_notification_bytes, before_bytes);
    let repeated = fixture.subscribe_fixed_data(0, StreamKind::Main).unwrap();
    assert_eq!(repeated.delivery, first.delivery);
    assert_eq!(repeated.selected_variant_id, "main");
    fixture.close();
}

#[test]
fn shared_feedback_remove_and_replace_discard_only_their_blocked_stream_states() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto; 2]);
    let sibling_bytes = fixture
        .media
        .control_notifications
        .iter()
        .find_map(|notification| {
            if let Some(crate::api::proto::notification::Event::SubscriptionStreamState(state)) =
                &notification.event
                && state.subscription_id == "camera-1"
            {
                Some(control_notification_encoded_len(notification))
            } else {
                None
            }
        })
        .unwrap();
    fixture.remove(0);
    assert_eq!(fixture.media.control_notifications.len(), 1);
    assert_eq!(fixture.media.control_notification_bytes, sibling_bytes);
    fixture.subscribe(2, StreamQuality::Auto, true);
    let states = fixture.take_stream_states();
    assert_eq!(states.len(), 2);
    assert!(
        states
            .iter()
            .all(|state| state.subscription_id != "camera-0")
    );
    assert_eq!(fixture.media.control_notification_bytes, 0);
    let track_id = fixture.media.tracks[1].runtime.track_id.clone();
    fixture
        .media
        .enqueue_stream_state(&track_id, StreamKind::Sub);
    assert!(fixture.media.control_notification_bytes > 0);
    fixture.subscribe(2, StreamQuality::High, true);
    assert!(fixture.take_stream_states().iter().all(|state| {
        state.live_quality.as_ref().is_some_and(|quality| {
            quality.selection_reason == crate::api::proto::LiveQualitySelectionReason::Manual as i32
        })
    }));
    assert_eq!(fixture.media.control_notification_bytes, 0);
    fixture.close();
}
