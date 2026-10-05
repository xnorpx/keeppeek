use keeppeek::storage::metadata::{EventSource, TimelineEvent};
use keeppeek::storage::retention::{Interval, Policy, Predicate, Recording, Rule};
use keeppeek::storage::retention::{MAX_EVENTS, MAX_RULES, Reason};

fn recording() -> Recording<'static> {
    Recording {
        camera_id: "front",
        stream_id: "main",
        interval: Interval::new(0, 10_000).unwrap(),
        protected: false,
        committed_deadline_ms: None,
    }
}

fn event(kind: &str, start: i64, end: Option<i64>) -> TimelineEvent {
    TimelineEvent {
        id: "event-1".into(),
        revision: 1,
        camera_id: "front".into(),
        stream: None,
        source: EventSource::KeepPeek,
        kind: kind.into(),
        start_time_ms: start,
        end_time_ms: end,
        confidence: None,
        bbox: None,
        bbox_attachment_id: None,
        zone: None,
        text: None,
        payload: None,
        attachments: vec![],
        canonical_attachment_id: None,
        icon_key: "motion".into(),
        rejected_icon_key: None,
        thumbnail_filename: None,
    }
}

fn motion_policy() -> Policy {
    Policy::new(vec![
        Rule::new("motion", 43_200_000, Predicate::Motion).unwrap(),
    ])
    .unwrap()
}

#[test]
fn half_open_intervals_do_not_match_boundary_only_events() {
    let policy = motion_policy();
    for (start, end, matches) in [
        (-10, Some(0), false),
        (10_000, Some(11_000), false),
        (9_999, Some(10_001), true),
        (0, Some(0), true),
        (10_000, Some(10_000), false),
        (-10, None, true),
        (10_000, None, false),
    ] {
        let decision = policy
            .resolve(recording(), &[event("motion", start, end)])
            .unwrap();
        assert_eq!(decision.deadline_ms.is_some(), matches, "{start}, {end:?}");
        if matches {
            assert_eq!(decision.deadline_ms, Some(43_210_000));
        }
    }
}

#[test]
fn detections_and_other_camera_streams_do_not_fabricate_motion() {
    let policy = motion_policy();
    let person = event("person", 1, Some(2));
    let mut other_camera = event("motion", 1, Some(2));
    other_camera.camera_id = "back".into();
    let mut other_stream = event("motion", 1, Some(2));
    other_stream.stream = Some("sub".into());
    for observation in [person, other_camera, other_stream] {
        let decision = policy.resolve(recording(), &[observation]).unwrap();
        assert_eq!(decision.reason, Reason::NoMatchingEvidence);
        assert!(decision.expired(10_000));
    }
    let policy = Policy::new(vec![
        Rule::new(
            "person",
            86_400_000,
            Predicate::Event {
                event_type: "person".into(),
            },
        )
        .unwrap(),
    ])
    .unwrap();
    assert_eq!(
        policy
            .resolve(recording(), &[event("person", 1, Some(2))])
            .unwrap()
            .deadline_ms,
        Some(86_410_000)
    );
}

#[test]
fn zero_disables_rules_and_existing_deadlines_never_shorten() {
    let disabled = Policy::new(vec![
        Rule::new("continuous", 0, Predicate::Continuous).unwrap(),
    ])
    .unwrap();
    assert!(
        disabled
            .resolve(recording(), &[])
            .unwrap()
            .matching_rules
            .is_empty()
    );
    let mut saved = recording();
    saved.committed_deadline_ms = Some(90_000_000);
    for policy in [disabled, motion_policy()] {
        let decision = policy.resolve(saved, &[]).unwrap();
        assert_eq!(decision.deadline_ms, saved.committed_deadline_ms);
        assert_eq!(decision.reason, Reason::CommittedDeadline);
        assert!(!decision.expired(89_999_999));
        assert!(decision.expired(90_000_000));
    }
}

#[test]
fn protected_evidence_dominates_expiration() {
    let mut held = recording();
    held.protected = true;
    let policy = motion_policy();
    let decision = policy.resolve(held, &[]).unwrap();
    assert_eq!(decision.reason, Reason::Protected);
    assert!(!decision.expired(i64::MAX));
}

#[test]
fn invalid_and_duplicate_current_revisions_fail_closed() {
    let policy = motion_policy();
    assert!(
        policy
            .resolve(recording(), &[event("motion", 2, Some(1))])
            .is_err()
    );
    let first = event("motion", 1, Some(2));
    let mut revision = first.clone();
    revision.revision = 2;
    assert!(policy.resolve(recording(), &[first, revision]).is_err());
    let mut missing_identity = recording();
    missing_identity.camera_id = "";
    assert!(policy.resolve(missing_identity, &[]).is_err());
}

#[test]
fn rule_event_and_timestamp_limits_are_enforced() {
    assert!(Interval::new(0, 0).is_err());
    assert!(Interval::new(1, 0).is_err());
    assert!(Rule::new("duration", u64::MAX, Predicate::Continuous).is_err());
    assert!(Rule::new("", 1, Predicate::Continuous).is_err());
    assert!(Rule::new("x".repeat(129), 1, Predicate::Continuous).is_err());
    let rules: Vec<_> = (0..MAX_RULES)
        .map(|i| Rule::new(format!("rule-{i}"), 1, Predicate::Continuous).unwrap())
        .collect();
    assert!(Policy::new(rules.clone()).is_ok());
    let mut excess = rules.clone();
    excess.push(Rule::new("extra", 1, Predicate::Continuous).unwrap());
    assert!(Policy::new(excess).is_err());
    assert!(Policy::new(vec![rules[0].clone(), rules[0].clone()]).is_err());
    let policy = motion_policy();
    let events: Vec<_> = (0..MAX_EVENTS)
        .map(|i| {
            let mut e = event("motion", 1, Some(2));
            e.id = format!("e-{i}");
            e
        })
        .collect();
    assert!(policy.resolve(recording(), &events).is_ok());
    let mut excess = events;
    excess.push(event("motion", 1, Some(2)));
    assert!(policy.resolve(recording(), &excess).is_err());
    let mut overflow = recording();
    overflow.interval = Interval::new(i64::MAX - 1, i64::MAX).unwrap();
    assert!(Policy::new(rules).unwrap().resolve(overflow, &[]).is_err());
}

#[test]
fn serialization_cannot_bypass_policy_validation() {
    let policy = motion_policy();
    let encoded = toml::to_string(&policy).unwrap();
    assert_eq!(toml::from_str::<Policy>(&encoded).unwrap(), policy);
    for invalid in [
        r#"{"rules":[{"id":"motion","duration_ms":-1,"predicate":{"kind":"motion"}}]}"#,
        r#"{"rules":[],"ignored":true}"#,
        r#"{"rules":[{"id":"motion","duration_ms":1,"predicate":{"kind":"unknown"}}]}"#,
    ] {
        assert!(serde_json::from_str::<Policy>(invalid).is_err());
    }
    let rule = serde_json::json!({"id":"same", "duration_ms":1, "predicate":{"kind":"motion"}});
    assert!(
        serde_json::from_value::<Policy>(serde_json::json!({"rules":[rule.clone(), rule.clone()]}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<Policy>(serde_json::json!({"rules":vec![rule; MAX_RULES + 1]}))
            .is_err()
    );
}

#[test]
fn overlapping_rules_keep_the_latest_deadline_independent_of_order() {
    let short = Rule::new("continuous", 43_200_000, Predicate::Continuous).unwrap();
    let long = Rule::new("longer", 172_800_000, Predicate::Continuous).unwrap();
    for rules in [
        vec![short.clone(), long.clone()],
        vec![long.clone(), short.clone()],
    ] {
        let policy = Policy::new(rules).unwrap();
        let decision = policy
            .resolve(
                Recording {
                    camera_id: "front",
                    stream_id: "main",
                    interval: Interval::new(0, 10_000).unwrap(),
                    protected: false,
                    committed_deadline_ms: None,
                },
                &[],
            )
            .unwrap();
        assert_eq!(decision.deadline_ms, Some(172_810_000));
        assert_eq!(decision.matching_rules.len(), 2);
    }
}
