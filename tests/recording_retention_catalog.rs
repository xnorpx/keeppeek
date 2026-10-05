use std::path::PathBuf;

use keeppeek::storage::catalog::{CatalogRecording, RecordingCatalog};
use keeppeek::storage::metadata::{EventSource, TimelineEvent};
use keeppeek::storage::retention::{MAX_EVENTS, Reason};
use keeppeek::storage::retention::{Policy, Predicate, Rule};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("keeppeek-retention-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn open(&self) -> RecordingCatalog {
        RecordingCatalog::open(&self.0.join("catalog.db")).unwrap()
    }

    fn execute_sql(&self, sql: &str) {
        let database = pollster::block_on(
            turso::Builder::new_local(self.0.join("catalog.db").to_str().unwrap()).build(),
        )
        .unwrap();
        let connection = database.connect().unwrap();
        pollster::block_on(connection.execute_batch(sql)).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn policy(duration: u64) -> Policy {
    Policy::new(vec![
        Rule::new("continuous", duration, Predicate::Continuous).unwrap(),
    ])
    .unwrap()
}

fn recording() -> CatalogRecording {
    CatalogRecording {
        id: "recording".into(),
        stream_id: "front/main".into(),
        source_id: Some("front".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: 0,
        ended_at_ms: Some(10_000),
        path: "recording.mp4".into(),
        init_offset: 0,
        init_len: 0,
        finalized: true,
    }
}

fn event(id: &str, kind: &str) -> TimelineEvent {
    TimelineEvent {
        id: id.into(),
        revision: 1,
        camera_id: "front".into(),
        stream: None,
        source: EventSource::KeepPeek,
        kind: kind.into(),
        start_time_ms: -100,
        end_time_ms: None,
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
        Rule::new("motion", 50_000, Predicate::Motion).unwrap(),
    ])
    .unwrap()
}

#[test]
fn commitments_survive_restart_and_shorter_policy_revisions() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let handle = catalog.handle();
    handle.upsert_recording(recording()).unwrap();
    let first = handle
        .commit_retention("recording", 1, &policy(50_000))
        .unwrap();
    assert_eq!(first.deadline_ms, Some(60_000));
    drop(handle);
    drop(catalog);

    let catalog = fixture.open();
    let handle = catalog.handle();
    assert_eq!(handle.retention_decision("recording").unwrap(), Some(first));
    let shorter = handle.commit_retention("recording", 2, &policy(1)).unwrap();
    assert_eq!(shorter.deadline_ms, Some(60_000));
    assert!(
        handle
            .commit_retention("recording", 1, &policy(50_000))
            .is_err()
    );
    assert!(handle.commit_retention("recording", 2, &policy(2)).is_err());
    assert_eq!(
        handle.retention_decision("recording").unwrap(),
        Some(shorter)
    );
}

#[test]
fn corrected_events_preserve_commitments_and_file_moves_preserve_identity() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let handle = catalog.handle();
    handle.upsert_recording(recording()).unwrap();
    handle.insert_event(event("event", "motion")).unwrap();
    let first = handle
        .commit_retention("recording", 1, &motion_policy())
        .unwrap();
    assert_eq!(first.deadline_ms, Some(60_000));
    assert!(first.event_revision > 0);
    let mut corrected = event("event", "person");
    corrected.revision = 2;
    handle.insert_event(corrected).unwrap();
    let second = handle
        .commit_retention("recording", 1, &motion_policy())
        .unwrap();
    assert_eq!(second.deadline_ms, first.deadline_ms);
    assert_eq!(second.reason, Reason::CommittedDeadline);
    assert!(second.matching_rules.is_empty());
    assert!(second.event_revision > first.event_revision);
    handle
        .update_recording_path("recording", &fixture.0.join("moved.mp4"), true)
        .unwrap();
    assert_eq!(
        handle.retention_decision("recording").unwrap(),
        Some(second)
    );
}

#[test]
fn invalid_recordings_and_policy_revisions_do_not_create_commitments() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let handle = catalog.handle();
    for invalid in [0, u64::MAX] {
        assert!(
            handle
                .commit_retention("recording", invalid, &policy(1))
                .is_err()
        );
    }
    let mut incomplete = recording();
    incomplete.finalized = false;
    handle.upsert_recording(incomplete).unwrap();
    assert!(handle.commit_retention("recording", 1, &policy(1)).is_err());
    let mut unknown = recording();
    unknown.source_id = None;
    handle.upsert_recording(unknown).unwrap();
    assert!(handle.commit_retention("recording", 1, &policy(1)).is_err());
    assert_eq!(handle.retention_decision("recording").unwrap(), None);
    handle.upsert_recording(recording()).unwrap();
    handle.set_recording_protected("recording", true).unwrap();
    assert_eq!(
        handle
            .commit_retention("recording", 1, &policy(1))
            .unwrap()
            .reason,
        Reason::Protected
    );
    catalog.shutdown();
    assert!(handle.retention_decision("recording").is_err());
    assert!(handle.commit_retention("recording", 1, &policy(1)).is_err());
}

#[test]
fn oversized_event_snapshot_rolls_back_without_shortening_deadline() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let handle = catalog.handle();
    handle.upsert_recording(recording()).unwrap();
    for index in 0..MAX_EVENTS {
        handle
            .insert_event(event(&format!("event-{index}"), "motion"))
            .unwrap();
    }
    let before = handle
        .commit_retention("recording", 1, &motion_policy())
        .unwrap();
    handle.insert_event(event("overflow", "motion")).unwrap();
    let error = handle
        .commit_retention("recording", 2, &policy(1))
        .unwrap_err();
    assert!(error.to_string().contains("snapshot limit"), "{error}");
    assert_eq!(
        handle.retention_decision("recording").unwrap(),
        Some(before)
    );
}

#[test]
fn failed_write_preserves_previous_decision_and_connection_recovers() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let handle = catalog.handle();
    handle.upsert_recording(recording()).unwrap();
    let before = handle
        .commit_retention("recording", 1, &policy(50_000))
        .unwrap();
    drop(handle);
    drop(catalog);
    fixture.execute_sql("CREATE TRIGGER retention_write_failure BEFORE UPDATE ON recording_retention_decisions WHEN NEW.policy_revision = 2 BEGIN SELECT RAISE(ABORT, 'injected retention write failure'); END;");
    let catalog = fixture.open();
    let handle = catalog.handle();
    assert!(
        handle
            .commit_retention("recording", 2, &policy(100_000))
            .is_err()
    );
    assert_eq!(
        handle.retention_decision("recording").unwrap(),
        Some(before)
    );
    let recovered = handle
        .commit_retention("recording", 3, &policy(100_000))
        .unwrap();
    assert_eq!(recovered.deadline_ms, Some(110_000));
    drop(handle);
    drop(catalog);
    fixture.execute_sql("DROP TRIGGER retention_write_failure;");
    let catalog = fixture.open();
    let after = catalog
        .handle()
        .commit_retention("recording", 3, &policy(100_000))
        .unwrap();
    assert_eq!(after.deadline_ms, Some(110_000));
}

#[test]
fn corrupt_deadline_metadata_fails_closed() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let handle = catalog.handle();
    handle.upsert_recording(recording()).unwrap();
    handle
        .commit_retention("recording", 1, &policy(50_000))
        .unwrap();
    drop(handle);
    drop(catalog);
    fixture.execute_sql(
        "UPDATE recording_retention_decisions SET reason_json = '\"invalid-reason\"';",
    );
    let catalog = fixture.open();
    let handle = catalog.handle();
    assert!(handle.retention_decision("recording").is_err());
    assert!(handle.commit_retention("recording", 2, &policy(1)).is_err());
}

#[test]
fn catalog_event_query_preserves_half_open_and_camera_stream_boundaries() {
    for (start, end, camera, stream, expected) in [
        (-100, Some(0), "front", None, None),
        (10_000, Some(11_000), "front", None, None),
        (0, Some(0), "front", None, Some(60_000)),
        (-100, None, "front", Some("main"), Some(60_000)),
        (-100, None, "front", Some("sub"), None),
        (-100, None, "other", None, None),
    ] {
        let fixture = Fixture::new();
        let catalog = fixture.open();
        let handle = catalog.handle();
        handle.upsert_recording(recording()).unwrap();
        let mut candidate = event("event", "motion");
        candidate.start_time_ms = start;
        candidate.end_time_ms = end;
        candidate.camera_id = camera.into();
        candidate.stream = stream.map(str::to_owned);
        handle.insert_event(candidate).unwrap();
        assert_eq!(
            handle
                .commit_retention("recording", 1, &motion_policy())
                .unwrap()
                .deadline_ms,
            expected,
            "{start}, {end:?}, {camera}, {stream:?}"
        );
    }
}

#[test]
fn pending_cleanup_cannot_acquire_a_new_retention_obligation() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let handle = catalog.handle();
    handle.upsert_recording(recording()).unwrap();
    let before = handle
        .commit_retention("recording", 1, &policy(50_000))
        .unwrap();
    drop(handle);
    drop(catalog);
    fixture.execute_sql("UPDATE recording_files SET cleanup_pending = 1 WHERE id = 'recording';");
    let catalog = fixture.open();
    let handle = catalog.handle();
    assert!(
        handle
            .commit_retention("recording", 2, &policy(100_000))
            .is_err()
    );
    assert_eq!(
        handle.retention_decision("recording").unwrap(),
        Some(before)
    );
}
