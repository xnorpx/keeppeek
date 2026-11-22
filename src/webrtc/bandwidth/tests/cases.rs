use super::*;

#[test]
fn shared_feedback_poll_hook_observes_an_actual_str0m_event() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    let now = fixture.start + Duration::from_millis(1);
    fixture.rtc.bwe().reset(Bitrate::mbps(4));
    fixture.rtc.handle_input(Input::Timeout(now)).unwrap();
    let mut emitted_bitrate_bps = None;
    for _ in 0..64 {
        match poll_api_output(&mut fixture.rtc, &mut fixture.media, now).unwrap() {
            Output::Event(Event::EgressBitrateEstimate(BweKind::Twcc(bitrate))) => {
                emitted_bitrate_bps = Some(bitrate.as_u64());
                break;
            }
            Output::Timeout(_) => break,
            Output::Event(_) | Output::Transmit(_) => {}
        }
    }
    let emitted =
        emitted_bitrate_bps.expect("str0m did not emit the changed BWE event after reset");
    assert!(emitted > 0);
    assert!(
        fixture.allocation(0) > 0,
        "the shared poll hook discarded the BWE event"
    );
    assert!(fixture.allocation(0) <= emitted);
}

#[test]
fn shared_feedback_unknown_and_zero_budget_keep_the_native_lowest_stream() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    for time_ms in (100..=4_000).step_by(100) {
        fixture.advance(time_ms, true);
        assert_eq!(fixture.allocation(0), 0);
        assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    }
    fixture.feedback(6_000_000, 4_100);
    fixture.advance(4_100, true);
    fixture.feedback(6_000_000, 7_100);
    fixture.advance(7_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.feedback(0, 7_200);
    fixture.advance(7_200, false);
    assert_eq!(fixture.allocation(0), 0);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.advance(7_300, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    fixture.close();
}

#[test]
fn shared_feedback_finite_budget_below_the_floor_does_not_invent_a_smaller_source() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    fixture.feedback(100_000, 100);
    fixture.advance(100, true);
    assert!(fixture.allocation(0) <= 100_000);
    assert!(fixture.allocation(0) > 0);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    assert_eq!(fixture.source_demand(0), 500_000);
    fixture.close();
}

#[test]
fn shared_feedback_manual_intent_reserves_capacity_and_reuses_its_mid() {
    let mut fixture = SharedFixture::new(&[StreamQuality::High, StreamQuality::Auto]);
    fixture.prime();
    let mid = fixture.mid(0);
    fixture.feedback(1_000_000, 100);
    fixture.advance(100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    assert_eq!(fixture.active_stream(1), StreamKind::Sub);
    assert_eq!(fixture.allocation(1), 0);
    fixture.replace_quality(0, StreamQuality::Low);
    fixture.feedback(20_000_000, 200);
    fixture.advance(200, false);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.advance(300, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    assert_eq!(fixture.mid(0), mid);
    fixture.close();
}

#[test]
fn shared_feedback_exact_variants_keep_their_resolved_intent() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    let mid = fixture.mid(0);
    fixture.subscribe_exact(0, StreamKind::Main);
    fixture.feedback(100_000, 100);
    fixture.advance(100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.subscribe_exact(0, StreamKind::Sub);
    fixture.feedback(20_000_000, 200);
    fixture.advance(200, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    assert_eq!(fixture.mid(0), mid);
    assert_eq!(fixture.media.tracks.len(), 1);
    fixture.close();
}

#[test]
fn shared_feedback_auto_without_a_substream_reserves_its_fixed_main_demand() {
    let mut fixture = SharedFixture::with_substreams(&[
        (StreamQuality::Auto, false),
        (StreamQuality::Auto, true),
    ]);
    fixture.prime();
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    assert_eq!(fixture.active_stream(1), StreamKind::Sub);
    assert!(fixture.allocation(1) <= 1_000_000);
    fixture.close();
}

#[test]
fn shared_feedback_remb_caps_only_its_mid_and_cannot_supply_a_session_budget() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto; 2]);
    fixture.remb(fixture.mid(0), 500_000, 0);
    assert_eq!(fixture.allocation(0), 0);
    assert_eq!(fixture.allocation(1), 0);
    fixture.feedback(6_000_000, 1);
    assert!(fixture.allocation(0) <= 500_000);
    assert!(fixture.allocation(1) > 500_000);
    assert!(fixture.allocation(0) + fixture.allocation(1) <= 6_000_000);
    fixture.close();
}

#[test]
fn shared_feedback_removed_mid_does_not_pass_its_receiver_cap_to_the_next_track() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto; 2]);
    let reused_mid = fixture.mid(0);
    fixture.remb(reused_mid, 500_000, 0);
    fixture.feedback(6_000_000, 1);
    assert!(fixture.allocation(0) <= 500_000);
    fixture.remove(0);
    assert_eq!(fixture.media.tracks.len(), 1);
    assert_eq!(fixture.subscriber_count(0), 1);
    fixture.subscribe(2, StreamQuality::Auto, true);
    assert_eq!(fixture.mid(1), reused_mid);
    fixture.feedback(6_000_000, 2);
    assert!(fixture.allocation(1) > 500_000);
    fixture.assert_subscriber_bounds();
    fixture.close();
}
