use super::*;

fn peer(index: u8) -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, index))
}

#[test]
fn per_address_action_limits_do_not_charge_other_actions_or_reset_on_rejection() {
    let now = Instant::now();
    let mut budget = Budget::new(now);
    for _ in 0..20 {
        budget.admit(peer(1), Action::Exchange, now).unwrap();
    }
    assert_eq!(budget.admit(peer(1), Action::Exchange, now), Err(Limited));
    budget.admit(peer(1), Action::Status, now).unwrap();
    budget.admit(peer(2), Action::Exchange, now).unwrap();
    assert_eq!(
        budget.admit(peer(1), Action::Exchange, now + Duration::from_secs(59)),
        Err(Limited)
    );
    budget
        .admit(peer(1), Action::Exchange, now + Duration::from_secs(60))
        .unwrap();
}

#[test]
fn global_action_limits_apply_across_actual_transport_peers() {
    let now = Instant::now();
    let mut budget = Budget::new(now);
    for index in 0..6 {
        for _ in 0..20 {
            budget.admit(peer(index), Action::Exchange, now).unwrap();
        }
    }
    assert_eq!(budget.admit(peer(7), Action::Exchange, now), Err(Limited));
    budget.admit(peer(7), Action::Status, now).unwrap();
}

#[test]
fn active_address_windows_are_never_evicted_to_admit_new_attacker_keys() {
    let now = Instant::now();
    let mut budget = Budget::new(now);
    for index in 0..128 {
        budget.admit(peer(index), Action::Status, now).unwrap();
    }
    assert_eq!(budget.addresses.len(), 128);
    assert_eq!(budget.admit(peer(128), Action::Status, now), Err(Limited));
    budget.admit(peer(0), Action::Status, now).unwrap();
    budget
        .admit(peer(128), Action::Status, now + Duration::from_secs(60))
        .unwrap();
    assert_eq!(budget.addresses.len(), 1);
}

#[test]
fn all_action_ceiling_values_are_exercised_at_exact_boundary() {
    for (action, limit) in [
        (Action::Status, 240),
        (Action::Media, 120),
        (Action::Download, 12),
    ] {
        let now = Instant::now();
        let mut budget = Budget::new(now);
        for _ in 0..limit {
            budget.admit(peer(1), action, now).unwrap();
        }
        assert_eq!(budget.admit(peer(1), action, now), Err(Limited));
    }
}

#[test]
fn egress_caps_both_peer_and_global_bursts_without_refunding_failed_delivery() {
    let now = Instant::now();
    let mut budget = Budget::new(now);
    budget.admit(peer(1), Action::Media, now).unwrap();
    assert_eq!(
        budget.egress_delay(peer(1), 65_536, now).unwrap(),
        Duration::ZERO
    );
    let wait = budget.egress_delay(peer(1), 65_536, now).unwrap();
    assert!(wait > Duration::ZERO && wait <= Duration::from_millis(4));
    assert_eq!(
        budget.egress_delay(peer(1), 65_536, now + wait).unwrap(),
        Duration::ZERO
    );
    assert!(budget.egress_delay(peer(1), 65_536, now + wait).unwrap() > Duration::ZERO);
    let mut global = Budget::new(now);
    for index in 1..=4 {
        global.admit(peer(index), Action::Media, now).unwrap();
        assert_eq!(
            global.egress_delay(peer(index), 65_536, now).unwrap(),
            Duration::ZERO
        );
    }
    global.admit(peer(5), Action::Media, now).unwrap();
    assert!(global.egress_delay(peer(5), 65_536, now).unwrap() > Duration::ZERO);
    assert_eq!(global.egress_delay(peer(5), 65_537, now), Err(Limited));
}

#[test]
fn missing_address_and_zero_bytes_cannot_bypass_egress_admission() {
    let now = Instant::now();
    let mut budget = Budget::new(now);
    assert_eq!(budget.egress_delay(peer(1), 1, now), Err(Limited));
    budget.admit(peer(1), Action::Media, now).unwrap();
    assert_eq!(budget.egress_delay(peer(1), 0, now), Err(Limited));
}

#[test]
fn active_egress_survives_request_window_reset_without_granting_another_burst() {
    let now = Instant::now();
    let mut budget = Budget::new(now);
    budget.admit(peer(1), Action::Media, now).unwrap();
    let active = now + Duration::from_secs(60);
    assert_eq!(
        budget.egress_delay(peer(1), 65_536, active).unwrap(),
        Duration::ZERO
    );
    budget.admit(peer(1), Action::Media, active).unwrap();
    assert!(budget.egress_delay(peer(1), 65_536, active).unwrap() > Duration::ZERO);
    assert_eq!(budget.addresses.len(), 1);
    assert_eq!(budget.addresses[&peer(1)].window.counts[2], 1);
}
