use keeppeek::storage::retention::{
    Evidence, EvidenceKind, Interval, RetentionMode, RetentionPolicy, RetentionRule, RuleClass,
};

const DAY_MS: u64 = 86_400_000;
const START_MS: i64 = 1_800_403_200_000;

fn interval(start_seconds: i64, end_seconds: i64) -> Interval {
    Interval::new(
        START_MS + start_seconds * 1_000,
        START_MS + end_seconds * 1_000,
    )
    .unwrap()
}

fn rule(class: RuleClass, days: u64, mode: RetentionMode) -> RetentionRule {
    RetentionRule::new(class, days * DAY_MS, mode).unwrap()
}

#[test]
fn recording_policy_examples_resolve_expected_deadlines() {
    let evidence = [
        Evidence::new(EvidenceKind::Motion, interval(10, 30)),
        Evidence::new(EvidenceKind::Alert, interval(20, 30)),
        Evidence::new(EvidenceKind::Detection, interval(20, 30)),
    ];
    let examples = [
        (
            vec![
                rule(RuleClass::Continuous, 3, RetentionMode::All),
                rule(RuleClass::Motion, 7, RetentionMode::Motion),
                rule(RuleClass::Alert, 30, RetentionMode::All),
                rule(RuleClass::Detection, 30, RetentionMode::All),
            ],
            [Some(3), Some(7), Some(30)],
        ),
        (
            vec![
                rule(RuleClass::Motion, 3, RetentionMode::Motion),
                rule(RuleClass::Alert, 30, RetentionMode::Motion),
                rule(RuleClass::Detection, 30, RetentionMode::Motion),
            ],
            [None, Some(3), Some(30)],
        ),
        (
            vec![rule(RuleClass::Alert, 30, RetentionMode::Motion)],
            [None, None, Some(30)],
        ),
    ];
    for (rules, expected_days) in examples {
        let policy = RetentionPolicy::new(rules).unwrap();
        for (index, expected_days) in expected_days.into_iter().enumerate() {
            let start = i64::try_from(index).unwrap() * 10;
            let media = interval(start, start + 10);
            let decision = policy.resolve(media, &evidence, None).unwrap();
            let expected = expected_days.map(|days| media.end_ms() + days * DAY_MS as i64);
            assert_eq!(decision.deadline_ms, expected);
        }
    }
}

#[test]
fn recording_policy_sub_day_expiry_is_exclusive_and_revisions_never_shorten_it() {
    let media = interval(0, 10);
    let policy = RetentionPolicy::new(vec![
        RetentionRule::new(RuleClass::Continuous, DAY_MS / 2, RetentionMode::All).unwrap(),
        rule(RuleClass::Alert, 0, RetentionMode::All),
    ])
    .unwrap();
    let decision = policy.resolve(media, &[], None).unwrap();
    let deadline = START_MS + 10_000 + 43_200_000;
    assert_eq!(decision.deadline_ms, Some(deadline));
    assert!(!decision.expired_at(deadline - 1));
    assert!(decision.expired_at(deadline));
    let revised = RetentionPolicy::new(vec![]).unwrap();
    assert_eq!(
        revised.resolve(media, &[], Some(deadline)).unwrap(),
        decision
    );
}

#[test]
fn recording_policy_event_and_motion_must_overlap_at_the_same_time() {
    let policy =
        RetentionPolicy::new(vec![rule(RuleClass::Alert, 30, RetentionMode::Motion)]).unwrap();
    let evidence = [
        Evidence::new(EvidenceKind::Alert, interval(0, 10)),
        Evidence::new(EvidenceKind::Motion, interval(10, 20)),
    ];
    assert_eq!(
        policy
            .resolve(interval(0, 30), &evidence, None)
            .unwrap()
            .deadline_ms,
        None
    );
}

#[test]
fn recording_policy_overlap_order_and_duplicate_evidence_do_not_change_expiry() {
    let media = interval(0, 60);
    let rules = vec![
        rule(RuleClass::Continuous, 1, RetentionMode::All),
        rule(RuleClass::Alert, 30, RetentionMode::ActiveObjects),
    ];
    let evidence = [
        Evidence::new(EvidenceKind::Alert, interval(10, 50)),
        Evidence::new(EvidenceKind::ActiveObject, interval(20, 30)),
    ];
    let policy = RetentionPolicy::new(rules.clone()).unwrap();
    let expected = Some(START_MS + 30_000 + 30 * DAY_MS as i64);
    assert_eq!(
        policy.resolve(media, &evidence, None).unwrap().deadline_ms,
        expected
    );
    let reversed = RetentionPolicy::new(rules.into_iter().rev().collect()).unwrap();
    let duplicated = [evidence[1], evidence[0], evidence[1]];
    assert_eq!(
        reversed
            .resolve(media, &duplicated, None)
            .unwrap()
            .deadline_ms,
        expected
    );
    let older_deadline = START_MS + DAY_MS as i64;
    assert_eq!(
        policy
            .resolve(media, &evidence, Some(older_deadline))
            .unwrap()
            .deadline_ms,
        expected
    );
}

#[test]
fn recording_policy_rejects_overflow_and_unbounded_inputs_without_an_expiry_decision() {
    assert!(Interval::new(0, 0).is_err());
    assert!(Interval::new(10, 0).is_err());
    assert!(RetentionRule::new(RuleClass::Continuous, u64::MAX, RetentionMode::All).is_err());
    assert!(
        RetentionPolicy::new(vec![rule(RuleClass::Continuous, 1, RetentionMode::All); 17]).is_err()
    );
    let policy =
        RetentionPolicy::new(vec![rule(RuleClass::Continuous, 1, RetentionMode::All)]).unwrap();
    assert!(
        policy
            .resolve(Interval::new(i64::MAX - 1, i64::MAX).unwrap(), &[], None)
            .is_err()
    );
    let evidence = vec![Evidence::new(EvidenceKind::Motion, interval(0, 1)); 257];
    assert!(policy.resolve(interval(0, 1), &evidence, None).is_err());
    let no_rules = RetentionPolicy::new(vec![]).unwrap();
    assert!(
        !no_rules
            .resolve(interval(0, 1), &[], None)
            .unwrap()
            .expired_at(i64::MAX)
    );
}

#[test]
fn recording_policy_motion_does_not_imply_active_objects_and_day_boundaries_are_utc() {
    let policy = RetentionPolicy::new(vec![rule(
        RuleClass::Alert,
        1,
        RetentionMode::ActiveObjects,
    )])
    .unwrap();
    let crossing_midnight = Interval::new(86_399_000, 86_401_000).unwrap();
    let evidence = [
        Evidence::new(EvidenceKind::Alert, crossing_midnight),
        Evidence::new(EvidenceKind::Motion, crossing_midnight),
    ];
    assert_eq!(
        policy
            .resolve(crossing_midnight, &evidence, None)
            .unwrap()
            .deadline_ms,
        None
    );
    let active = [
        evidence[0],
        Evidence::new(EvidenceKind::ActiveObject, crossing_midnight),
    ];
    assert_eq!(
        policy
            .resolve(crossing_midnight, &active, None)
            .unwrap()
            .deadline_ms,
        Some(172_801_000)
    );
}

#[test]
fn recording_policy_zero_rules_disable_matches_but_preserve_committed_deadlines() {
    let policy = RetentionPolicy::new(vec![rule(RuleClass::Alert, 0, RetentionMode::All)]).unwrap();
    let media = interval(0, 10);
    let evidence = [Evidence::new(EvidenceKind::Alert, media)];
    assert_eq!(
        policy.resolve(media, &evidence, None).unwrap().deadline_ms,
        None
    );
    assert_eq!(
        policy
            .resolve(media, &evidence, Some(START_MS + 90_000))
            .unwrap()
            .deadline_ms,
        Some(START_MS + 90_000)
    );
}

#[test]
fn recording_policy_latest_deadline_is_not_necessarily_the_longest_duration() {
    let policy = RetentionPolicy::new(vec![
        RetentionRule::new(RuleClass::Alert, 10_000, RetentionMode::All).unwrap(),
        RetentionRule::new(RuleClass::Detection, 1_000, RetentionMode::All).unwrap(),
    ])
    .unwrap();
    let evidence = [
        Evidence::new(EvidenceKind::Alert, interval(0, 1)),
        Evidence::new(EvidenceKind::Detection, interval(20, 21)),
    ];
    assert_eq!(
        policy
            .resolve(interval(0, 30), &evidence, None)
            .unwrap()
            .deadline_ms,
        Some(START_MS + 22_000)
    );
}
