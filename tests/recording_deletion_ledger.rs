use keeppeek::storage::catalog::maintenance::Scope;
use keeppeek::storage::catalog::maintenance::jobs::preflight::Status;
use keeppeek::storage::catalog::maintenance::jobs::{
    Action, Failure, Intent, Job, ObjectState, Reason, State,
};
use keeppeek::storage::catalog::{CatalogRecording, RecordingCatalog};
use keeppeek::storage::long_term::inspection::Archive;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-deletion-ledger-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("recording.mp4"), [42; 64]).unwrap();
        Self { root }
    }

    fn open(&self) -> RecordingCatalog {
        RecordingCatalog::open(&self.root.join("recordings.db")).unwrap()
    }

    fn replace_snapshot(&self, job: &Job, snapshot: &serde_json::Value) {
        pollster::block_on(async {
            let database_path = self.root.join("recordings.db");
            let database = turso::Builder::new_local(database_path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let connection = database.connect().unwrap();
            connection
                .execute(
                    "UPDATE recording_maintenance_intents SET snapshot_json = ?1 WHERE id = ?2",
                    turso::params![serde_json::to_string(snapshot).unwrap(), job.id.as_str()],
                )
                .await
                .unwrap();
        });
    }

    fn confirm(&self, catalog: &RecordingCatalog) -> Job {
        let prepared = self.prepare(catalog);
        catalog
            .handle()
            .recording_deletion_intent(
                "administrator",
                Action::Confirm {
                    id: prepared.id,
                    nonce: prepared.confirmation.unwrap(),
                    expected_revision: prepared.revision,
                },
            )
            .unwrap()
    }

    fn interrupt_started_work(&self, job: &Job) -> PathBuf {
        pollster::block_on(async {
            let database =
                turso::Builder::new_local(self.root.join("recordings.db").to_str().unwrap())
                    .build()
                    .await
                    .unwrap();
            let connection = database.connect().unwrap();
            connection
                .execute(
                    "INSERT INTO recording_maintenance_execution
                 (job_id, recording_id, phase, executor, changed_at_ms)
                 VALUES (?1, 'recording-1', 'working', 'interrupted', 1)",
                    turso::params![job.id.as_str()],
                )
                .await
                .unwrap();
            let mut rows = connection
                .query(
                    "SELECT token FROM recording_maintenance_claims WHERE job_id = ?1",
                    turso::params![job.id.as_str()],
                )
                .await
                .unwrap();
            let token = rows
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap();
            self.root.join(".maintenance").join(token)
        })
    }

    fn prepare(&self, catalog: &RecordingCatalog) -> Job {
        let handle = catalog.handle();
        let media = self.root.join("recording.mp4");
        handle
            .upsert_recording(CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front/sub".to_owned(),
                source_id: Some("front".to_owned()),
                logical_stream_id: Some("sub".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(2_000),
                path: media.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 8,
                finalized: true,
            })
            .unwrap();
        handle
            .update_recording_path("recording-1", &media, true)
            .unwrap();
        let snapshot = handle
            .recording_maintenance_snapshot(Scope::Recording {
                source_id: "front".to_owned(),
                stream_id: "sub".to_owned(),
                recording_id: "recording-1".to_owned(),
            })
            .unwrap();
        handle
            .recording_deletion_intent(
                "administrator",
                Action::Prepare(Intent {
                    scope: snapshot.scope,
                    expected_revision: snapshot.revision,
                    reason: Reason::Operator,
                }),
            )
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn cancellation_after_restart_preserves_unresolved_missing_media_claims() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let job = fixture.confirm(&catalog);
    assert_eq!(
        catalog
            .handle()
            .recording_deletion_claims("administrator", &job.id)
            .unwrap()
            .len(),
        1
    );
    catalog.shutdown();
    fixture.interrupt_started_work(&job);
    let original = fixture.root.join("recording.mp4");
    let relocated = fixture.root.join("relocated.mp4");
    std::fs::rename(&original, &relocated).unwrap();
    let catalog = fixture.open();
    catalog
        .handle()
        .recording_deletion_intent("administrator", Action::Cancel { id: job.id.clone() })
        .unwrap();
    let archive = Archive::open(&fixture.root).unwrap();
    let report = catalog
        .handle()
        .execute_recording_deletion("administrator", &job.id, &archive)
        .unwrap();
    assert_eq!(report.deleted, 0);
    assert_eq!(report.failed, 1);
    assert!(report.cancelled);
    assert!(
        catalog
            .handle()
            .update_recording_path("recording-1", &relocated, true)
            .is_err()
    );
    assert_eq!(std::fs::read(&relocated).unwrap(), [42; 64]);

    std::fs::rename(&relocated, &original).unwrap();
    let resolved = catalog
        .handle()
        .execute_recording_deletion("administrator", &job.id, &archive)
        .unwrap();
    assert_eq!(resolved.failed, 0);
    assert_eq!(
        resolved.objects[0].status,
        keeppeek::storage::catalog::maintenance::jobs::execution::Status::Cancelled
    );
    assert_eq!(std::fs::read(original).unwrap(), [42; 64]);
    catalog.shutdown();
}

#[test]
fn cancelled_work_with_unexpected_staging_contents_remains_unresolved() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let job = fixture.confirm(&catalog);
    catalog
        .handle()
        .recording_deletion_claims("administrator", &job.id)
        .unwrap();
    catalog.shutdown();
    let directory = fixture.interrupt_started_work(&job);
    std::fs::create_dir_all(&directory).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            directory.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(directory.join("unexpected"), [24; 8]).unwrap();
    let catalog = fixture.open();
    catalog
        .handle()
        .recording_deletion_intent("administrator", Action::Cancel { id: job.id.clone() })
        .unwrap();
    let archive = Archive::open(&fixture.root).unwrap();
    let report = catalog
        .handle()
        .execute_recording_deletion("administrator", &job.id, &archive)
        .unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(report.deleted, 0);
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    assert_eq!(
        std::fs::read(directory.join("unexpected")).unwrap(),
        [24; 8]
    );
    catalog.shutdown();
}

#[test]
fn restarted_worker_reports_interrupted_work_as_retryable_failure() {
    use keeppeek::storage::catalog::maintenance::jobs::execution::Status as ExecutionStatus;
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let job = fixture.confirm(&catalog);
    catalog
        .handle()
        .recording_deletion_claims("administrator", &job.id)
        .unwrap();
    catalog.shutdown();
    fixture.interrupt_started_work(&job);
    let catalog = fixture.open();
    let progress = catalog
        .handle()
        .recording_deletion_progress("administrator", &job.id)
        .unwrap();

    assert_eq!(progress.objects[0].status, ExecutionStatus::Failed);
    assert_eq!(progress.failed, 1);
    assert_eq!(progress.deleted, 0);
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    let archive = Archive::open(&fixture.root).unwrap();
    assert_eq!(
        catalog
            .handle()
            .execute_recording_deletion("administrator", &job.id, &archive)
            .unwrap()
            .deleted,
        1
    );
    catalog.shutdown();
}

#[test]
fn reconciliation_remedies_require_the_owner_and_reject_reappeared_files() {
    use keeppeek::storage::catalog::maintenance::reconciliation::{Kind, Remedy};
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.prepare(&catalog);
    let file = fixture.root.join("recording.mp4");
    std::fs::remove_file(&file).unwrap();
    let archive = Archive::open(&fixture.root).unwrap();
    let report = catalog
        .handle()
        .recording_reconciliation("administrator", &archive)
        .unwrap();
    let item = report
        .items
        .iter()
        .find(|item| item.kind == Kind::MissingFile)
        .unwrap();
    assert!(
        catalog
            .handle()
            .apply_recording_reconciliation(
                "other",
                &report,
                &item.id,
                Remedy::RetainTombstone,
                &archive
            )
            .is_err()
    );
    std::fs::write(&file, [24; 64]).unwrap();
    assert!(
        catalog
            .handle()
            .apply_recording_reconciliation(
                "administrator",
                &report,
                &item.id,
                Remedy::RetainTombstone,
                &archive
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&file).unwrap(), [24; 64]);
    std::fs::remove_file(&file).unwrap();
    catalog
        .handle()
        .apply_recording_reconciliation(
            "administrator",
            &report,
            &item.id,
            Remedy::RetainTombstone,
            &archive,
        )
        .unwrap();
    assert_eq!(catalog.handle().stats().unwrap().recording_files, 0);
    catalog.shutdown();
}

#[test]
fn reconciliation_reports_missing_and_unknown_files_without_mutating_them() {
    use keeppeek::storage::catalog::maintenance::reconciliation::Kind;
    let fixture = Fixture::new();
    let catalog = fixture.open();
    fixture.prepare(&catalog);
    std::fs::remove_file(fixture.root.join("recording.mp4")).unwrap();
    let unknown = fixture.root.join("unknown.mp4");
    std::fs::write(&unknown, [24; 64]).unwrap();
    let archive = Archive::open(&fixture.root).unwrap();
    let report = catalog
        .handle()
        .recording_reconciliation("administrator", &archive)
        .unwrap();
    assert!(report.complete);
    assert!(
        report
            .items
            .iter()
            .any(|item| item.kind == Kind::MissingFile
                && item.recording_id.as_deref() == Some("recording-1"))
    );
    assert!(
        report
            .items
            .iter()
            .any(|item| item.kind == Kind::UnknownFile)
    );
    assert_eq!(std::fs::read(unknown).unwrap(), [24; 64]);
    assert!(!format!("{report:?}").contains(fixture.root.to_str().unwrap()));
    assert_eq!(catalog.handle().stats().unwrap().recording_files, 1);
    catalog.shutdown();
}

#[test]
fn completed_deletion_does_not_reserve_the_path_for_a_new_recording() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let first = fixture.confirm(&catalog);
    let archive = Archive::open(&fixture.root).unwrap();
    assert_eq!(
        catalog
            .handle()
            .execute_recording_deletion("administrator", &first.id, &archive)
            .unwrap()
            .deleted,
        1
    );
    std::fs::write(fixture.root.join("recording.mp4"), [24; 64]).unwrap();
    let replacement = fixture.confirm(&catalog);
    let report = catalog
        .handle()
        .execute_recording_deletion("administrator", &replacement.id, &archive)
        .unwrap();
    assert_eq!(report.deleted, 1);
    assert_eq!(
        catalog
            .handle()
            .recording_deletion_progress("administrator", &first.id)
            .unwrap()
            .deleted,
        1
    );
    catalog.shutdown();
}

#[test]
fn confirmed_deletion_preserves_unknown_media_and_reports_the_same_result_after_restart() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let queued = fixture.confirm(&catalog);
    let unknown = fixture.root.join("unknown.mp4");
    std::fs::write(&unknown, [24; 64]).unwrap();
    let archive = Archive::open(&fixture.root).unwrap();
    let report = catalog
        .handle()
        .execute_recording_deletion("administrator", &queued.id, &archive)
        .unwrap();
    assert_eq!(report.deleted, 1);
    assert_eq!(report.failed, 0);
    assert!(!fixture.root.join("recording.mp4").exists());
    assert_eq!(std::fs::read(&unknown).unwrap(), [24; 64]);
    assert!(
        catalog
            .handle()
            .recording_maintenance_snapshot(queued.snapshot.scope.clone())
            .unwrap()
            .recordings
            .is_empty()
    );
    assert_eq!(
        catalog
            .handle()
            .execute_recording_deletion("administrator", &queued.id, &archive)
            .unwrap(),
        report
    );
    catalog.shutdown();
    let catalog = fixture.open();
    assert_eq!(
        catalog
            .handle()
            .execute_recording_deletion("administrator", &queued.id, &archive)
            .unwrap(),
        report
    );
    assert_eq!(std::fs::read(unknown).unwrap(), [24; 64]);
    catalog.shutdown();
}

#[test]
fn deletion_claims_survive_restart_and_fence_recording_mutation_until_cancelled() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let queued = fixture.confirm(&catalog);
    let handle = catalog.handle();
    let claims = handle
        .recording_deletion_claims("administrator", &queued.id)
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].recording_id(), "recording-1");
    assert!(!format!("{claims:?}").contains(fixture.root.to_str().unwrap()));
    assert!(handle.set_recording_protected("recording-1", true).is_err());
    let replacement = fixture.root.join("replacement.mp4");
    std::fs::write(&replacement, [24; 64]).unwrap();
    assert!(
        handle
            .update_recording_path("recording-1", &replacement, true)
            .is_err()
    );
    catalog.shutdown();
    let catalog = fixture.open();
    let handle = catalog.handle();
    assert_eq!(
        handle
            .recording_deletion_claims("administrator", &queued.id)
            .unwrap(),
        claims
    );
    handle
        .recording_deletion_intent("administrator", Action::Cancel { id: queued.id })
        .unwrap();
    handle.set_recording_protected("recording-1", true).unwrap();
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn deletion_preflight_rejects_wrong_owners_and_unconfirmed_or_cancelled_jobs() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let archive = Archive::open(&fixture.root).unwrap();
    let prepared = fixture.prepare(&catalog);
    let handle = catalog.handle();
    let error = handle
        .recording_deletion_preflight("administrator", &prepared.id, &archive)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<Failure>(),
        Some(&Failure::InvalidState)
    );
    let queued = fixture.confirm(&catalog);
    let error = handle
        .recording_deletion_preflight("other-actor", &queued.id, &archive)
        .unwrap_err();
    assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::NotFound));
    for invalid in ["".to_owned(), "z".repeat(32), "0".repeat(33)] {
        let error = handle
            .recording_deletion_preflight("administrator", invalid, &archive)
            .unwrap_err();
        assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::Invalid));
    }
    handle
        .recording_deletion_intent(
            "administrator",
            Action::Cancel {
                id: queued.id.clone(),
            },
        )
        .unwrap();
    let error = handle
        .recording_deletion_preflight("administrator", &queued.id, &archive)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<Failure>(),
        Some(&Failure::InvalidState)
    );
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn deletion_preflight_reports_new_protection_and_both_catalog_revisions() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let archive = Archive::open(&fixture.root).unwrap();
    let queued = fixture.confirm(&catalog);
    let handle = catalog.handle();
    handle.set_recording_protected("recording-1", true).unwrap();
    let current = handle
        .recording_maintenance_snapshot(queued.snapshot.scope.clone())
        .unwrap();
    let report = handle
        .recording_deletion_preflight("administrator", &queued.id, &archive)
        .unwrap();
    assert_eq!(report.objects[0].status, Status::ProtectedRecording);
    assert_eq!(report.planned_revision, queued.revision);
    assert_eq!(report.catalog_revision, current.revision);
    assert!(report.catalog_revision > report.planned_revision);
    assert_eq!(
        handle
            .recording_deletion_intent(
                "administrator",
                Action::Read {
                    id: queued.id.clone()
                }
            )
            .unwrap(),
        queued
    );
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
    let catalog = fixture.open();
    assert_eq!(
        catalog
            .handle()
            .recording_deletion_preflight("administrator", &queued.id, &archive)
            .unwrap(),
        report
    );
    catalog.shutdown();
}

#[test]
fn deletion_preflight_reports_size_drift_and_rejects_paths_outside_the_archive() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let catalog = fixture.open();
    let archive = Archive::open(&fixture.root).unwrap();
    let queued = fixture.confirm(&catalog);
    let handle = catalog.handle();
    std::fs::write(fixture.root.join("recording.mp4"), [24; 65]).unwrap();
    let report = handle
        .recording_deletion_preflight("administrator", &queued.id, &archive)
        .unwrap();
    assert_eq!(report.objects[0].status, Status::SizeMismatch);
    handle
        .update_recording_path("recording-1", &outside.root.join("recording.mp4"), true)
        .unwrap();
    let report = handle
        .recording_deletion_preflight("administrator", &queued.id, &archive)
        .unwrap();
    assert_eq!(report.objects[0].status, Status::PathRejected);
    let rendered = format!("{report:?}");
    assert!(!rendered.contains(fixture.root.to_str().unwrap()));
    assert!(!rendered.contains(outside.root.to_str().unwrap()));
    assert!(!rendered.contains("recording.mp4"));
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [24; 65]
    );
    assert_eq!(
        std::fs::read(outside.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn deletion_preflight_does_not_accept_a_same_size_replacement_as_the_catalog_file() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let queued = fixture.confirm(&catalog);
    let archive = Archive::open(&fixture.root).unwrap();
    let original = fixture.root.join("original.mp4");
    let recording = fixture.root.join("recording.mp4");
    std::fs::rename(&recording, &original).unwrap();
    std::fs::write(&recording, [24; 64]).unwrap();
    let handle = catalog.handle();
    let report = handle
        .recording_deletion_preflight("administrator", &queued.id, &archive)
        .unwrap();
    assert_eq!(report.objects[0].status, Status::IdentityChanged);
    assert_eq!(std::fs::read(original).unwrap(), [42; 64]);
    assert_eq!(std::fs::read(recording).unwrap(), [24; 64]);
    assert_eq!(
        handle
            .recording_deletion_intent(
                "administrator",
                Action::Read {
                    id: queued.id.clone()
                }
            )
            .unwrap(),
        queued
    );
    catalog.shutdown();
}

#[test]
fn deletion_preflight_preserves_planned_identity_after_catalog_refresh_and_restart() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let queued = fixture.confirm(&catalog);
    let archive = Archive::open(&fixture.root).unwrap();
    let original = fixture.root.join("original.mp4");
    let recording = fixture.root.join("recording.mp4");
    std::fs::rename(&recording, &original).unwrap();
    std::fs::write(&recording, [24; 64]).unwrap();
    let handle = catalog.handle();
    handle
        .update_recording_path("recording-1", &recording, true)
        .unwrap();
    let report = handle
        .recording_deletion_preflight("administrator", &queued.id, &archive)
        .unwrap();
    assert_eq!(report.objects[0].status, Status::IdentityChanged);
    assert!(report.catalog_revision > report.planned_revision);
    catalog.shutdown();
    let catalog = fixture.open();
    assert_eq!(
        catalog
            .handle()
            .recording_deletion_preflight("administrator", &queued.id, &archive)
            .unwrap(),
        report
    );
    assert_eq!(std::fs::read(original).unwrap(), [42; 64]);
    assert_eq!(std::fs::read(recording).unwrap(), [24; 64]);
    catalog.shutdown();
}

#[test]
fn malformed_persisted_identity_is_redacted_for_intent_reads_and_confirmation_retries() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let prepared = fixture.prepare(&catalog);
    let confirm = Action::Confirm {
        id: prepared.id,
        nonce: prepared.confirmation.unwrap(),
        expected_revision: prepared.revision,
    };
    let queued = catalog
        .handle()
        .recording_deletion_intent("administrator", confirm.clone())
        .unwrap();
    catalog.shutdown();
    let original = serde_json::to_value(&queued.snapshot).unwrap();
    for invalid in ["123456:987654", "/private/recordings/camera.mp4"] {
        let mut corrupted = original.clone();
        corrupted["recordings"][0]["file_identity"] = serde_json::json!(invalid);
        fixture.replace_snapshot(&queued, &corrupted);
        let catalog = fixture.open();
        for action in [
            Action::Read {
                id: queued.id.clone(),
            },
            confirm.clone(),
        ] {
            let error = catalog
                .handle()
                .recording_deletion_intent("administrator", action)
                .unwrap_err();
            assert!(!format!("{error}").contains(invalid));
            assert!(!format!("{error:?}").contains(invalid));
            assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::Invalid));
        }
        catalog.shutdown();
    }
    fixture.replace_snapshot(&queued, &original);
    let catalog = fixture.open();
    assert_eq!(
        catalog
            .handle()
            .recording_deletion_intent("administrator", confirm)
            .unwrap(),
        queued
    );
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn deletion_preflight_reports_missing_media_without_mutating_the_job_or_catalog() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let prepared = fixture.prepare(&catalog);
    let handle = catalog.handle();
    let queued = handle
        .recording_deletion_intent(
            "administrator",
            Action::Confirm {
                id: prepared.id,
                nonce: prepared.confirmation.unwrap(),
                expected_revision: prepared.revision,
            },
        )
        .unwrap();
    let archive = Archive::open(&fixture.root).unwrap();
    let report = handle
        .recording_deletion_preflight("administrator", &queued.id, &archive)
        .unwrap();
    assert_eq!(report.objects.len(), 1);
    assert_eq!(report.objects[0].recording_id, "recording-1");
    assert_eq!(report.objects[0].status, Status::Present);
    assert_eq!(report.catalog_revision, queued.revision);
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    std::fs::remove_file(fixture.root.join("recording.mp4")).unwrap();
    let report = handle
        .recording_deletion_preflight("administrator", &queued.id, &archive)
        .unwrap();
    assert_eq!(report.objects[0].status, Status::MissingFile);
    assert_eq!(
        handle
            .recording_maintenance_snapshot(queued.snapshot.scope.clone())
            .unwrap(),
        queued.snapshot
    );
    assert_eq!(
        handle
            .recording_deletion_intent(
                "administrator",
                Action::Read {
                    id: queued.id.clone()
                }
            )
            .unwrap(),
        queued
    );
    catalog.shutdown();
}

#[test]
fn concurrent_confirmations_return_the_same_job_and_ledger() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let prepared = fixture.prepare(&catalog);
    let confirm = Action::Confirm {
        id: prepared.id.clone(),
        nonce: prepared.confirmation.unwrap(),
        expected_revision: prepared.revision,
    };
    let barrier = std::sync::Barrier::new(4);
    let jobs = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let handle = catalog.handle();
                let action = confirm.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    handle
                        .recording_deletion_intent("administrator", action)
                        .unwrap()
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(jobs.len(), 4);
    assert_eq!(jobs[0].objects.len(), 1);
    assert_eq!(jobs[0].objects[0].recording_id, "recording-1");
    assert!(jobs.iter().all(|job| *job == jobs[0]));
    assert_eq!(jobs[0].state, State::Queued);
    let denied = catalog
        .handle()
        .recording_deletion_intent("other-actor", Action::Read { id: prepared.id });
    assert!(denied.is_err());
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn cancellation_before_confirmation_never_creates_work() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let prepared = fixture.prepare(&catalog);
    let cancel = Action::Cancel { id: prepared.id };
    let cancelled = catalog
        .handle()
        .recording_deletion_intent("administrator", cancel.clone())
        .unwrap();
    assert_eq!(cancelled.state, State::Cancelled);
    assert_eq!(cancelled.confirmed_at_ms, None);
    assert!(cancelled.objects.is_empty());
    catalog.shutdown();
    let catalog = fixture.open();
    assert_eq!(
        catalog
            .handle()
            .recording_deletion_intent("administrator", cancel)
            .unwrap(),
        cancelled
    );
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn cancelled_objects_survive_restart_and_cannot_be_confirmed_again() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let prepared = fixture.prepare(&catalog);
    let confirm = Action::Confirm {
        id: prepared.id,
        nonce: prepared.confirmation.unwrap(),
        expected_revision: prepared.revision,
    };
    let queued = catalog
        .handle()
        .recording_deletion_intent("administrator", confirm.clone())
        .unwrap();
    let cancel = Action::Cancel { id: queued.id };
    let cancelled = catalog
        .handle()
        .recording_deletion_intent("administrator", cancel.clone())
        .unwrap();
    assert_eq!(cancelled.state, State::Cancelled);
    assert_eq!(cancelled.objects.len(), 1);
    assert_eq!(cancelled.objects[0].recording_id, "recording-1");
    assert_eq!(cancelled.objects[0].state, ObjectState::Cancelled);
    catalog.shutdown();
    let catalog = fixture.open();
    assert_eq!(
        catalog
            .handle()
            .recording_deletion_intent("administrator", cancel)
            .unwrap(),
        cancelled
    );
    assert!(
        catalog
            .handle()
            .recording_deletion_intent("administrator", confirm)
            .is_err()
    );
    assert_eq!(
        catalog
            .handle()
            .recording_maintenance_snapshot(cancelled.snapshot.scope.clone())
            .unwrap(),
        cancelled.snapshot
    );
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    catalog.shutdown();
}

#[test]
fn confirmed_objects_survive_retry_and_restart_without_claiming_media() {
    let fixture = Fixture::new();
    let catalog = fixture.open();
    let prepared = fixture.prepare(&catalog);
    assert!(prepared.objects.is_empty());
    let confirm = Action::Confirm {
        id: prepared.id,
        nonce: prepared.confirmation.unwrap(),
        expected_revision: prepared.revision,
    };
    let queued = catalog
        .handle()
        .recording_deletion_intent("administrator", confirm.clone())
        .unwrap();
    assert_eq!(queued.state, State::Queued);
    assert_eq!(queued.objects.len(), 1);
    assert_eq!(queued.objects[0].recording_id, "recording-1");
    assert_eq!(queued.objects[0].state, ObjectState::Queued);
    assert_eq!(
        catalog
            .handle()
            .recording_deletion_intent("administrator", confirm.clone())
            .unwrap(),
        queued
    );
    assert_eq!(
        catalog
            .handle()
            .recording_maintenance_snapshot(queued.snapshot.scope.clone())
            .unwrap(),
        queued.snapshot
    );
    catalog.shutdown();
    let catalog = fixture.open();
    assert_eq!(
        catalog
            .handle()
            .recording_deletion_intent("administrator", confirm)
            .unwrap(),
        queued
    );
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    assert_eq!(
        catalog
            .handle()
            .recording_maintenance_snapshot(queued.snapshot.scope.clone())
            .unwrap(),
        queued.snapshot
    );
    catalog.shutdown();
}
