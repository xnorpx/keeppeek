use super::*;
use crate::api::proto::{
    LiveQualityDiagnostics, LiveQualitySelectionReason as Reason, Notification,
};

#[test]
fn live_preview_changes_emit_timing_even_when_network_quality_is_stable() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    fixture.take_stream_states();
    fixture.media.tracks[0]
        .runtime
        .timing
        .set_preview(Some(-80));
    fixture
        .media
        .report_live_quality(fixture.start + Duration::from_secs(2));
    let events = fixture.take_stream_states();
    let timing = events.last().unwrap().live_audio.unwrap();
    assert!(timing.preview);
    assert_eq!(
        (timing.configured_offset_ms, timing.applied_offset_ms),
        (0, -80)
    );
    fixture
        .media
        .report_live_quality(fixture.start + Duration::from_secs(4));
    assert!(fixture.take_stream_states().is_empty());
    fixture.close();
}

#[test]
fn shared_queue_occupancy_is_sampled_while_drops_and_preview_are_reported_promptly() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    fixture
        .media
        .report_live_quality(fixture.start + Duration::from_secs(1));
    fixture.take_stream_states();

    let source = fixture.media.tracks[0].runtime.subscription.active_source;
    let mut other_viewer =
        live_timing::Queue::new(fixture.session.inner.camera_timing(source.camera_ip));
    other_viewer.set_preview(Some(-80));
    let packet = |received, bytes| live_timing::Packet {
        stream: source.stream,
        format: live_timing::Format::Video(VideoCodec::H264),
        received,
        timestamp: None,
        bytes,
        payload_address: 1,
    };
    let mut notified_at = Vec::new();
    for second in 2..=21 {
        let now = fixture.start + Duration::from_secs(second);
        let bytes = usize::try_from(second).unwrap();
        other_viewer.clear();
        assert!(other_viewer.admit((), packet(now, bytes), now).is_none());
        match second {
            4 => {
                assert!(
                    other_viewer
                        .admit((), packet(now - Duration::from_secs(1), bytes), now)
                        .is_none()
                );
            }
            6 => {
                assert!(
                    other_viewer
                        .admit((), packet(now, usize::MAX), now)
                        .is_none()
                );
            }
            8 => {
                fixture.media.tracks[0].runtime.timing.set_preview(Some(40));
            }
            _ => {}
        }
        fixture.media.report_live_quality(now);
        let states = fixture.take_stream_states();
        if !states.is_empty() {
            assert_eq!(states.len(), 1);
            let timing = states[0].live_audio.unwrap();
            assert_eq!(u64::from(timing.held_video_bytes), second);
            assert_eq!(timing.held_video_frames, 1);
            assert_eq!(timing.expired_drops, u64::from(second >= 4));
            assert_eq!(timing.capacity_drops, u64::from(second >= 6));
            assert_eq!(timing.preview, second >= 8);
            notified_at.push(second);
        }
    }
    assert_eq!(notified_at, [4, 6, 8, 10, 20]);
    fixture.close();
}

#[test]
fn live_quality_follows_holds_and_the_actual_keyframe_handoff() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    phase(&fixture, Reason::BandwidthUnknown, "sub", "");
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, false);
    phase(&fixture, Reason::UpgradeHold, "sub", "");
    fixture.advance(3_100, false);
    phase(&fixture, Reason::WaitingKeyframe, "sub", "main");
    fixture.advance(3_200, true);
    phase(&fixture, Reason::Stable, "main", "");
    fixture.feedback(2_000_000, 3_300);
    fixture.advance(3_300, false);
    phase(&fixture, Reason::DowngradeHold, "main", "");
    fixture.advance(4_300, false);
    phase(&fixture, Reason::WaitingKeyframe, "main", "sub");
    fixture.advance(4_400, true);
    phase(&fixture, Reason::InsufficientBandwidth, "sub", "");
    fixture.close();
}

#[test]
fn live_quality_distinguishes_unknown_zero_and_explicit_or_fixed_intent() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    assert_eq!(snapshot(&fixture, 0).session_bandwidth_bps, None);
    assert_eq!(snapshot(&fixture, 0).allocated_bitrate_bps, Some(0));
    fixture.feedback(0, 100);
    fixture.advance(100, true);
    let zero = snapshot(&fixture, 0);
    assert_eq!(zero.session_bandwidth_bps, Some(0));
    assert_eq!(zero.allocated_bitrate_bps, Some(0));
    phase(&fixture, Reason::InsufficientBandwidth, "sub", "");
    fixture.subscribe_exact(0, StreamKind::Main);
    fixture.advance(200, false);
    phase(&fixture, Reason::Manual, "sub", "main");
    assert_eq!(snapshot(&fixture, 0).allocated_bitrate_bps, None);
    fixture.subscribe_fixed_data(0, StreamKind::Main).unwrap();
    let fixed = snapshot(&fixture, 1);
    assert_eq!(fixed.selection_reason, Reason::FixedDelivery as i32);
    assert_eq!(fixed.allocated_bitrate_bps, None);
    fixture.close();
    let mut only_sub = SharedFixture::with_variants(&[(StreamQuality::Auto, false, true)]);
    only_sub.prime();
    only_sub.feedback(20_000_000, 100);
    phase(&only_sub, Reason::SingleVariant, "sub", "");
    assert_eq!(snapshot(&only_sub, 0).allocated_bitrate_bps, None);
    only_sub.close();
}

#[test]
fn live_quality_unavailable_main_is_distinct_from_insufficient_capacity() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    let main = fixture.media.tracks[0].runtime.subscription.high_source;
    fixture
        .session
        .inner
        .sources
        .lock()
        .unwrap()
        .get_mut(&main)
        .unwrap()
        .keyframe = None;
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, false);
    fixture.advance(3_100, false);
    phase(&fixture, Reason::VariantUnavailable, "sub", "");
    assert!(
        snapshot(&fixture, 0)
            .allocated_bitrate_bps
            .is_some_and(|available| available >= 5_000_000)
    );
    fixture.close();
}

#[test]
fn live_quality_manual_and_exact_main_report_the_unnegotiated_codec_without_losing_intent() {
    for exact in [false, true] {
        let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
        fixture.prime();
        let mid = fixture.mid(0);
        assert!(
            fixture
                .rtc
                .writer(mid)
                .unwrap()
                .payload_params()
                .all(|params| params.spec().codec != Codec::H265)
        );
        let main = fixture.media.tracks[0].runtime.subscription.high_source;
        fixture
            .session
            .inner
            .sources
            .lock()
            .unwrap()
            .get_mut(&main)
            .unwrap()
            .keyframe
            .as_mut()
            .unwrap()
            .codec = VideoCodec::H265;
        if exact {
            fixture.subscribe_exact(0, StreamKind::Main);
        } else {
            fixture.replace_quality(0, StreamQuality::High);
        }
        fixture.feedback(6_000_000, 100);
        fixture.advance(100, false);
        phase(&fixture, Reason::VariantUnavailable, "sub", "");
        assert_eq!(
            fixture.media.tracks[0]
                .runtime
                .subscription
                .requested_quality(),
            Some(StreamQuality::High)
        );
        assert_eq!(fixture.mid(0), mid);
        fixture.close();
    }
}

#[test]
fn live_quality_full_notification_queue_retries_the_latest_snapshot() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    fixture.take_stream_states();
    for _ in 0..API_CONTROL_NOTIFICATION_MAX_MESSAGES {
        fixture
            .media
            .enqueue_control_notification(Notification::default())
            .unwrap();
    }
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, false);
    fixture
        .media
        .report_live_quality(fixture.start + Duration::from_secs(1));
    assert_eq!(
        fixture.media.control_notifications.len(),
        API_CONTROL_NOTIFICATION_MAX_MESSAGES
    );
    fixture
        .media
        .flush_control_notifications(|_| Ok(true))
        .unwrap();
    fixture.feedback(2_000_000, 1_100);
    fixture.advance(1_100, false);
    fixture
        .media
        .report_live_quality(fixture.start + Duration::from_secs(2));
    let states = fixture.take_stream_states();
    assert_eq!(states.len(), 1);
    let quality = states[0].live_quality.as_ref().unwrap();
    assert_eq!(quality.session_bandwidth_bps, Some(2_000_000));
    assert_eq!(
        quality.selection_reason,
        Reason::InsufficientBandwidth as i32
    );
    assert_eq!(fixture.media.control_notification_bytes, 0);
    fixture.close();
}

fn snapshot(fixture: &SharedFixture, index: usize) -> LiveQualityDiagnostics {
    fixture
        .media
        .live_quality(&fixture.media.tracks[index].runtime.track_id)
        .unwrap()
}

fn phase(fixture: &SharedFixture, reason: Reason, active: &str, pending: &str) {
    let quality = snapshot(fixture, 0);
    assert_eq!(quality.selection_reason, reason as i32);
    assert_eq!(quality.active_variant_id, active);
    assert_eq!(quality.pending_variant_id, pending);
}
