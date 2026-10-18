use super::*;

mod cases;
mod codecs;
mod diagnostic_states;
mod diagnostics;
mod fixture;
mod handover;
mod lifecycle;
mod preopen;
mod trace;

use fixture::SharedFixture;

#[test]
fn shared_feedback_reaches_real_auto_subscriptions() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.feedback(6_000_000, 0);

    let allocation = fixture.allocation(0);
    assert!(
        allocation > 0,
        "shared TWCC feedback never reached the Auto track"
    );
    assert!(
        allocation <= 6_000_000,
        "track allocation exceeds session feedback"
    );
    assert_eq!(fixture.active_stream(0), StreamKind::Sub);
}

#[test]
fn shared_feedback_is_a_session_budget_not_a_budget_per_track() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto; 3]);
    fixture.feedback(6_000_000, 0);

    let allocations: Vec<_> = (0..3).map(|index| fixture.allocation(index)).collect();
    assert!(allocations.iter().all(|allocation| *allocation > 0));
    assert!(allocations.iter().sum::<u64>() <= 6_000_000);
    assert!(allocations.iter().all(|allocation| *allocation < 4_000_000));
    fixture.assert_subscriber_bounds();
}
