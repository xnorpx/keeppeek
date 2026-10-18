use super::*;
use crate::api::proto::LiveQualitySelectionReason;

#[test]
fn shared_feedback_unoffered_h264_profile_keeps_the_compatible_active_substream() {
    for quality in [StreamQuality::Auto, StreamQuality::High] {
        let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
        fixture.prime();
        cache_main_payload(&fixture, profile_payload(Some(0x6e001f)));
        fixture.replace_quality(0, quality);
        fixture.feedback(6_000_000, 100);
        fixture.advance(100, false);
        fixture.advance(3_100, false);
        assert_unavailable_without_handoff(&fixture);
        fixture.close();
    }
}

#[test]
fn shared_feedback_main_profile_offered_only_with_packetization_zero_cannot_replace_sub() {
    let mut offer = rtc_config().enable_h265(false).enable_h264(false);
    offer
        .codec_config()
        .add_h264(127.into(), None, true, 0x42001f);
    offer
        .codec_config()
        .add_h264(35.into(), None, false, 0x4d001f);
    let mut fixture = SharedFixture::with_offer_config(&[(StreamQuality::Auto, true, true)], offer);
    fixture.prime();
    let mid = fixture.mid(0);
    assert!(
        fixture
            .rtc
            .writer(mid)
            .unwrap()
            .payload_params()
            .any(|params| params.spec().codec == Codec::H264
                && params.spec().format.profile_level_id == Some(0x4d001f)
                && params.spec().format.packetization_mode == Some(0))
    );
    cache_main_payload(&fixture, profile_payload(Some(0x4d001f)));
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, false);
    fixture.advance(3_100, false);
    assert_unavailable_without_handoff(&fixture);
    fixture.close();
}

#[test]
fn shared_feedback_auto_requires_a_known_candidate_sps_profile() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    cache_main_payload(&fixture, profile_payload(None));
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, false);
    fixture.advance(3_100, false);
    assert_unavailable_without_handoff(&fixture);
    fixture.close();
}

#[test]
fn shared_feedback_actual_target_keyframe_must_remain_compatible_with_the_mid() {
    for profile in [Some(0x6e001f), None] {
        let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
        fixture.prime();
        fixture.feedback(6_000_000, 100);
        fixture.advance(100, false);
        fixture.advance(3_100, false);
        let main = fixture.media.tracks[0].runtime.subscription.high_source;
        assert_eq!(
            fixture.media.tracks[0].runtime.subscription.pending_source,
            Some(main)
        );
        let now = fixture.start + Duration::from_millis(3_200);
        // The queued frame can change after the owner has checked the cached candidate.
        inject_frame(&fixture, main, sample(profile_payload(profile), true, now));
        let result = drive_track_runtime(
            &mut fixture.rtc,
            &mut fixture.media.tracks[0].runtime,
            true,
            now,
        )
        .unwrap();
        assert_eq!(fixture.active_stream(0), StreamKind::Sub);
        assert!(fixture.media.tracks[0].runtime.keyframe_gate.has_live_gop());
        assert_eq!(result.applied_stream, None);
        let sub = fixture.media.tracks[0].runtime.subscription.active_source;
        assert!(
            fixture.media.tracks[0]
                .runtime
                .subscription
                .pending_source
                .is_none()
        );
        let sources = fixture.session.inner.sources.lock().unwrap();
        assert!(sources[&main].subscribers.is_empty());
        assert_eq!(sources[&sub].subscribers.len(), 1);
        drop(sources);
        inject_frame(
            &fixture,
            sub,
            sample(Bytes::from_static(&[0, 0, 0, 2, 0x41, 0x88]), false, now),
        );
        let continued = drive_track_runtime(
            &mut fixture.rtc,
            &mut fixture.media.tracks[0].runtime,
            true,
            now,
        )
        .unwrap();
        assert!(
            continued.wrote_media,
            "the incompatible target interrupted the old live GOP"
        );
        assert_eq!(fixture.active_stream(0), StreamKind::Sub);
        fixture.close();
    }
}

fn cache_main_payload(fixture: &SharedFixture, payload: Bytes) {
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
        .data = Arc::new(MediaFrameData::new(payload));
}

fn assert_unavailable_without_handoff(fixture: &SharedFixture) {
    let runtime = &fixture.media.tracks[0].runtime;
    assert_eq!(runtime.subscription.active_source.stream, StreamKind::Sub);
    assert!(
        runtime.subscription.pending_source.is_none(),
        "an unplayable Main was armed"
    );
    let quality = fixture.media.live_quality(&runtime.track_id).unwrap();
    assert_eq!(
        quality.selection_reason,
        LiveQualitySelectionReason::VariantUnavailable as i32
    );
}

fn inject_frame(fixture: &SharedFixture, source: Source, frame: MediaFrame) {
    let sequence = fixture.session.inner.sources.lock().unwrap()[&source].next_sequence;
    fixture.media.tracks[0]
        .runtime
        .subscription
        .sender
        .try_send(sequence, source, frame)
        .unwrap();
}

fn sample(data: Bytes, is_keyframe: bool, received_at: Instant) -> MediaFrame {
    MediaFrame {
        codec: VideoCodec::H264,
        is_keyframe,
        received_at,
        timestamp: None,
        data: Arc::new(MediaFrameData::new(data)),
    }
}

fn profile_payload(profile: Option<u32>) -> Bytes {
    profile.map_or_else(
        || Bytes::from_static(&[0, 0, 0, 2, 0x65, 0x88]),
        |profile| {
            let [_, profile_id, constraints, level] = profile.to_be_bytes();
            Bytes::from(vec![
                0,
                0,
                0,
                4,
                0x67,
                profile_id,
                constraints,
                level,
                0,
                0,
                0,
                2,
                0x65,
                0x88,
            ])
        },
    )
}
