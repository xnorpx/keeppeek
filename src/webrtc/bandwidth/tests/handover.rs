use super::*;

#[test]
fn shared_feedback_manual_handover_reserves_the_still_active_source_until_admission() {
    let mut fixture = SharedFixture::new(&[StreamQuality::High, StreamQuality::Auto]);
    fixture.prime();
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    assert_eq!(fixture.active_stream(1), StreamKind::Sub);
    fixture.replace_quality(0, StreamQuality::Low);
    let withheld = Source {
        camera_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        stream: StreamKind::Sub,
    };
    let mut peak_active_bps = 0;
    let mut peak_auto_allocation_bps = 0;
    let mut early_auto_promotion = false;
    for time_ms in (100..=4_200).step_by(100) {
        fixture.feedback(6_000_000, time_ms);
        fixture.advance_withheld(time_ms, time_ms % 500 == 0, Some(withheld));
        assert_eq!(fixture.active_stream(0), StreamKind::Main);
        peak_auto_allocation_bps = peak_auto_allocation_bps.max(fixture.allocation(1));
        peak_active_bps = peak_active_bps.max(fixture.source_demand(0) + fixture.source_demand(1));
        early_auto_promotion |= fixture.active_stream(1) == StreamKind::Main;
    }
    println!(
        "SHARED_MANUAL_HANDOVER peak_active_bps={peak_active_bps} peak_auto_allocation_bps={peak_auto_allocation_bps} early_auto_promotion={early_auto_promotion}"
    );
    assert!(
        peak_active_bps <= 6_000_000,
        "manual handover overbooked active source demand: {peak_active_bps}"
    );
    assert!(
        !early_auto_promotion,
        "Auto promoted before the manual downgrade was admitted"
    );
    assert!(peak_auto_allocation_bps <= 2_000_000);
    fixture.feedback(6_000_000, 4_300);
    fixture.advance(4_300, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    for time_ms in (4_400..=8_000).step_by(100) {
        fixture.feedback(6_000_000, time_ms);
        fixture.advance(time_ms, time_ms % 500 == 0);
    }
    assert_eq!(fixture.active_stream(1), StreamKind::Main);
    fixture.close();
}

#[test]
fn shared_feedback_zero_budget_resets_a_partial_upgrade_hold_before_recovery() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, true);
    fixture.feedback(0, 2_100);
    fixture.advance(2_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    fixture.feedback(6_000_000, 20_000);
    fixture.advance(20_000, false);
    assert!(
        fixture.media.tracks[0]
            .runtime
            .subscription
            .pending_source
            .is_none(),
        "recovery reused the upgrade hold from before zero feedback"
    );
    fixture.advance(20_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    fixture.advance(23_000, false);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    fixture.advance(23_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.close();
}

#[test]
fn shared_feedback_zero_budget_preserves_an_armed_downgrade_under_continued_pressure() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, true);
    fixture.advance(3_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.feedback(2_000_000, 3_200);
    fixture.advance(3_200, false);
    fixture.feedback(0, 3_300);
    fixture.advance(3_300, false);
    assert_eq!(
        fixture.media.tracks[0]
            .runtime
            .subscription
            .pending_source
            .map(|source| source.stream),
        Some(StreamKind::Sub)
    );
    fixture.feedback(4_000_000, 20_000);
    fixture.advance(20_000, false);
    assert_eq!(
        fixture.media.tracks[0]
            .runtime
            .subscription
            .pending_source
            .map(|source| source.stream),
        Some(StreamKind::Sub)
    );
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.advance(20_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    fixture.close();
}

#[test]
fn shared_feedback_manual_low_interrupts_a_partial_auto_upgrade_hold() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, true);
    fixture.replace_quality(0, StreamQuality::Low);
    fixture.advance(2_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    fixture.replace_quality(0, StreamQuality::Auto);
    fixture.feedback(6_000_000, 20_000);
    fixture.advance(20_000, false);
    assert!(
        fixture.media.tracks[0]
            .runtime
            .subscription
            .pending_source
            .is_none(),
        "return to Auto reused headroom from before the manual override"
    );
    fixture.advance(20_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    fixture.advance(23_000, false);
    fixture.advance(23_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.close();
}

#[test]
fn shared_feedback_idempotent_auto_resubscribe_preserves_an_ongoing_upgrade_hold() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    fixture.feedback(6_000_000, 100);
    fixture.advance(100, true);
    fixture.subscribe(0, StreamQuality::Auto, true);
    fixture.advance(2_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
    fixture.advance(3_100, true);
    assert_eq!(fixture.active_stream(0), StreamKind::Main);
    fixture.close();
}
