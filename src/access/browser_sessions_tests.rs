use super::{Binding, Limits, Sessions};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn sessions() -> Sessions {
    Sessions::new(Limits {
        idle: Duration::from_secs(30),
        absolute: Duration::from_secs(120),
        per_identity: 2,
        per_address: 3,
    })
}

#[test]
fn rotation_replaces_bootstrap_at_capacity_without_leaving_an_old_handle() {
    let mut sessions = Sessions::new(Limits {
        idle: Duration::from_secs(30),
        absolute: Duration::from_secs(120),
        per_identity: 1,
        per_address: 1,
    });
    let now = Instant::now();
    let origin = "https://keeppeek.example";
    let anonymous = sessions
        .issue(None, origin, "203.0.113.8".parse().unwrap(), now, 0)
        .unwrap();
    let binding = Binding {
        identity_id: Uuid::new_v4(),
        revision: 1,
    };
    let issued = sessions
        .rotate(anonymous.session_id, binding, now, 1)
        .unwrap();
    assert_ne!(issued.cookie, anonymous.cookie);
    assert_ne!(issued.csrf, anonymous.csrf);
    assert!(sessions.lookup(&anonymous.cookie, origin, now).is_none());
    assert_eq!(
        sessions
            .lookup(&issued.cookie, origin, now)
            .unwrap()
            .binding,
        Some(binding)
    );
    assert_eq!(sessions.list().count(), 1);
    assert!(
        sessions
            .rotate(
                issued.session_id,
                binding,
                now + Duration::from_secs(30),
                30_000
            )
            .is_err()
    );
}

#[test]
fn browser_sessions_bind_cookie_origin_and_csrf_without_storing_raw_handle() {
    let mut sessions = sessions();
    let now = Instant::now();
    let binding = Binding {
        identity_id: Uuid::new_v4(),
        revision: 1,
    };
    let issued = sessions
        .issue(
            Some(binding),
            "https://keeppeek.example",
            "203.0.113.8".parse().unwrap(),
            now,
            1_000,
        )
        .unwrap();
    let current = sessions
        .authenticate(&issued.cookie, "https://keeppeek.example", now)
        .unwrap();
    assert_eq!(current.binding, Some(binding));
    assert!(current.csrf_matches(&issued.csrf));
    assert!(!current.csrf_matches("wrong"));
    assert!(
        sessions
            .authenticate(&issued.cookie, "https://other.example", now)
            .is_none()
    );
    assert!(!format!("{sessions:?}").contains(&issued.cookie));
    assert!(!format!("{issued:?}").contains(&issued.cookie));
    assert!(!format!("{current:?}").contains(&issued.csrf));
    let forged = format!("{}.{}", current.id.simple(), "x".repeat(43));
    assert!(
        sessions
            .authenticate(&forged, "https://keeppeek.example", now)
            .is_none()
    );
}

#[test]
fn browser_sessions_expire_idle_and_absolute_and_revoke_immediately() {
    let mut sessions = sessions();
    let now = Instant::now();
    let binding = Binding {
        identity_id: Uuid::new_v4(),
        revision: 4,
    };
    let origin = "https://keeppeek.example";
    let peer = "203.0.113.8".parse().unwrap();
    let issued = sessions.issue(Some(binding), origin, peer, now, 0).unwrap();
    assert!(
        sessions
            .authenticate(&issued.cookie, origin, now + Duration::from_secs(30))
            .is_none()
    );
    let issued = sessions.issue(Some(binding), origin, peer, now, 0).unwrap();
    for seconds in [20, 40, 60, 80, 100] {
        assert!(
            sessions
                .authenticate(&issued.cookie, origin, now + Duration::from_secs(seconds))
                .is_some()
        );
    }
    assert!(
        sessions
            .authenticate(&issued.cookie, origin, now + Duration::from_secs(120))
            .is_none()
    );
    let issued = sessions.issue(Some(binding), origin, peer, now, 0).unwrap();
    let session = sessions.authenticate(&issued.cookie, origin, now).unwrap();
    sessions.revoke(session.id);
    assert!(!sessions.active(session.id, binding, now));
    assert!(sessions.authenticate(&issued.cookie, origin, now).is_none());
}

#[test]
fn browser_sessions_enforce_identity_and_address_limits_and_remove_expired_entries() {
    let mut sessions = sessions();
    let now = Instant::now();
    let binding = Binding {
        identity_id: Uuid::new_v4(),
        revision: 1,
    };
    let origin = "https://keeppeek.example";
    let peer = "203.0.113.8".parse().unwrap();
    for _ in 0..2 {
        sessions.issue(Some(binding), origin, peer, now, 0).unwrap();
    }
    assert!(sessions.issue(Some(binding), origin, peer, now, 0).is_err());
    sessions.issue(None, origin, peer, now, 0).unwrap();
    assert!(sessions.issue(None, origin, peer, now, 0).is_err());
    sessions.expire(now + Duration::from_secs(301));
    assert!(
        sessions
            .issue(
                Some(binding),
                origin,
                peer,
                now + Duration::from_secs(301),
                301_000
            )
            .is_ok()
    );
}
