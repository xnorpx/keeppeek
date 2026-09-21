use keeppeek::storage::catalog::{CatalogRecording, RecordingCatalog, retention::Update};
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-retention-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("media.mp4"), [42; 64]).unwrap();
        Self { root }
    }

    fn open(&self) -> RecordingCatalog {
        RecordingCatalog::open(&self.root.join("catalog.db")).unwrap()
    }

    fn execute(&self, sql: &str) {
        pollster::block_on(async {
            let database =
                turso::Builder::new_local(self.root.join("catalog.db").to_str().unwrap())
                    .build()
                    .await
                    .unwrap();
            database
                .connect()
                .unwrap()
                .execute_batch(sql)
                .await
                .unwrap();
        });
    }

    fn seed(&self, catalog: &RecordingCatalog) {
        catalog
            .handle()
            .upsert_recording(CatalogRecording {
                id: "media".to_owned(),
                stream_id: "camera/sub".to_owned(),
                source_id: Some("camera".to_owned()),
                logical_stream_id: Some("sub".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(2_000),
                path: self.root.join("media.mp4").to_str().unwrap().to_owned(),
                init_offset: 0,
                init_len: 8,
                finalized: true,
            })
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn update(expected_revision: Option<u64>, deadline_ms: Option<i64>) -> Update {
    Update {
        expected_revision,
        policy_revision: 1,
        deadline_ms,
        evidence_through_ms: 2_000,
    }
}

#[test]
fn retention_deadline_survives_restart_and_zero_policy_without_losing_media() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let first = catalog
        .handle()
        .commit_recording_retention("media", update(None, Some(50_000)))
        .unwrap();
    assert_eq!(first.revision, 1);
    catalog.shutdown();
    let catalog = fixture.open();
    assert_eq!(
        catalog.handle().recording_retention("media").unwrap(),
        Some(first)
    );
    let next = catalog
        .handle()
        .commit_recording_retention("media", update(Some(1), None))
        .unwrap();
    assert_eq!(next.deadline_ms, Some(50_000));
    assert_eq!(next.revision, 2);
    assert_eq!(
        std::fs::read(fixture.root.join("media.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn retention_conflict_incomplete_evidence_and_missing_media_leave_state_unchanged() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let first = catalog
        .handle()
        .commit_recording_retention("media", update(None, Some(50_000)))
        .unwrap();
    assert!(
        catalog
            .handle()
            .commit_recording_retention("media", update(None, Some(90_000)))
            .is_err()
    );
    let mut incomplete = update(Some(1), Some(90_000));
    incomplete.evidence_through_ms = 1_999;
    assert!(
        catalog
            .handle()
            .commit_recording_retention("media", incomplete)
            .is_err()
    );
    assert!(
        catalog
            .handle()
            .commit_recording_retention("unknown", update(None, Some(90_000)))
            .is_err()
    );
    assert_eq!(
        catalog.handle().recording_retention("media").unwrap(),
        Some(first)
    );
    let extended = catalog
        .handle()
        .commit_recording_retention("media", update(Some(1), Some(90_000)))
        .unwrap();
    assert_eq!(extended.deadline_ms, Some(90_000));
    assert!(
        catalog
            .handle()
            .commit_recording_retention("media", update(Some(1), Some(100_000)))
            .is_err()
    );
    assert_eq!(
        catalog.handle().recording_retention("media").unwrap(),
        Some(extended)
    );
    catalog.shutdown();
}

#[test]
fn shorter_deadlines_preserve_media_and_stale_policies_fail() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let handle = catalog.handle();
    handle
        .commit_recording_retention("media", update(None, Some(50_000)))
        .unwrap();
    let next = handle
        .commit_recording_retention("media", update(Some(1), Some(10_000)))
        .unwrap();
    assert_eq!(next.deadline_ms, Some(50_000));
    let mut stale = update(Some(2), Some(90_000));
    stale.policy_revision = 0;
    assert!(handle.commit_recording_retention("media", stale).is_err());
    assert_eq!(handle.recording_retention("media").unwrap(), Some(next));
    assert_eq!(
        std::fs::read(fixture.root.join("media.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn incomplete_and_claimed_recordings_reject_retention_updates() {
    for mutation in [
        "UPDATE recording_files SET finalized = 0 WHERE id = 'media'",
        "UPDATE recording_files SET cleanup_pending = 1 WHERE id = 'media'",
        "INSERT INTO recording_maintenance_claims
         (job_id, ordinal, recording_id, token, path, file_identity, file_bytes, active)
         VALUES ('job', 0, 'media', 'token', 'claimed', X'01', 64, 1)",
    ] {
        let fixture = Fixture::new();
        let catalog = fixture.open();
        fixture.seed(&catalog);
        let first = catalog
            .handle()
            .commit_recording_retention("media", update(None, Some(50_000)))
            .unwrap();
        fixture.execute(mutation);
        assert!(
            catalog
                .handle()
                .commit_recording_retention("media", update(Some(1), Some(90_000)))
                .is_err(),
            "{mutation}"
        );
        assert_eq!(
            catalog.handle().recording_retention("media").unwrap(),
            Some(first)
        );
        assert_eq!(
            std::fs::read(fixture.root.join("media.mp4")).unwrap(),
            [42; 64]
        );
        catalog.shutdown();
    }
}

#[test]
fn legacy_catalog_migration_and_path_changes_preserve_retention_identity() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    catalog.shutdown();
    fixture.execute("DROP TABLE recording_retention");
    let catalog = fixture.open();
    let handle = catalog.handle();
    assert_eq!(handle.recording_retention("media").unwrap(), None);
    let first = handle
        .commit_recording_retention("media", update(None, Some(50_000)))
        .unwrap();
    let destination = fixture.root.join("moved.mp4");
    std::fs::rename(fixture.root.join("media.mp4"), &destination).unwrap();
    handle
        .update_recording_path("media", &destination, true)
        .unwrap();
    assert_eq!(
        handle.recording_retention("media").unwrap(),
        Some(first.clone())
    );
    catalog.shutdown();
    let catalog = fixture.open();
    assert_eq!(
        catalog.handle().recording_retention("media").unwrap(),
        Some(first)
    );
    assert_eq!(std::fs::read(destination).unwrap(), [42; 64]);
    catalog.shutdown();
}
