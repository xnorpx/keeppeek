use keeppeek::{
    config::retention::{
        Settings,
        evidence::{EventOutcome, UnavailableReason},
    },
    storage::{
        metadata::TimelineEvent,
        retention::{Evidence, EvidenceKind, Interval},
    },
};

fn settings() -> Settings {
    toml::from_str("event_mappings=[{source='camera',kind='Motion',evidence='motion'}]").unwrap()
}

fn event() -> TimelineEvent {
    serde_json::from_value(serde_json::json!({
        "id": "event-1", "revision": 1, "camera_id": "192.0.2.8",
        "source": "camera", "kind": "Motion", "start_time_ms": 1000,
        "end_time_ms": 2000, "attachments": [], "icon_key": "motion"
    }))
    .unwrap()
}

#[test]
fn retention_evidence_matches_exact_origin_and_closed_interval() {
    let settings = settings();
    let ip = "192.0.2.8".parse().unwrap();
    let mut event = event();
    for stream in ["main", "sub"] {
        let normalized = settings.normalize_event(ip, stream, &event).unwrap();
        assert_eq!(normalized.event_id, "event-1");
        assert_eq!(normalized.revision, 1);
        assert_eq!(
            normalized.outcome,
            EventOutcome::Available(Evidence::new(
                EvidenceKind::Motion,
                Interval::new(1000, 2000).unwrap()
            ))
        );
    }
    for kind in ["motion", "person", "unknown"] {
        event.kind = kind.into();
        assert_eq!(
            settings
                .normalize_event(ip, "main", &event)
                .unwrap()
                .outcome,
            EventOutcome::Unavailable(UnavailableReason::UnknownKind)
        );
    }
    event.kind = "Motion".into();
    event.source = keeppeek::storage::metadata::EventSource::KeepPeek;
    assert_eq!(
        settings
            .normalize_event(ip, "main", &event)
            .unwrap()
            .outcome,
        EventOutcome::Unavailable(UnavailableReason::UnknownKind)
    );
    event.source = keeppeek::storage::metadata::EventSource::Camera;
    event.camera_id = "2001:db8::1".into();
    assert!(matches!(
        settings
            .normalize_event("2001:db8::1".parse().unwrap(), "main", &event)
            .unwrap()
            .outcome,
        EventOutcome::Available(_)
    ));
}

#[test]
fn retention_evidence_isolates_camera_and_stream_without_hiding_malformed_scope() {
    let settings = settings();
    let ip = "192.0.2.8".parse().unwrap();
    let mut event = event();
    event.camera_id = "192.0.2.9".into();
    assert_eq!(
        settings
            .normalize_event(ip, "main", &event)
            .unwrap()
            .outcome,
        EventOutcome::NotApplicable
    );
    for invalid in ["", "camera-name", "2001:0db8::1"] {
        event.camera_id = invalid.into();
        assert_eq!(
            settings
                .normalize_event(ip, "main", &event)
                .unwrap()
                .outcome,
            EventOutcome::Unavailable(UnavailableReason::InvalidIdentity)
        );
    }
    event.camera_id = "192.0.2.8".into();
    event.stream = Some("sub".into());
    assert_eq!(
        settings
            .normalize_event(ip, "main", &event)
            .unwrap()
            .outcome,
        EventOutcome::NotApplicable
    );
    for invalid in ["", "aux"] {
        event.stream = Some(invalid.into());
        assert_eq!(
            settings
                .normalize_event(ip, "main", &event)
                .unwrap()
                .outcome,
            EventOutcome::Unavailable(UnavailableReason::InvalidIdentity)
        );
    }
    assert!(settings.normalize_event(ip, "aux", &event).is_err());
}

#[test]
fn retention_evidence_keeps_revision_identity_when_coverage_becomes_unavailable() {
    let settings = settings();
    let ip = "192.0.2.8".parse().unwrap();
    let mut event = event();
    event.revision = 2;
    event.end_time_ms = None;
    let unavailable = settings.normalize_event(ip, "main", &event).unwrap();
    assert_eq!((unavailable.event_id, unavailable.revision), ("event-1", 2));
    assert_eq!(
        unavailable.outcome,
        EventOutcome::Unavailable(UnavailableReason::OpenInterval)
    );
    for end in [999, 1000] {
        event.end_time_ms = Some(end);
        assert_eq!(
            settings
                .normalize_event(ip, "main", &event)
                .unwrap()
                .outcome,
            EventOutcome::Unavailable(UnavailableReason::InvalidInterval)
        );
    }
    for revision in [0, u64::MAX] {
        event.revision = revision;
        assert_eq!(
            settings
                .normalize_event(ip, "main", &event)
                .unwrap()
                .outcome,
            EventOutcome::Unavailable(UnavailableReason::InvalidIdentity)
        );
    }
    event.revision = 1;
    event.id.clear();
    assert_eq!(
        settings
            .normalize_event(ip, "main", &event)
            .unwrap()
            .outcome,
        EventOutcome::Unavailable(UnavailableReason::InvalidIdentity)
    );
}
