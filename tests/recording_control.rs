use keeppeek::{
    cameras::CameraRecordingMode as Mode,
    storage::recording_control::{Clock, Control, Override, Reason, Source},
};
use std::time::{Duration, Instant};

const fn clock(now: Instant, utc_ms: i64) -> Clock {
    Clock {
        monotonic: now,
        utc_ms: Some(utc_ms),
    }
}

fn request(enabled: bool) -> Override {
    Override {
        enabled,
        source: Source::Manual,
        actor: "admin".into(),
        reason: "inspection".into(),
        ttl_ms: 1_000,
    }
}

#[test]
fn configured_off_and_privacy_bound_runtime_enablement() {
    let now = Instant::now();
    let mut off = Control::new(Mode::Off);
    assert!(
        off.set_override(off.revision(), request(true), clock(now, 10_000))
            .is_err()
    );
    assert_eq!(
        off.snapshot(clock(now, 10_000)).reason,
        Reason::ConfiguredDisabled
    );
    let mut control = Control::new(Mode::Both);
    control.set_privacy(Some(true));
    assert!(
        control
            .set_override(control.revision(), request(true), clock(now, 10_000))
            .is_err()
    );
    assert_eq!(control.snapshot(clock(now, 10_000)).mode, Mode::Off);
    assert_eq!(control.snapshot(clock(now, 10_000)).reason, Reason::Privacy);
    control.set_privacy(None);
    assert_eq!(
        control.snapshot(clock(now, 10_000)).reason,
        Reason::PrivacyUnavailable
    );
}

#[test]
fn override_expires_on_either_clock_and_does_not_revive_after_clock_reversal() {
    let now = Instant::now();
    let mut control = Control::new(Mode::Main);
    control
        .set_override(control.revision(), request(false), clock(now, 10_000))
        .unwrap();
    let disabled = control.snapshot(clock(now, 10_999));
    assert_eq!(disabled.mode, Mode::Off);
    assert_eq!(disabled.reason, Reason::Override);
    assert_eq!(disabled.request.unwrap().actor, "admin");
    assert_eq!(control.snapshot(clock(now, 11_000)).mode, Mode::Main);
    assert_eq!(control.snapshot(clock(now, 10_000)).mode, Mode::Main);
    control
        .set_override(control.revision(), request(false), clock(now, 10_000))
        .unwrap();
    assert_eq!(
        control
            .snapshot(clock(now + Duration::from_secs(1), 9_000))
            .mode,
        Mode::Main
    );
}

#[test]
fn stale_revisions_restart_and_invalid_requests_preserve_authority() {
    let now = Instant::now();
    let mut control = Control::new(Mode::Sub);
    let original = control.revision();
    control
        .set_override(original, request(false), clock(now, 10_000))
        .unwrap();
    assert!(
        control
            .clear_override(original, clock(now, 10_000))
            .is_err()
    );
    let current = control.revision();
    for invalid in [
        Override {
            ttl_ms: 0,
            ..request(true)
        },
        Override {
            ttl_ms: 86_400_001,
            ..request(true)
        },
        Override {
            reason: " ".into(),
            ..request(true)
        },
        Override {
            actor: "a".repeat(129),
            ..request(true)
        },
    ] {
        assert!(
            control
                .set_override(current, invalid, clock(now, 10_000))
                .is_err()
        );
        assert_eq!(control.revision(), current);
    }
    let mut restarted = Control::new(Mode::Sub);
    assert!(
        restarted
            .clear_override(current, clock(now, 10_000))
            .is_err()
    );
    assert_eq!(restarted.snapshot(clock(now, 10_000)).mode, Mode::Sub);
    control.clear_override(current, clock(now, 10_000)).unwrap();
    assert_eq!(control.snapshot(clock(now, 10_000)).mode, Mode::Sub);
}

#[test]
fn invalid_clock_fails_closed_without_reactivating_expired_requests() {
    let now = Instant::now();
    let mut control = Control::new(Mode::Main);
    control
        .set_override(control.revision(), request(true), clock(now, 10_000))
        .unwrap();
    let unknown = Clock {
        monotonic: now,
        utc_ms: None,
    };
    assert_eq!(control.snapshot(unknown).mode, Mode::Off);
    assert_eq!(control.snapshot(unknown).reason, Reason::ClockUnavailable);
    assert_eq!(
        control.snapshot(clock(now, 10_001)).reason,
        Reason::ClockUnavailable
    );
    assert!(
        control
            .set_override(control.revision(), request(true), unknown)
            .is_err()
    );
}

#[test]
fn monotonic_reversal_and_expiry_overflow_cannot_enable_recording() {
    let now = Instant::now();
    let mut control = Control::new(Mode::Main);
    let revision = control.revision();
    assert!(
        control
            .set_override(revision, request(true), clock(now, i64::MAX))
            .is_err()
    );
    assert_eq!(control.revision(), revision);
    control
        .set_override(
            revision,
            request(true),
            clock(now + Duration::from_secs(1), 10_000),
        )
        .unwrap();
    assert_eq!(
        control.snapshot(clock(now, 10_000)).reason,
        Reason::ClockUnavailable
    );
    assert_eq!(
        control
            .snapshot(clock(now + Duration::from_secs(1), 10_001))
            .mode,
        Mode::Off
    );
}

#[test]
fn reconfiguration_preserves_privacy_and_invalidates_old_requests() {
    let now = Instant::now();
    let mut control = Control::new(Mode::Both);
    control
        .set_override(control.revision(), request(false), clock(now, 10_000))
        .unwrap();
    control.set_privacy(Some(true));
    let old = control.revision();
    control.configure(Mode::Main);
    assert!(control.clear_override(old, clock(now, 10_000)).is_err());
    assert_eq!(control.snapshot(clock(now, 10_000)).reason, Reason::Privacy);
    control.set_privacy(Some(false));
    assert_eq!(control.snapshot(clock(now, 10_000)).mode, Mode::Main);
    assert!(control.snapshot(clock(now, 10_000)).request.is_none());
}

#[test]
fn expiry_and_invalid_clock_invalidate_cas_independently_of_observers() {
    let now = Instant::now();
    for observe in [false, true] {
        let mut control = Control::new(Mode::Main);
        control
            .set_override(control.revision(), request(false), clock(now, 10_000))
            .unwrap();
        let old = control.revision();
        let expired = clock(now + Duration::from_secs(1), 11_000);
        if observe {
            control.snapshot(expired);
        }
        assert!(control.clear_override(old, expired).is_err());
        assert!(control.set_override(old, request(false), expired).is_err());
        assert_eq!(control.snapshot(expired).mode, Mode::Main);
    }
    let mut control = Control::new(Mode::Main);
    control
        .set_override(control.revision(), request(true), clock(now, 10_000))
        .unwrap();
    let old = control.revision();
    let unknown = Clock {
        monotonic: now,
        utc_ms: None,
    };
    let invalid = control.snapshot(unknown).revision;
    assert_ne!(invalid, old);
    assert_eq!(control.snapshot(unknown).revision, invalid);
    assert!(control.clear_override(old, clock(now, 10_001)).is_err());
    assert_eq!(control.snapshot(clock(now, 10_001)).mode, Mode::Off);
}
