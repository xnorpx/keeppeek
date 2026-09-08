use super::{Inputs, Report, Status, inspect, read};
use crate::storage::catalog::maintenance::jobs::{
    Action, Epoch, Failure, Intent, Job, Reason, Request, execute,
};
use crate::storage::catalog::maintenance::{Scope, snapshot};
use crate::storage::catalog::{BUSY_TIMEOUT, initialize_schema};
use crate::storage::long_term::inspection::Archive;
use std::{path::PathBuf, time::Instant};

struct Fixture {
    root: PathBuf,
    database: turso::Database,
    connection: turso::Connection,
    epoch: Epoch,
}

impl Fixture {
    async fn new(count: usize) -> Self {
        assert!((1..=128).contains(&count));
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let connection = database.connect().unwrap();
        initialize_schema(&connection).await.unwrap();
        let root = std::env::temp_dir().join(format!(
            "keeppeek-preflight-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        let fixture = Self {
            root,
            database,
            connection,
            epoch: Epoch::new(),
        };
        for ordinal in 0..count {
            let file_path = fixture.root.join(format!("{ordinal}.mp4"));
            std::fs::write(&file_path, [42; 64]).unwrap();
            let identity = crate::storage::catalog::recording_file_identity(
                &file_path,
                &std::fs::metadata(&file_path).unwrap(),
            );
            let started_at_ms = i64::try_from(ordinal).unwrap() * 1_000;
            fixture
                .connection
                .execute(
                    "INSERT INTO recording_files
                 (id, stream_id, source_id, logical_stream_id, started_at_ms, ended_at_ms,
                  path, init_offset, init_len, finalized, file_bytes, file_identity)
                 VALUES (?1, 'front/sub', 'front', 'sub', ?2, ?3, ?4, 0, 8, 1, 64, ?5)",
                    turso::params![
                        format!("recording-{ordinal}"),
                        started_at_ms,
                        started_at_ms + 1_000,
                        file_path.to_str().unwrap(),
                        identity
                    ],
                )
                .await
                .unwrap();
        }
        fixture
    }

    async fn queue(&self) -> Job {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let snapshot = snapshot(
            &self.connection,
            Scope::TimeRange {
                source_id: "front".to_owned(),
                stream_id: "sub".to_owned(),
                start_ms: 0,
                end_ms: 128_000,
            },
            deadline,
        )
        .await
        .unwrap();
        let prepared = execute(
            &self.connection,
            &self.epoch,
            Request {
                actor: "administrator".to_owned(),
                action: Action::Prepare(Intent {
                    scope: snapshot.scope.clone(),
                    expected_revision: snapshot.revision,
                    reason: Reason::Operator,
                }),
                snapshot: Some(snapshot),
                deadline,
            },
        )
        .await
        .unwrap();
        execute(
            &self.connection,
            &self.epoch,
            Request {
                actor: "administrator".to_owned(),
                action: Action::Confirm {
                    id: prepared.id,
                    nonce: prepared.confirmation.unwrap(),
                    expected_revision: prepared.revision,
                },
                snapshot: None,
                deadline,
            },
        )
        .await
        .unwrap()
    }

    async fn report(&self, job: &Job) -> Report {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let inputs = read(&self.connection, "administrator", &job.id, deadline)
            .await
            .unwrap();
        assert!(self.connection.is_autocommit().unwrap());
        inspect(inputs, &Archive::open(&self.root).unwrap(), deadline).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn failure(result: anyhow::Result<Inputs>, expected: Failure) {
    let error = result.err().expect("preflight must fail");
    assert_eq!(error.downcast_ref::<Failure>(), Some(&expected));
}

#[test]
fn replacement_after_the_catalog_read_is_compared_with_the_original_identity() {
    pollster::block_on(async {
        let fixture = Fixture::new(1).await;
        let job = fixture.queue().await;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let inputs = read(&fixture.connection, "administrator", &job.id, deadline)
            .await
            .unwrap();
        let recording = fixture.root.join("0.mp4");
        std::fs::rename(&recording, fixture.root.join("original.mp4")).unwrap();
        std::fs::write(&recording, [24; 64]).unwrap();
        let replacement_identity = crate::storage::catalog::recording_file_identity(
            &recording,
            &std::fs::metadata(&recording).unwrap(),
        );
        fixture
            .connection
            .execute(
                "UPDATE recording_files SET file_identity = ?1",
                turso::params![replacement_identity],
            )
            .await
            .unwrap();
        let report = inspect(inputs, &Archive::open(&fixture.root).unwrap(), deadline).unwrap();
        assert_eq!(report.objects[0].status, Status::IdentityChanged);
        assert_eq!(report.catalog_revision, job.revision);
        assert_eq!(
            std::fs::read(fixture.root.join("original.mp4")).unwrap(),
            [42; 64]
        );
        assert_eq!(std::fs::read(recording).unwrap(), [24; 64]);
    });
}

#[test]
fn missing_catalog_identity_does_not_become_present_from_path_and_size_alone() {
    pollster::block_on(async {
        let fixture = Fixture::new(1).await;
        let job = fixture.queue().await;
        fixture
            .connection
            .execute("UPDATE recording_files SET file_identity = NULL", ())
            .await
            .unwrap();
        let report = fixture.report(&job).await;
        assert_eq!(report.objects[0].status, Status::IdentityUnavailable);
        assert_eq!(std::fs::read(fixture.root.join("0.mp4")).unwrap(), [42; 64]);
        std::fs::remove_file(fixture.root.join("0.mp4")).unwrap();
        assert_eq!(
            fixture.report(&job).await.objects[0].status,
            Status::MissingFile
        );
    });
}

#[test]
fn malformed_catalog_identifiers_fail_closed_without_exposing_their_values() {
    pollster::block_on(async {
        let fixture = Fixture::new(1).await;
        let job = fixture.queue().await;
        for invalid in [
            "",
            "private-path",
            "1",
            "1:2:3",
            ":1",
            "1:",
            "+1:2",
            "1:-2",
            "18446744073709551616:2",
            &"private-path".repeat(100_000),
        ] {
            fixture
                .connection
                .execute(
                    "UPDATE recording_files SET file_identity = ?1",
                    turso::params![invalid],
                )
                .await
                .unwrap();
            let result = read(
                &fixture.connection,
                "administrator",
                &job.id,
                Instant::now() + BUSY_TIMEOUT,
            )
            .await;
            failure(result, Failure::Invalid);
            assert!(fixture.connection.is_autocommit().unwrap());
            assert_eq!(std::fs::read(fixture.root.join("0.mp4")).unwrap(), [42; 64]);
        }
    });
}

#[test]
fn preflight_uses_the_search_worker_when_the_writer_queue_is_full() {
    use crate::storage::catalog::maintenance::ReadRequest;
    use crate::storage::catalog::{Command, RecordingCatalogHandle, SearchCommand};
    use std::sync::mpsc;

    pollster::block_on(async {
        let fixture = Fixture::new(1).await;
        let job = fixture.queue().await;
        let (write_tx, write_rx) = mpsc::sync_channel(1);
        write_tx.send(Command::Shutdown).unwrap();
        let (search_tx, search_rx) = mpsc::sync_channel(1);
        let handle = RecordingCatalogHandle {
            tx: write_tx,
            search_tx,
        };
        let connection = fixture.database.connect().unwrap();
        let worker = std::thread::spawn(move || {
            let SearchCommand::Maintenance(ReadRequest::Preflight {
                actor,
                id,
                deadline,
                reply,
            }) = search_rx.recv_timeout(BUSY_TIMEOUT).unwrap()
            else {
                panic!("preflight must use the search worker");
            };
            assert!(deadline > Instant::now());
            let result = pollster::block_on(read(&connection, &actor, &id, deadline));
            assert!(connection.is_autocommit().unwrap());
            assert!(reply.send(result).is_ok());
        });
        let report = handle
            .recording_deletion_preflight(
                "administrator",
                &job.id,
                &Archive::open(&fixture.root).unwrap(),
            )
            .unwrap();
        worker.join().unwrap();
        assert_eq!(report.objects[0].status, Status::Present);
        assert!(matches!(write_rx.try_recv(), Ok(Command::Shutdown)));
    });
}

#[test]
fn full_and_disconnected_read_queues_fail_without_admitting_preflight_work() {
    use crate::storage::catalog::{RecordingCatalogHandle, SearchCommand};
    use std::sync::mpsc;

    pollster::block_on(async {
        let fixture = Fixture::new(1).await;
        let archive = Archive::open(&fixture.root).unwrap();
        let (write_tx, _write_rx) = mpsc::sync_channel(1);
        let (search_tx, search_rx) = mpsc::sync_channel(1);
        search_tx.send(SearchCommand::Shutdown).unwrap();
        let handle = RecordingCatalogHandle {
            tx: write_tx,
            search_tx,
        };
        let error = handle
            .recording_deletion_preflight("administrator", "z".repeat(32), &archive)
            .unwrap_err();
        assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::Invalid));
        let error = handle
            .recording_deletion_preflight("administrator", "0".repeat(32), &archive)
            .unwrap_err();
        assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::Unavailable));
        assert!(matches!(search_rx.try_recv(), Ok(SearchCommand::Shutdown)));
        drop(search_rx);
        let error = handle
            .recording_deletion_preflight("administrator", "0".repeat(32), &archive)
            .unwrap_err();
        assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::Unavailable));
    });
}

#[test]
fn job_revision_and_current_rows_share_one_snapshot_while_another_connection_writes() {
    pollster::block_on(async {
        let fixture = Fixture::new(1).await;
        let job = fixture.queue().await;
        fixture.connection.execute_batch("BEGIN").await.unwrap();
        let mut rows = fixture
            .connection
            .query(
                "SELECT revision FROM recording_catalog_state WHERE id = 1",
                (),
            )
            .await
            .unwrap();
        let original: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        drop(rows);
        let writer = fixture.database.connect().unwrap();
        writer
            .execute("UPDATE recording_files SET protected = 1", ())
            .await
            .unwrap();
        let inputs = super::read_inputs(
            &fixture.connection,
            "administrator",
            &job.id,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        fixture.connection.execute_batch("COMMIT").await.unwrap();
        let report = inspect(
            inputs,
            &Archive::open(&fixture.root).unwrap(),
            Instant::now() + BUSY_TIMEOUT,
        )
        .unwrap();
        assert_eq!(report.catalog_revision, u64::try_from(original).unwrap());
        assert_eq!(report.objects[0].status, Status::Present);
        let fresh = fixture.report(&job).await;
        assert_eq!(fresh.objects[0].status, Status::ProtectedRecording);
        assert!(fresh.catalog_revision > report.catalog_revision);
    });
}

#[test]
fn maximum_preflight_is_complete_and_keeps_unknown_files_untouched() {
    pollster::block_on(async {
        let fixture = Fixture::new(128).await;
        let job = fixture.queue().await;
        std::fs::write(fixture.root.join("unknown.mp4"), [24; 64]).unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        let mut baseline = Vec::with_capacity(30);
        let mut preflight = Vec::with_capacity(30);
        for _ in 0..30 {
            let started = Instant::now();
            fixture.connection.execute_batch("BEGIN").await.unwrap();
            let (loaded, _) = super::super::load(
                &fixture.connection,
                "administrator",
                &job.id,
                started + BUSY_TIMEOUT,
            )
            .await
            .unwrap();
            fixture.connection.execute_batch("COMMIT").await.unwrap();
            baseline.push(started.elapsed());
            assert_eq!(loaded, job);
            let started = Instant::now();
            let inputs = read(
                &fixture.connection,
                "administrator",
                &job.id,
                started + BUSY_TIMEOUT,
            )
            .await
            .unwrap();
            let report = inspect(inputs, &archive, started + BUSY_TIMEOUT).unwrap();
            preflight.push(started.elapsed());
            assert_eq!(report.objects.len(), 128);
            for (ordinal, object) in report.objects.iter().enumerate() {
                assert_eq!(object.recording_id, format!("recording-{ordinal}"));
                assert_eq!(object.status, Status::Present);
            }
        }
        assert!(preflight.iter().all(|elapsed| *elapsed < BUSY_TIMEOUT));
        timings("LEDGER_READ_128", baseline);
        timings("PREFLIGHT_128", preflight);
        assert_eq!(
            std::fs::read(fixture.root.join("unknown.mp4")).unwrap(),
            [24; 64]
        );
    });
}

fn timings(label: &str, mut samples: Vec<std::time::Duration>) {
    assert_eq!(samples.len(), 30);
    samples.sort_unstable();
    println!(
        "{label} runs=30 median_ms={:.3} p95_ms={:.3} max_ms={:.3} budget_ms=2000",
        samples[15].as_secs_f64() * 1_000.0,
        samples[28].as_secs_f64() * 1_000.0,
        samples[29].as_secs_f64() * 1_000.0
    );
}

#[cfg(unix)]
#[test]
fn an_ancestor_swap_after_the_catalog_read_cannot_redirect_file_inspection() {
    pollster::block_on(async {
        let fixture = Fixture::new(1).await;
        let outside = Fixture::new(1).await;
        std::fs::write(outside.root.join("0.mp4"), [24; 64]).unwrap();
        let camera = fixture.root.join("camera");
        std::fs::create_dir(&camera).unwrap();
        std::fs::rename(fixture.root.join("0.mp4"), camera.join("0.mp4")).unwrap();
        fixture
            .connection
            .execute(
                "UPDATE recording_files SET path = ?1",
                turso::params![camera.join("0.mp4").to_str().unwrap()],
            )
            .await
            .unwrap();
        let job = fixture.queue().await;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let inputs = read(&fixture.connection, "administrator", &job.id, deadline)
            .await
            .unwrap();
        assert!(fixture.connection.is_autocommit().unwrap());
        std::fs::rename(&camera, fixture.root.join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside.root, &camera).unwrap();
        let report = inspect(inputs, &Archive::open(&fixture.root).unwrap(), deadline).unwrap();
        assert!(matches!(
            report.objects[0].status,
            Status::PathRejected | Status::InspectionFailed
        ));
        assert_eq!(
            std::fs::read(fixture.root.join("moved/0.mp4")).unwrap(),
            [42; 64]
        );
        assert_eq!(std::fs::read(outside.root.join("0.mp4")).unwrap(), [24; 64]);
    });
}

#[test]
fn search_errors_do_not_expose_internal_paths_to_the_preflight_caller() {
    use crate::storage::catalog::maintenance::ReadRequest;
    use crate::storage::catalog::{RecordingCatalogHandle, SearchCommand};
    use std::sync::mpsc;

    pollster::block_on(async {
        let fixture = Fixture::new(1).await;
        let archive = Archive::open(&fixture.root).unwrap();
        let (write_tx, _write_rx) = mpsc::sync_channel(1);
        let (search_tx, search_rx) = mpsc::sync_channel(1);
        let handle = RecordingCatalogHandle {
            tx: write_tx,
            search_tx,
        };
        let diagnostic_path = fixture.root.join("private-camera.mp4");
        let worker = std::thread::spawn(move || {
            let SearchCommand::Maintenance(ReadRequest::Preflight { reply, .. }) =
                search_rx.recv_timeout(BUSY_TIMEOUT).unwrap()
            else {
                panic!("preflight must use the search worker");
            };
            let result = Err(anyhow::anyhow!(
                "read failed at {}",
                diagnostic_path.display()
            ));
            assert!(reply.send(result).is_ok());
        });
        let error = handle
            .recording_deletion_preflight("administrator", "0".repeat(32), &archive)
            .unwrap_err();
        worker.join().unwrap();
        assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::Unavailable));
        assert!(!format!("{error:?}").contains(fixture.root.to_str().unwrap()));
        assert!(!format!("{error:?}").contains("private-camera.mp4"));
    });
}

#[test]
fn mixed_file_and_catalog_absence_never_claims_that_unknown_media_is_gone() {
    pollster::block_on(async {
        let fixture = Fixture::new(4).await;
        let job = fixture.queue().await;
        std::fs::remove_file(fixture.root.join("1.mp4")).unwrap();
        fixture
            .connection
            .execute(
                "DELETE FROM recording_files WHERE id IN ('recording-2', 'recording-3')",
                (),
            )
            .await
            .unwrap();
        std::fs::remove_file(fixture.root.join("3.mp4")).unwrap();
        let report = fixture.report(&job).await;
        let observations: Vec<_> = report
            .objects
            .iter()
            .map(|object| (object.recording_id.as_str(), object.status))
            .collect();
        assert_eq!(
            observations,
            [
                ("recording-0", Status::Present),
                ("recording-1", Status::MissingFile),
                ("recording-2", Status::MissingCatalog),
                ("recording-3", Status::MissingCatalog),
            ]
        );
        assert!(report.catalog_revision > report.planned_revision);
        assert_eq!(std::fs::read(fixture.root.join("0.mp4")).unwrap(), [42; 64]);
        assert_eq!(std::fs::read(fixture.root.join("2.mp4")).unwrap(), [42; 64]);
        assert_eq!(fixture.report(&job).await, report);
    });
}

#[test]
fn catalog_blockers_and_changed_scope_are_reported_before_filesystem_checks() {
    pollster::block_on(async {
        for (mutation, expected) in [
            ("finalized = 0", Status::ActiveRecording),
            ("protected = 1", Status::ProtectedRecording),
            ("cleanup_pending = 1", Status::CleanupPending),
            ("source_id = 'other'", Status::CatalogChanged),
            ("logical_stream_id = 'main'", Status::CatalogChanged),
            ("source_id = NULL", Status::CatalogChanged),
            ("started_at_ms = 1", Status::CatalogChanged),
            ("ended_at_ms = NULL", Status::CatalogChanged),
            ("file_bytes = 65", Status::CatalogChanged),
        ] {
            let fixture = Fixture::new(1).await;
            let job = fixture.queue().await;
            fixture
                .connection
                .execute_batch(&format!("UPDATE recording_files SET {mutation}"))
                .await
                .unwrap();
            std::fs::remove_file(fixture.root.join("0.mp4")).unwrap();
            let report = fixture.report(&job).await;
            assert_eq!(report.objects[0].status, expected);
            assert!(report.catalog_revision > job.revision);
        }
    });
}

#[test]
fn corrupt_rows_roll_back_the_read_and_allow_a_later_valid_request() {
    pollster::block_on(async {
        for (mutation, repair) in [
            (
                "UPDATE recording_files SET finalized = 2",
                "UPDATE recording_files SET finalized = 1",
            ),
            (
                "UPDATE recording_files SET file_bytes = -1",
                "UPDATE recording_files SET file_bytes = 64",
            ),
            (
                "UPDATE recording_maintenance_objects SET recording_id = 'substitute'",
                "UPDATE recording_maintenance_objects SET recording_id = 'recording-0'",
            ),
            (
                "UPDATE recording_catalog_state SET revision = -1",
                "UPDATE recording_catalog_state SET revision = 1",
            ),
        ] {
            let fixture = Fixture::new(1).await;
            let job = fixture.queue().await;
            fixture.connection.execute_batch(mutation).await.unwrap();
            failure(
                read(
                    &fixture.connection,
                    "administrator",
                    &job.id,
                    Instant::now() + BUSY_TIMEOUT,
                )
                .await,
                Failure::Invalid,
            );
            assert!(fixture.connection.is_autocommit().unwrap());
            fixture.connection.execute_batch(repair).await.unwrap();
            assert_eq!(
                fixture.report(&job).await.objects[0].status,
                Status::Present
            );
            assert_eq!(std::fs::read(fixture.root.join("0.mp4")).unwrap(), [42; 64]);
        }
    });
}

#[test]
fn overlong_catalog_paths_are_rejected_without_being_returned_in_reports() {
    pollster::block_on(async {
        let fixture = Fixture::new(1).await;
        let job = fixture.queue().await;
        fixture
            .connection
            .execute(
                "UPDATE recording_files SET path = ?1",
                turso::params!["private-location".repeat(300)],
            )
            .await
            .unwrap();
        let report = fixture.report(&job).await;
        assert_eq!(report.objects[0].status, Status::PathRejected);
        assert!(!format!("{report:?}").contains("private-location"));
        assert_eq!(std::fs::read(fixture.root.join("0.mp4")).unwrap(), [42; 64]);
    });
}

#[test]
fn one_deadline_covers_catalog_work_and_every_filesystem_check() {
    pollster::block_on(async {
        let fixture = Fixture::new(2).await;
        let job = fixture.queue().await;
        let expired = Instant::now();
        let error = read(&fixture.connection, "administrator", &job.id, expired)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("deadline expired"));
        assert!(fixture.connection.is_autocommit().unwrap());
        let inputs = read(
            &fixture.connection,
            "administrator",
            &job.id,
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        let archive = Archive::open(&fixture.root).unwrap();
        assert!(inspect(inputs, &archive, expired).is_err());
        let error = archive
            .inspect_until(fixture.root.join("missing.mp4"), 64, expired)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(fixture.report(&job).await.objects.len(), 2);
        assert_eq!(std::fs::read(fixture.root.join("0.mp4")).unwrap(), [42; 64]);
        assert_eq!(std::fs::read(fixture.root.join("1.mp4")).unwrap(), [42; 64]);
    });
}
