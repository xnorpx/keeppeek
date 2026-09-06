use super::Tracker;
use crate::keeppeek::KeepPeekEvent;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

fn event(kind: &str, state: &str, extra: &str) -> ::isapi::Event {
    ::isapi::Event::parse(format!("<EventNotificationAlert><eventType>{kind}</eventType><eventState>{state}</eventState><channelID>1</channelID>{extra}</EventNotificationAlert>")).unwrap()
}

fn tracker(generic_motion: bool) -> Tracker {
    Tracker::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 1, generic_motion)
}

#[test]
fn repeated_active_alarms_share_one_interval_until_explicit_clear() {
    let mut tracker = tracker(true);
    let now = Instant::now();
    let active = event("VMD", "active", "<dateTime>2020-01-01T00:00:00Z</dateTime>");
    let first = tracker.apply(&active, now, 1000).unwrap();
    assert!(first.activity);
    let [KeepPeekEvent::TimelineEventStarted { event: started }] = first.changes.as_slice() else {
        panic!("expected one interval start")
    };
    assert_eq!(started.kind, "motion");
    assert_eq!(started.camera_id, "192.0.2.10");
    assert_eq!(started.start_time_ms, 1000);
    assert_eq!(
        started.payload.as_ref().unwrap()["camera_time"],
        "2020-01-01T00:00:00Z"
    );
    let repeated = tracker
        .apply(&active, now + Duration::from_secs(1), 2000)
        .unwrap();
    assert!(repeated.activity);
    assert!(repeated.changes.is_empty());
    let clear = tracker
        .apply(
            &event("VMD", "inactive", ""),
            now + Duration::from_secs(2),
            3000,
        )
        .unwrap();
    let [KeepPeekEvent::TimelineEventEnded { id, end_time_ms }] = clear.changes.as_slice() else {
        panic!("expected one clear")
    };
    assert_eq!(id, &started.id);
    assert_eq!(*end_time_ms, 3000);
    assert!(
        tracker
            .apply(&event("VMD", "inactive", ""), now, 4000)
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn classification_requires_event_evidence_and_obeys_generic_motion_policy() {
    let mut tracker = tracker(false);
    let now = Instant::now();
    assert!(
        !tracker
            .apply(&event("VMD", "active", ""), now, 1000)
            .unwrap()
            .activity
    );
    for (target, expected) in [("human", "person"), ("vehicle", "vehicle")] {
        let input = event(
            "VMD",
            "active",
            &format!("<detectionTarget>{target}</detectionTarget>"),
        );
        let output = tracker.apply(&input, now, 2000).unwrap();
        let [KeepPeekEvent::TimelineEventStarted { event }] = output.changes.as_slice() else {
            panic!("expected classified event")
        };
        assert_eq!(event.kind, expected);
    }
    assert_eq!(
        tracker
            .apply(&event("VMD", "inactive", ""), now, 3000)
            .unwrap()
            .changes
            .len(),
        2
    );
}

#[test]
fn heartbeats_unknown_events_and_other_channels_do_not_create_timeline_events() {
    let mut tracker = tracker(true);
    let now = Instant::now();
    for input in [
        event("videoloss", "inactive", ""),
        event("heartBeat", "active", ""),
        event("unknownRule", "active", ""),
        event("VMD", "pulse", ""),
        event("VMD", "active", "<dynChannelID>2</dynChannelID>"),
    ] {
        let output = tracker.apply(&input, now, 1000).unwrap();
        assert!(!output.activity);
        assert!(output.changes.is_empty());
    }
}

#[test]
fn reconnect_gap_ends_only_the_observed_span_and_allows_a_new_start() {
    let mut tracker = tracker(true);
    let now = Instant::now();
    let active = event("VMD", "active", "");
    tracker.apply(&active, now, 1000).unwrap();
    tracker
        .apply(&active, now + Duration::from_secs(1), 2000)
        .unwrap();
    let ended = tracker.disconnect();
    let [KeepPeekEvent::TimelineEventEnded { end_time_ms, .. }] = ended.as_slice() else {
        panic!("expected end of observed interval")
    };
    assert_eq!(*end_time_ms, 2000);
    assert_eq!(
        tracker
            .apply(&active, now + Duration::from_secs(10), 11000)
            .unwrap()
            .changes
            .len(),
        1
    );
}

#[test]
fn missing_clear_expires_at_last_evidence_and_heartbeats_do_not_extend_motion() {
    let mut tracker = tracker(true);
    let now = Instant::now();
    tracker
        .apply(&event("VMD", "active", ""), now, 1000)
        .unwrap();
    tracker
        .apply(
            &event("videoloss", "inactive", ""),
            now + Duration::from_secs(10),
            11000,
        )
        .unwrap();
    assert!(tracker.expire(now + Duration::from_secs(29)).is_empty());
    let ended = tracker.expire(now + Duration::from_secs(30));
    let [KeepPeekEvent::TimelineEventEnded { end_time_ms, .. }] = ended.as_slice() else {
        panic!("expected bounded observation interval")
    };
    assert_eq!(*end_time_ms, 1000);
}

#[test]
fn distinct_analytics_objects_have_independent_lifecycles_and_retained_evidence() {
    let mut tracker = tracker(false);
    let now = Instant::now();
    let input = ::isapi::Event::parse_json(br#"{"eventType":"targetCapture","eventState":"active","channelID":1,"detectionResult":[{"targetID":"first","targetType":"human","regionID":"gate","confidenceLevel":90,"humanInfo":{"jacketColor":"red"}},{"targetID":"second","targetType":"human","regionID":"gate"}]}"#).unwrap();
    let output = tracker.apply(&input, now, 1000).unwrap();
    assert_eq!(output.changes.len(), 2);
    let KeepPeekEvent::TimelineEventStarted { event: first } = &output.changes[0] else {
        panic!("expected first object")
    };
    let KeepPeekEvent::TimelineEventStarted { event: second } = &output.changes[1] else {
        panic!("expected second object")
    };
    assert_ne!(first.id, second.id);
    assert_eq!(first.kind, "person");
    assert_eq!(first.confidence, Some(0.9));
    assert_eq!(first.zone.as_deref(), Some("gate"));
    assert_eq!(
        first.payload.as_ref().unwrap()["object"]["attributes"]["jacketColor"],
        "red"
    );
    let clear = ::isapi::Event::parse_json(br#"{"eventType":"targetCapture","eventState":"inactive","channelID":1,"detectionResult":[{"targetID":"first","targetType":"human","regionID":"gate"}]}"#).unwrap();
    let output = tracker.apply(&clear, now, 2000).unwrap();
    let [KeepPeekEvent::TimelineEventEnded { id, .. }] = output.changes.as_slice() else {
        panic!("expected one clear")
    };
    assert_eq!(id, &first.id);
    assert_eq!(tracker.disconnect().len(), 1);
}
