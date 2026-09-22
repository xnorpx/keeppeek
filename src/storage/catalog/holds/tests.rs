use crate::storage::catalog::{CatalogRecording, RecordingCatalog, holds::Update};
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("keeppeek-holds-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("media.mp4"), [42; 64]).unwrap();
        Self { root }
    }

    fn open(&self) -> RecordingCatalog {
        RecordingCatalog::open(&self.root.join("catalog.db")).unwrap()
    }

    fn execute(&self, sql: &str) {
        pollster::block_on(self.connection().execute_batch(sql)).unwrap();
    }

    fn connection(&self) -> turso::Connection {
        pollster::block_on(async {
            let database =
                turso::Builder::new_local(self.root.join("catalog.db").to_str().unwrap())
                    .build()
                    .await
                    .unwrap();
            database.connect().unwrap()
        })
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

fn update(revision: Option<u64>, active: bool) -> Update {
    Update {
        expected_revision: revision,
        active,
        actor: "administrator".to_owned(),
        reason: "Keep this evidence".to_owned(),
    }
}

#[test]
fn inspection_distinguishes_marker_protection_and_missing_media() {
    use crate::storage::long_term::inspection::Archive;
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let handle = catalog.handle();
    handle
        .update_recording_path("media", &fixture.root.join("media.mp4"), true)
        .unwrap();
    let archive = Archive::open(&fixture.root).unwrap();
    let initial = handle
        .inspect_recording_hold("media", "recording", Some(&archive))
        .unwrap();
    assert!(initial.hold.is_none());
    assert!(initial.media_available);
    assert!(!initial.protected);
    assert_eq!(initial.bytes, 64);
    handle
        .update_recording_hold("media", "event", update(None, true))
        .unwrap();
    let shared = handle
        .inspect_recording_hold("media", "recording", Some(&archive))
        .unwrap();
    assert!(shared.protected);
    assert!(shared.independently_protected);
    assert!(shared.hold.is_none());
    std::fs::remove_file(fixture.root.join("media.mp4")).unwrap();
    let missing = handle
        .inspect_recording_hold("media", "event", Some(&archive))
        .unwrap();
    assert!(missing.hold.unwrap().active);
    assert!(missing.protected);
    assert!(!missing.media_available);
    assert!(
        handle
            .inspect_recording_hold("absent", "recording", Some(&archive))
            .is_err()
    );
}

#[test]
fn inspection_preserves_attribution_without_an_archive_and_detects_file_drift() {
    use crate::storage::long_term::inspection::Archive;
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let handle = catalog.handle();
    let path = fixture.root.join("media.mp4");
    handle.update_recording_path("media", &path, true).unwrap();
    let saved = handle
        .update_recording_hold("media", "saved", update(None, true))
        .unwrap();
    let archive = Archive::open(&fixture.root).unwrap();
    let state = handle
        .inspect_recording_hold("media", "saved", None)
        .unwrap();
    assert_eq!(state.hold, Some(saved.clone()));
    assert!(!state.media_available);
    assert!(!state.independently_protected);
    std::fs::write(&path, [42; 65]).unwrap();
    assert!(
        !handle
            .inspect_recording_hold("media", "saved", Some(&archive))
            .unwrap()
            .media_available
    );
    std::fs::rename(&path, fixture.root.join("original.mp4")).unwrap();
    std::fs::write(&path, [42; 64]).unwrap();
    let replaced = handle
        .inspect_recording_hold("media", "saved", Some(&archive))
        .unwrap();
    assert!(!replaced.media_available);
    assert_eq!(replaced.hold, Some(saved));
    handle
        .update_recording_hold("media", "saved", update(Some(1), false))
        .unwrap();
    handle.set_recording_protected("media", true).unwrap();
    let legacy = handle
        .inspect_recording_hold("media", "saved", None)
        .unwrap();
    assert!(legacy.independently_protected);
    handle
        .update_recording_hold("media", "saved", update(Some(2), true))
        .unwrap();
    assert!(
        handle
            .inspect_recording_hold("media", "saved", None)
            .unwrap()
            .independently_protected
    );
    fixture.execute("UPDATE recording_files SET path = printf('%5000s', 'x'), file_identity = printf('%5000s', 'x') WHERE id = 'media'");
    let malformed = handle
        .inspect_recording_hold("media", "saved", Some(&archive))
        .unwrap();
    assert!(malformed.hold.unwrap().active);
    assert!(!malformed.media_available);
}

#[test]
fn independent_holds_survive_restart_and_release_only_their_own_protection() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let handle = catalog.handle();
    let first = handle
        .update_recording_hold("media", "recording", update(None, true))
        .unwrap();
    handle
        .update_recording_hold("media", "event", update(None, true))
        .unwrap();
    assert_eq!(first.revision, 1);
    assert!(handle.claim_cleanup_candidate().unwrap().is_none());
    assert!(handle.set_recording_protected("media", false).is_err());
    assert!(handle.delete_recording("media").is_err());
    catalog.shutdown();
    let catalog = fixture.open();
    let handle = catalog.handle();
    assert_eq!(
        handle.recording_hold("media", "recording").unwrap(),
        Some(first)
    );
    handle
        .update_recording_hold("media", "recording", update(Some(1), false))
        .unwrap();
    assert!(handle.claim_cleanup_candidate().unwrap().is_none());
    handle
        .update_recording_hold("media", "event", update(Some(1), false))
        .unwrap();
    assert_eq!(
        handle
            .claim_cleanup_candidate()
            .unwrap()
            .unwrap()
            .recording_id,
        "media"
    );
    assert_eq!(
        std::fs::read(fixture.root.join("media.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn legacy_protection_is_preserved_and_inactive_release_cannot_overwrite_new_protection() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let handle = catalog.handle();
    handle.set_recording_protected("media", true).unwrap();
    handle
        .update_recording_hold("media", "saved", update(None, true))
        .unwrap();
    assert!(handle.set_recording_protected("media", true).is_err());
    handle
        .update_recording_hold("media", "saved", update(Some(1), false))
        .unwrap();
    assert_eq!(handle.stats().unwrap().protected_files, 1);
    handle.set_recording_protected("media", false).unwrap();
    handle
        .update_recording_hold("media", "saved", update(Some(2), true))
        .unwrap();
    handle
        .update_recording_hold("media", "saved", update(Some(3), false))
        .unwrap();
    assert_eq!(handle.stats().unwrap().protected_files, 0);
    handle.set_recording_protected("media", true).unwrap();
    assert!(
        handle
            .update_recording_hold("media", "saved", update(Some(4), false))
            .is_err()
    );
    assert_eq!(handle.stats().unwrap().protected_files, 1);
    assert_eq!(
        handle
            .recording_hold("media", "saved")
            .unwrap()
            .unwrap()
            .revision,
        4
    );
    catalog.shutdown();
}

#[test]
fn cleanup_claim_wins_before_a_hold_and_retry_after_cancellation_is_safe() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let handle = catalog.handle();
    let claim = handle.claim_cleanup_candidate().unwrap().unwrap();
    assert!(
        handle
            .update_recording_hold("media", "saved", update(None, true))
            .is_err()
    );
    assert!(handle.recording_hold("media", "saved").unwrap().is_none());
    assert_eq!(
        handle
            .pending_cleanup_candidate()
            .unwrap()
            .unwrap()
            .recording_id,
        claim.recording_id
    );
    handle.cancel_cleanup("media").unwrap();
    handle
        .update_recording_hold("media", "saved", update(None, true))
        .unwrap();
    assert!(handle.claim_cleanup_candidate().unwrap().is_none());
    assert_eq!(
        std::fs::read(fixture.root.join("media.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn invalid_and_stale_requests_leave_the_hold_and_media_unchanged() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let handle = catalog.handle();
    let saved = handle
        .update_recording_hold("media", "saved", update(None, true))
        .unwrap();
    for revision in [None, Some(0), Some(2), Some(u64::MAX)] {
        assert!(
            handle
                .update_recording_hold("media", "saved", update(revision, false))
                .is_err()
        );
    }
    for id in [
        String::new(),
        " ".to_owned(),
        "x".repeat(129),
        "bad\0id".to_owned(),
    ] {
        assert!(handle.recording_hold("media", &id).is_err());
        assert!(
            handle
                .update_recording_hold(&id, "saved", update(None, true))
                .is_err()
        );
    }
    let mut invalid = update(Some(1), false);
    invalid.actor = "é".repeat(65);
    assert!(
        handle
            .update_recording_hold("media", "saved", invalid)
            .is_err()
    );
    let mut invalid = update(Some(1), false);
    invalid.reason = "é".repeat(129);
    assert!(
        handle
            .update_recording_hold("media", "saved", invalid)
            .is_err()
    );
    assert!(
        handle
            .update_recording_hold("missing", "saved", update(None, true))
            .is_err()
    );
    assert!(
        handle
            .update_recording_hold("media", "missing", update(None, false))
            .is_err()
    );
    assert_eq!(
        handle.recording_hold("media", "saved").unwrap(),
        Some(saved)
    );
    assert_eq!(handle.stats().unwrap().protected_files, 1);
    catalog.shutdown();
}

#[test]
fn active_unknown_end_and_maintenance_claimed_recordings_reject_holds() {
    for mutation in [
        "UPDATE recording_files SET finalized = 0",
        "UPDATE recording_files SET ended_at_ms = NULL",
        "INSERT INTO recording_maintenance_claims
         (job_id, ordinal, recording_id, token, path, file_identity, file_bytes, active)
         VALUES ('job', 0, 'media', 'token', 'claimed', X'01', 64, 1)",
    ] {
        let fixture = Fixture::new();
        let catalog = fixture.open();
        fixture.seed(&catalog);
        fixture.execute(mutation);
        let handle = catalog.handle();
        assert!(
            handle
                .update_recording_hold("media", "saved", update(None, true))
                .is_err()
        );
        assert!(handle.recording_hold("media", "saved").unwrap().is_none());
        assert_eq!(handle.stats().unwrap().protected_files, 0);
        catalog.shutdown();
    }
}

#[test]
fn released_identities_count_toward_capacity_but_existing_holds_remain_releasable() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let values = (1..=256)
        .map(|id| format!("('media', '{id}', 1, 0, 'administrator', 'Released')"))
        .collect::<Vec<_>>()
        .join(",");
    fixture.execute(&format!(
        "INSERT INTO recording_holds (recording_id, hold_id, revision, active, actor, reason) VALUES {values}"
    ));
    let handle = catalog.handle();
    assert!(
        handle
            .update_recording_hold("media", "new", update(None, true))
            .is_err()
    );
    assert_eq!(handle.stats().unwrap().protected_files, 0);
    handle
        .update_recording_hold("media", "256", update(Some(1), true))
        .unwrap();
    assert!(handle.claim_cleanup_candidate().unwrap().is_none());
    handle
        .update_recording_hold("media", "256", update(Some(2), false))
        .unwrap();
    assert_eq!(handle.stats().unwrap().protected_files, 0);
    fixture
        .execute("UPDATE recording_holds SET revision = 9223372036854775807 WHERE hold_id = '256'");
    assert!(
        handle
            .update_recording_hold("media", "256", update(Some(i64::MAX as u64), true))
            .is_err()
    );
    assert_eq!(
        handle
            .recording_hold("media", "256")
            .unwrap()
            .unwrap()
            .revision,
        i64::MAX as u64
    );
    assert_eq!(handle.stats().unwrap().protected_files, 0);
    catalog.shutdown();
}

#[test]
fn failed_materialization_rolls_back_acquisition_and_release() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    fixture.execute(
        "CREATE TRIGGER fail_protection BEFORE UPDATE OF protected ON recording_files
        BEGIN SELECT RAISE(ABORT, 'injected protection failure'); END",
    );
    let handle = catalog.handle();
    assert!(
        handle
            .update_recording_hold("media", "saved", update(None, true))
            .is_err()
    );
    assert!(handle.recording_hold("media", "saved").unwrap().is_none());
    assert_eq!(handle.stats().unwrap().protected_files, 0);
    fixture.execute("DROP TRIGGER fail_protection");
    let saved = handle
        .update_recording_hold("media", "saved", update(None, true))
        .unwrap();
    fixture.execute(
        "CREATE TRIGGER fail_protection BEFORE UPDATE OF protected ON recording_files
        BEGIN SELECT RAISE(ABORT, 'injected protection failure'); END",
    );
    assert!(
        handle
            .update_recording_hold("media", "saved", update(Some(1), false))
            .is_err()
    );
    assert_eq!(
        handle.recording_hold("media", "saved").unwrap(),
        Some(saved)
    );
    assert_eq!(handle.stats().unwrap().protected_files, 1);
    catalog.shutdown();
}

#[test]
fn legacy_catalog_upgrade_and_media_relocation_preserve_indefinite_protection() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    catalog
        .handle()
        .set_recording_protected("media", true)
        .unwrap();
    catalog.shutdown();
    fixture.execute(
        "DROP TRIGGER recording_hold_fence_delete;
        DROP TRIGGER recording_hold_fence_unprotect;
        DROP TABLE recording_holds; DROP TABLE recording_hold_baselines",
    );
    let catalog = fixture.open();
    let handle = catalog.handle();
    assert!(handle.recording_hold("media", "saved").unwrap().is_none());
    let saved = handle
        .update_recording_hold("media", "saved", update(None, true))
        .unwrap();
    let destination = fixture.root.join("moved.mp4");
    std::fs::rename(fixture.root.join("media.mp4"), &destination).unwrap();
    handle
        .update_recording_path("media", &destination, true)
        .unwrap();
    catalog.shutdown();
    let catalog = fixture.open();
    let handle = catalog.handle();
    assert_eq!(
        handle.recording_hold("media", "saved").unwrap(),
        Some(saved)
    );
    handle
        .update_recording_hold("media", "saved", update(Some(1), false))
        .unwrap();
    assert!(handle.claim_cleanup_candidate().unwrap().is_none());
    assert_eq!(std::fs::read(destination).unwrap(), [42; 64]);
    catalog.shutdown();
}

#[test]
fn expired_actor_request_cannot_install_protection() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.seed(&catalog);
    let request = super::Request::Update {
        recording_id: "media".to_owned(),
        hold_id: "saved".to_owned(),
        update: update(None, true),
    };
    assert!(
        pollster::block_on(super::execute(
            &fixture.connection(),
            request,
            std::time::Instant::now()
        ))
        .is_err()
    );
    assert!(
        catalog
            .handle()
            .recording_hold("media", "saved")
            .unwrap()
            .is_none()
    );
    assert_eq!(catalog.handle().stats().unwrap().protected_files, 0);
    catalog.shutdown();
}
