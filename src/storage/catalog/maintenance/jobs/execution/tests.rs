use super::{Action, Report, Status, execute};
use crate::storage::catalog::maintenance::jobs::{
    self, Epoch, Intent, Job, Reason, Request, claims::Claim,
};
use crate::storage::catalog::maintenance::{Scope, snapshot};
use crate::storage::catalog::{BUSY_TIMEOUT, initialize_schema, recording_file_identity};
use crate::storage::long_term::inspection::Archive;
use std::{path::PathBuf, time::Instant};

struct Fixture {
    _database: turso::Database,
    connection: turso::Connection,
    epoch: Epoch,
    root: PathBuf,
    job: Job,
    claim: Claim,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_count(1).await
    }

    async fn with_count(count: u32) -> Self {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-execution-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        Self::in_directory(count, root).await
    }

    async fn in_directory(count: u32, root: PathBuf) -> Self {
        assert!((1..=2).contains(&count));
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let connection = database.connect().unwrap();
        initialize_schema(&connection).await.unwrap();
        let media = root.join("recording.mp4");
        std::fs::write(&media, [42; 64]).unwrap();
        let identity = recording_file_identity(&media, &std::fs::metadata(&media).unwrap());
        connection.execute(
            "INSERT INTO recording_files (id, stream_id, source_id, logical_stream_id,
             started_at_ms, ended_at_ms, path, init_offset, init_len, finalized, file_bytes, file_identity)
             VALUES ('recording', 'front/sub', 'front', 'sub', 1000, 2000, ?1, 0, 8, 1, 64, ?2)",
            turso::params![media.to_str().unwrap(), identity],
        ).await.unwrap();
        if count == 2 {
            let second = root.join("second.mp4");
            std::fs::write(&second, [24; 64]).unwrap();
            let identity = recording_file_identity(&second, &std::fs::metadata(&second).unwrap());
            connection.execute(
                "INSERT INTO recording_files (id, stream_id, source_id, logical_stream_id, started_at_ms,
                 ended_at_ms, path, init_offset, init_len, finalized, file_bytes, file_identity)
                 VALUES ('second', 'front/sub', 'front', 'sub', 2000, 3000, ?1, 0, 8, 1, 64, ?2)",
                turso::params![second.to_str().unwrap(), identity],
            ).await.unwrap();
        }
        let epoch = Epoch::new();
        let job = confirm(&connection, &epoch).await;
        let claim = jobs::claims::reserve(
            &connection,
            "administrator",
            &job.id,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap()
        .remove(0);
        Self {
            _database: database,
            connection,
            epoch,
            root,
            job,
            claim,
        }
    }

    async fn apply(&self, action: Action) -> anyhow::Result<Report> {
        execute(
            &self.connection,
            &self.epoch,
            "administrator",
            &self.job.id,
            action,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
    }

    async fn begin(&self) -> anyhow::Result<Report> {
        let lease = std::sync::Arc::new(());
        self.apply(Action::Begin(
            self.claim.clone(),
            std::sync::Arc::downgrade(&lease),
        ))
        .await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

async fn confirm(connection: &turso::Connection, epoch: &Epoch) -> Job {
    let snapshot = snapshot(
        connection,
        Scope::TimeRange {
            source_id: "front".to_owned(),
            stream_id: "sub".to_owned(),
            start_ms: 1_000,
            end_ms: 3_000,
        },
        Instant::now() + BUSY_TIMEOUT,
    )
    .await
    .unwrap();
    let job = jobs::execute(
        connection,
        epoch,
        Request {
            actor: "administrator".to_owned(),
            action: jobs::Action::Prepare(Intent {
                scope: snapshot.scope.clone(),
                expected_revision: snapshot.revision,
                reason: Reason::Operator,
            }),
            snapshot: Some(snapshot),
            deadline: Instant::now() + BUSY_TIMEOUT,
        },
    )
    .await
    .unwrap();
    jobs::execute(
        connection,
        epoch,
        Request {
            actor: "administrator".to_owned(),
            action: jobs::Action::Confirm {
                id: job.id,
                nonce: job.confirmation.unwrap(),
                expected_revision: job.revision,
            },
            snapshot: None,
            deadline: Instant::now() + BUSY_TIMEOUT,
        },
    )
    .await
    .unwrap()
}

#[test]
fn checkout_archive_stages_and_removes_the_selected_recording() {
    pollster::block_on(async {
        let parent = std::env::current_dir().unwrap().join("target");
        std::fs::create_dir_all(&parent).unwrap();
        let root = parent.join(format!(
            "keeppeek-execution-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(windows)]
        assert!(
            std::process::Command::new("powershell.exe")
                .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/.github/scripts/protect-test-directory.ps1"
                ))
                .arg("-Directory")
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        let fixture = Fixture::in_directory(1, root).await;
        let archive = Archive::open(&fixture.root).unwrap();
        archive.validate_removal().unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        let checkpoint = staged.directory_identity();
        staged.remove().unwrap();
        assert!(!fixture.claim.path.exists());
        assert!(
            archive
                .stage_claim(&fixture.claim, Some(checkpoint))
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn cancellation_releases_unstarted_objects_but_keeps_started_reservations() {
    pollster::block_on(async {
        let fixture = Fixture::with_count(2).await;
        fixture.begin().await.unwrap();
        jobs::execute(
            &fixture.connection,
            &fixture.epoch,
            Request {
                actor: "administrator".to_owned(),
                action: jobs::Action::Cancel {
                    id: fixture.job.id.clone(),
                },
                snapshot: None,
                deadline: Instant::now() + BUSY_TIMEOUT,
            },
        )
        .await
        .unwrap();
        let report = fixture.apply(Action::Read).await.unwrap();
        assert_eq!(report.objects[0].status, Status::Working);
        assert_eq!(report.objects[1].status, Status::Cancelled);
        let mut rows = fixture.connection.query(
            "SELECT recording_id FROM recording_maintenance_claims WHERE active = 1 ORDER BY recording_id", (),
        ).await.unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "recording"
        );
        assert!(rows.next().await.unwrap().is_none());
        drop(rows);
        let candidate = crate::storage::catalog::claim_cleanup_candidate(&fixture.connection)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(candidate.recording_id, "second");
        assert_eq!(
            std::fs::read(fixture.root.join("second.mp4")).unwrap(),
            [24; 64]
        );
    });
}

#[test]
fn claimed_recordings_are_excluded_from_playback_and_export_resolution() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        fixture
            .connection
            .execute_batch(
                "INSERT INTO recording_fragments
             (recording_id, sequence, start_ms, duration_ms, byte_offset, byte_len, random_access)
             VALUES ('recording', 0, 1000, 1000, 8, 56, 1);",
            )
            .await
            .unwrap();
        let fragments = crate::storage::catalog::media_fragments_in_range(
            &fixture.connection,
            "front/sub",
            1_000,
            2_000,
        )
        .await
        .unwrap();
        assert!(fragments.is_empty());
        fixture.connection.execute_batch(
            "INSERT INTO recording_keyframes (recording_id, fragment_sequence, byte_offset, byte_len)
             VALUES ('recording', 0, 8, 56);"
        ).await.unwrap();
        assert!(
            crate::storage::catalog::resolve_media_object(
                &fixture.connection,
                "front",
                "sub",
                None,
                "recording",
                0,
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            crate::storage::catalog::availability_ranges_in_range(
                &fixture.connection,
                "front/sub",
                1_000,
                2_000,
                1_000,
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert!(
            crate::storage::catalog::claim_cleanup_candidate(&fixture.connection)
                .await
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn existing_claims_recheck_bookmark_evidence_before_unstaged_work() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        fixture
            .connection
            .execute_batch("UPDATE event_bookmark_state SET revision = revision + 1")
            .await
            .unwrap();
        let failure = fixture.begin().await.unwrap_err();
        assert_eq!(
            failure.downcast_ref::<jobs::Failure>(),
            Some(&jobs::Failure::Conflict)
        );
        assert_eq!(
            std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
            [42; 64]
        );
    });
}

#[test]
fn history_pruning_keeps_cancelled_jobs_with_unresolved_execution() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        fixture.begin().await.unwrap();
        jobs::execute(
            &fixture.connection,
            &fixture.epoch,
            Request {
                actor: "administrator".to_owned(),
                action: jobs::Action::Cancel {
                    id: fixture.job.id.clone(),
                },
                snapshot: None,
                deadline: Instant::now() + BUSY_TIMEOUT,
            },
        )
        .await
        .unwrap();
        let now = jobs::Moment {
            epoch: fixture.epoch.id.clone(),
            elapsed_ms: 1,
            utc_ms: crate::storage::catalog::current_unix_time_ms() + jobs::CANCEL_RETENTION_MS + 1,
        };
        jobs::prune(&fixture.connection, &now, Instant::now() + BUSY_TIMEOUT)
            .await
            .unwrap();
        let report = fixture.apply(Action::Read).await.unwrap();
        assert!(report.cancelled);
        assert_eq!(report.objects[0].status, Status::Working);
        assert_eq!(
            std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
            [42; 64]
        );
    });
}

#[test]
fn live_attempts_exclude_concurrent_recovery_and_expired_attempts_do_not() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        let lease = std::sync::Arc::new(());
        fixture
            .apply(Action::Begin(
                fixture.claim.clone(),
                std::sync::Arc::downgrade(&lease),
            ))
            .await
            .unwrap();
        assert!(fixture.begin().await.is_err());
        assert_eq!(
            fixture.apply(Action::Read).await.unwrap().objects[0].status,
            Status::Working
        );
        drop(lease);
        assert!(fixture.begin().await.is_ok());
        assert_eq!(
            std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
            [42; 64]
        );
    });
}

#[test]
fn replacement_before_staging_is_reported_without_modifying_either_file() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        let original = fixture.root.join("original.mp4");
        std::fs::rename(fixture.root.join("recording.mp4"), &original).unwrap();
        std::fs::write(fixture.root.join("recording.mp4"), [24; 64]).unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        assert!(archive.stage_claim(&fixture.claim, None).is_err());
        assert_eq!(std::fs::read(&original).unwrap(), [42; 64]);
        assert_eq!(
            std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
            [24; 64]
        );
    });
}

#[test]
fn missing_staging_directory_is_not_proof_of_completed_removal() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        let checkpoint = Some(staged.directory_identity());
        drop(staged);
        std::fs::rename(
            fixture.root.join(".maintenance"),
            fixture.root.join("relocated-staging"),
        )
        .unwrap();
        assert!(archive.stage_claim(&fixture.claim, checkpoint).is_err());
        assert_eq!(
            std::fs::read(
                fixture
                    .root
                    .join("relocated-staging")
                    .join(&fixture.claim.token)
                    .join("recording.mp4")
            )
            .unwrap(),
            [42; 64]
        );
        assert!(!fixture.root.join(".maintenance").exists());
    });
}

#[test]
fn file_removed_before_catalog_commit_can_be_recovered_without_restart() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        fixture.begin().await.unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        let directory_identity = staged.directory_identity();
        fixture
            .apply(Action::Staged(fixture.claim.clone(), directory_identity))
            .await
            .unwrap();
        staged.remove().unwrap();
        fixture
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_deletion BEFORE DELETE ON recording_files
             BEGIN SELECT RAISE(ABORT, 'injected catalog failure'); END;",
            )
            .await
            .unwrap();
        assert!(
            fixture
                .apply(Action::Finish(fixture.claim.clone()))
                .await
                .is_err()
        );
        assert_eq!(
            fixture.apply(Action::Read).await.unwrap().objects[0].status,
            Status::Staged
        );
        fixture
            .connection
            .execute_batch("DROP TRIGGER reject_deletion")
            .await
            .unwrap();
        let resumed = fixture.begin().await.unwrap();
        assert_eq!(
            resumed.objects[0].staged_directory,
            Some(directory_identity)
        );
        assert!(
            archive
                .stage_claim(&fixture.claim, resumed.objects[0].staged_directory)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            fixture
                .apply(Action::Finish(fixture.claim.clone()))
                .await
                .unwrap()
                .deleted,
            1
        );
    });
}

#[cfg(unix)]
#[test]
fn replacement_staging_directory_is_not_proof_of_completed_removal() {
    use std::os::unix::fs::DirBuilderExt;

    pollster::block_on(async {
        let fixture = Fixture::new().await;
        fixture.begin().await.unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        fixture
            .apply(Action::Staged(
                fixture.claim.clone(),
                staged.directory_identity(),
            ))
            .await
            .unwrap();
        drop(staged);
        let directory = fixture.root.join(".maintenance").join(&fixture.claim.token);
        let relocated = fixture.root.join("relocated-staging");
        std::fs::rename(&directory, &relocated).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();

        let resumed = fixture.begin().await.unwrap();
        assert!(
            archive
                .stage_claim(&fixture.claim, resumed.objects[0].staged_directory)
                .is_err()
        );
        assert_eq!(
            std::fs::read(relocated.join("recording.mp4")).unwrap(),
            [42; 64]
        );
        assert_eq!(fixture.apply(Action::Read).await.unwrap().deleted, 0);
    });
}

#[test]
fn renamed_staged_recording_is_not_proof_of_completed_removal() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        let checkpoint = Some(staged.directory_identity());
        drop(staged);
        let directory = fixture.root.join(".maintenance").join(&fixture.claim.token);
        std::fs::rename(directory.join("recording.mp4"), directory.join("held.mp4")).unwrap();

        assert!(archive.stage_claim(&fixture.claim, checkpoint).is_err());
        assert_eq!(std::fs::read(directory.join("held.mp4")).unwrap(), [42; 64]);
        assert_eq!(fixture.apply(Action::Read).await.unwrap().deleted, 0);
    });
}

#[test]
fn missing_source_parent_allows_checkpointed_recovery() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        let parent = fixture.root.join("day");
        std::fs::create_dir(&parent).unwrap();
        let mut claim = fixture.claim.clone();
        claim.path = parent.join("recording.mp4");
        std::fs::rename(fixture.root.join("recording.mp4"), &claim.path).unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&claim, None).unwrap().unwrap();
        let checkpoint = Some(staged.directory_identity());
        staged.remove().unwrap();
        std::fs::remove_dir(parent).unwrap();

        assert!(archive.stage_claim(&claim, checkpoint).unwrap().is_none());
    });
}

#[test]
fn unexpected_staging_contents_preserve_the_selected_recording() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        let checkpoint = Some(staged.directory_identity());
        let directory = fixture.root.join(".maintenance").join(&fixture.claim.token);
        std::fs::write(directory.join("unexpected"), [24; 8]).unwrap();

        assert!(staged.remove().is_err());
        assert!(archive.stage_claim(&fixture.claim, checkpoint).is_err());
        assert_eq!(
            std::fs::read(directory.join("recording.mp4")).unwrap(),
            [42; 64]
        );
        assert_eq!(
            std::fs::read(directory.join("unexpected")).unwrap(),
            [24; 8]
        );
    });
}

#[test]
fn cancellation_preserves_started_claims_for_recovery() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        fixture.begin().await.unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        let directory_identity = staged.directory_identity();
        fixture
            .apply(Action::Staged(fixture.claim.clone(), directory_identity))
            .await
            .unwrap();
        drop(staged);
        jobs::execute(
            &fixture.connection,
            &fixture.epoch,
            Request {
                actor: "administrator".to_owned(),
                action: jobs::Action::Cancel {
                    id: fixture.job.id.clone(),
                },
                snapshot: None,
                deadline: Instant::now() + BUSY_TIMEOUT,
            },
        )
        .await
        .unwrap();
        let claims = jobs::claims::reserve(
            &fixture.connection,
            "administrator",
            &fixture.job.id,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(claims.as_slice(), std::slice::from_ref(&fixture.claim));
        let restarted = Epoch::new();
        let lease = std::sync::Arc::new(());
        execute(
            &fixture.connection,
            &restarted,
            "administrator",
            &fixture.job.id,
            Action::Begin(fixture.claim.clone(), std::sync::Arc::downgrade(&lease)),
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        assert!(
            archive
                .stage_claim(&fixture.claim, Some(directory_identity))
                .unwrap()
                .is_some()
        );
    });
}

#[test]
fn startup_reconciliation_settles_completed_unlinks_without_deleting_present_media() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        fixture.begin().await.unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        fixture
            .apply(Action::Staged(
                fixture.claim.clone(),
                staged.directory_identity(),
            ))
            .await
            .unwrap();
        drop(staged);
        let restarted = Epoch::new();
        let first = super::super::recovery::recover(
            &fixture.connection,
            &restarted,
            &archive,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(first.completed, 0);
        assert_eq!(first.unresolved, 1);
        let progress = fixture.apply(Action::Read).await.unwrap();
        assert_eq!(progress.objects[0].status, Status::Failed);
        let staged = archive
            .stage_claim(&fixture.claim, progress.objects[0].staged_directory)
            .unwrap()
            .unwrap();
        staged.remove().unwrap();

        let second = super::super::recovery::recover(
            &fixture.connection,
            &Epoch::new(),
            &archive,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(second.completed, 1);
        assert_eq!(second.unresolved, 0);
        assert_eq!(fixture.apply(Action::Read).await.unwrap().deleted, 1);
        let repeated = super::super::recovery::recover(
            &fixture.connection,
            &Epoch::new(),
            &archive,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(repeated.completed, 0);
        assert_eq!(repeated.unresolved, 0);
    });
}

#[test]
fn startup_reconciliation_preserves_a_live_sibling_attempt() {
    pollster::block_on(async {
        let fixture = Fixture::with_count(2).await;
        let claims = jobs::claims::read(
            &fixture.connection,
            &fixture.job,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        let lease = std::sync::Arc::new(());
        let before = fixture
            .apply(Action::Begin(
                claims[1].clone(),
                std::sync::Arc::downgrade(&lease),
            ))
            .await
            .unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        super::super::recovery::recover(
            &fixture.connection,
            &fixture.epoch,
            &archive,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        let after = fixture.apply(Action::Read).await.unwrap();
        assert_eq!(after.objects[0].status, Status::Failed);
        assert_eq!(after.objects[1], before.objects[1]);
        assert_eq!(
            std::fs::read(fixture.root.join("second.mp4")).unwrap(),
            [24; 64]
        );
    });
}

#[test]
fn startup_filesystem_probe_respects_the_original_expired_deadline() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        let checkpoint = staged.directory_identity();
        staged.remove().unwrap();
        let expired = Instant::now() - std::time::Duration::from_millis(1);
        let error = archive
            .check_removed_claim(&fixture.claim, checkpoint, expired)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(fixture.apply(Action::Read).await.unwrap().deleted, 0);
    });
}

#[test]
fn startup_catalog_failure_does_not_leave_a_working_executor() {
    pollster::block_on(async {
        let fixture = Fixture::new().await;
        fixture.begin().await.unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        let staged = archive.stage_claim(&fixture.claim, None).unwrap().unwrap();
        fixture
            .apply(Action::Staged(
                fixture.claim.clone(),
                staged.directory_identity(),
            ))
            .await
            .unwrap();
        staged.remove().unwrap();
        fixture
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_recovery BEFORE DELETE ON recording_files
            BEGIN SELECT RAISE(ABORT, 'injected recovery catalog failure'); END;",
            )
            .await
            .unwrap();
        let result = super::super::recovery::recover(
            &fixture.connection,
            &Epoch::new(),
            &archive,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await;
        let mut rows = fixture
            .connection
            .query("SELECT phase FROM recording_maintenance_execution", ())
            .await
            .unwrap();
        let phase = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        drop(rows);

        assert!(result.is_err());
        assert_eq!(phase, "staged");
        assert_eq!(fixture.apply(Action::Read).await.unwrap().deleted, 0);
    });
}
