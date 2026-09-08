use super::{
    Action, Failure, NOW_MS, Scope, Snapshot, State, assert_failure, confirmation, fixture,
    prepare, read, run,
};
use crate::storage::catalog::BUSY_TIMEOUT;
use crate::storage::catalog::maintenance::{MAX_RECORDINGS, jobs::ObjectState, snapshot};
use std::time::Instant;

#[test]
fn maximum_scope_confirmation_preserves_every_object_within_the_wait_budget() {
    pollster::block_on(async {
        let connection = fixture().await;
        let original = selection(&connection, MAX_RECORDINGS).await;
        let mut samples = Vec::with_capacity(30);
        for _ in 0..30 {
            let prepared = prepare(&connection, original.clone(), NOW_MS)
                .await
                .unwrap();
            let started = Instant::now();
            let queued = run(&connection, confirmation(&prepared), None, NOW_MS + 1)
                .await
                .unwrap();
            samples.push(started.elapsed());
            assert_eq!(queued.objects.len(), MAX_RECORDINGS);
            assert_eq!(queued.snapshot.catalog_bytes, 8_192);
            for (object, recording) in queued.objects.iter().zip(&original.recordings) {
                assert_eq!(object.recording_id, recording.recording_id);
                assert_eq!(object.state, ObjectState::Queued);
            }
        }
        assert_eq!(object_count(&connection).await, 3_840);
        samples.sort_unstable();
        assert!(samples[29] < BUSY_TIMEOUT);
        println!(
            "LEDGER_CONFIRM_128 runs=30 median_ms={:.3} p95_ms={:.3} max_ms={:.3} budget_ms=2000",
            samples[15].as_secs_f64() * 1_000.0,
            samples[28].as_secs_f64() * 1_000.0,
            samples[29].as_secs_f64() * 1_000.0,
        );
    });
}

#[test]
fn corrupt_ledgers_fail_closed_without_reconstructing_entries() {
    pollster::block_on(async {
        for mutation in [
            "DELETE FROM recording_maintenance_objects WHERE ordinal = 1",
            "UPDATE recording_maintenance_objects SET recording_id = 'substitute' WHERE ordinal = 1",
            "UPDATE recording_maintenance_objects SET ordinal = 3 WHERE ordinal = 1",
            "UPDATE recording_maintenance_objects SET state = 'cancelled' WHERE ordinal = 1",
            "INSERT INTO recording_maintenance_objects
             SELECT id, 2, 'unselected', 'queued' FROM recording_maintenance_intents",
        ] {
            let connection = fixture().await;
            let prepared = prepare(&connection, selection(&connection, 2).await, NOW_MS)
                .await
                .unwrap();
            let queued = run(&connection, confirmation(&prepared), None, NOW_MS + 1)
                .await
                .unwrap();
            connection.execute_batch(mutation).await.unwrap();
            let count = object_count(&connection).await;
            for action in [
                Action::Read {
                    id: queued.id.clone(),
                },
                confirmation(&prepared),
                Action::Cancel {
                    id: queued.id.clone(),
                },
            ] {
                assert_failure(
                    run(&connection, action, None, NOW_MS + 2).await,
                    Failure::Invalid,
                );
                assert!(connection.is_autocommit().unwrap());
                assert_eq!(object_count(&connection).await, count);
            }
        }
    });
}

#[test]
fn pruning_cancellation_history_preserves_queued_objects() {
    pollster::block_on(async {
        let connection = fixture().await;
        let original = selection(&connection, 2).await;
        let keep = prepare(&connection, original.clone(), NOW_MS)
            .await
            .unwrap();
        let queued = run(&connection, confirmation(&keep), None, NOW_MS + 1)
            .await
            .unwrap();
        let discard = prepare(&connection, original.clone(), NOW_MS)
            .await
            .unwrap();
        run(&connection, confirmation(&discard), None, NOW_MS + 1)
            .await
            .unwrap();
        run(
            &connection,
            Action::Cancel {
                id: discard.id.clone(),
            },
            None,
            NOW_MS + 2,
        )
        .await
        .unwrap();
        assert_eq!(object_count(&connection).await, 4);
        let cutoff = NOW_MS + 2 + super::CANCEL_RETENTION_MS;
        prepare(&connection, original, cutoff).await.unwrap();
        assert_failure(
            run(&connection, Action::Read { id: discard.id }, None, cutoff).await,
            Failure::NotFound,
        );
        assert_eq!(object_count(&connection).await, 2);
        assert_eq!(read(&connection, &queued.id).await, queued);
    });
}

#[test]
fn expired_ledger_operations_do_not_extend_the_request_budget() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, selection(&connection, 1).await, NOW_MS)
            .await
            .unwrap();
        let deadline = Instant::now();
        let error = super::super::ledger::enqueue(&connection, &prepared, deadline)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("deadline expired"));
        let error = super::super::ledger::read(&connection, &prepared, deadline)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("deadline expired"));
        assert_eq!(object_count(&connection).await, 0);
        assert!(connection.is_autocommit().unwrap());
    });
}

#[test]
fn confirmed_ledger_is_independent_of_later_recording_catalog_changes() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, selection(&connection, 2).await, NOW_MS)
            .await
            .unwrap();
        let queued = run(&connection, confirmation(&prepared), None, NOW_MS + 1)
            .await
            .unwrap();
        connection
            .execute_batch("DELETE FROM recording_files")
            .await
            .unwrap();
        assert_eq!(read(&connection, &queued.id).await, queued);
        assert_eq!(
            run(&connection, confirmation(&prepared), None, NOW_MS + 2)
                .await
                .unwrap(),
            queued
        );
        assert_eq!(object_count(&connection).await, 2);
    });
}

async fn selection(connection: &turso::Connection, count: usize) -> Snapshot {
    assert!((1..=MAX_RECORDINGS).contains(&count));
    for ordinal in 1..count {
        let started_at_ms = i64::try_from(ordinal + 1).unwrap() * 1_000;
        connection
            .execute(
                "INSERT INTO recording_files
             (id, stream_id, source_id, logical_stream_id, started_at_ms, ended_at_ms,
              path, init_offset, init_len, finalized, file_bytes)
             VALUES (?1, 'front/sub', 'front', 'sub', ?2, ?3, ?4, 0, 8, 1, 64)",
                turso::params![
                    format!("recording-{ordinal}"),
                    started_at_ms,
                    started_at_ms + 1_000,
                    format!("synthetic-{ordinal}.mp4")
                ],
            )
            .await
            .unwrap();
    }
    snapshot(
        connection,
        Scope::TimeRange {
            source_id: "front".to_owned(),
            stream_id: "sub".to_owned(),
            start_ms: 1_000,
            end_ms: i64::try_from(count + 1).unwrap() * 1_000,
        },
        Instant::now() + BUSY_TIMEOUT,
    )
    .await
    .unwrap()
}

async fn object_count(connection: &turso::Connection) -> i64 {
    let mut rows = connection
        .query("SELECT count(*) FROM recording_maintenance_objects", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

#[test]
fn failure_after_the_first_object_rolls_back_the_entire_confirmation() {
    pollster::block_on(async {
        let connection = fixture().await;
        let original = selection(&connection, 2).await;
        let prepared = prepare(&connection, original.clone(), NOW_MS)
            .await
            .unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_second_object BEFORE INSERT ON recording_maintenance_objects
             WHEN NEW.ordinal = 1 BEGIN SELECT RAISE(ABORT, 'injected ledger failure'); END;",
            )
            .await
            .unwrap();
        assert!(
            run(&connection, confirmation(&prepared), None, NOW_MS + 1)
                .await
                .is_err()
        );
        assert!(connection.is_autocommit().unwrap());
        assert_eq!(object_count(&connection).await, 0);
        let unchanged = read(&connection, &prepared.id).await;
        assert_eq!(unchanged.state, State::Prepared);
        assert_eq!(unchanged.confirmed_at_ms, None);
        connection
            .execute_batch("DROP TRIGGER reject_second_object")
            .await
            .unwrap();
        let queued = run(&connection, confirmation(&prepared), None, NOW_MS + 2)
            .await
            .unwrap();
        assert_eq!(queued.objects.len(), 2);
        assert_eq!(queued.snapshot, original);
    });
}

#[test]
fn cancellation_failure_preserves_both_queued_ledger_and_job() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, selection(&connection, 2).await, NOW_MS)
            .await
            .unwrap();
        let queued = run(&connection, confirmation(&prepared), None, NOW_MS + 1)
            .await
            .unwrap();
        connection.execute_batch(
            "CREATE TRIGGER reject_job_cancel BEFORE UPDATE ON recording_maintenance_intents
             WHEN NEW.state = 'cancelled' BEGIN SELECT RAISE(ABORT, 'injected cancel failure'); END;",
        ).await.unwrap();
        let cancel = Action::Cancel {
            id: queued.id.clone(),
        };
        assert!(
            run(&connection, cancel.clone(), None, NOW_MS + 2)
                .await
                .is_err()
        );
        assert_eq!(read(&connection, &queued.id).await, queued);
        assert!(connection.is_autocommit().unwrap());
        connection
            .execute_batch("DROP TRIGGER reject_job_cancel")
            .await
            .unwrap();
        let cancelled = run(&connection, cancel, None, NOW_MS + 3).await.unwrap();
        assert_eq!(cancelled.state, State::Cancelled);
        assert_eq!(cancelled.objects.len(), 2);
        assert!(
            cancelled
                .objects
                .iter()
                .all(|object| object.state == ObjectState::Cancelled)
        );
    });
}

#[test]
fn cancellation_rejects_clock_rollback_before_confirmation() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, selection(&connection, 1).await, NOW_MS)
            .await
            .unwrap();
        let queued = run(&connection, confirmation(&prepared), None, NOW_MS + 10)
            .await
            .unwrap();
        assert_failure(
            run(
                &connection,
                Action::Cancel {
                    id: queued.id.clone(),
                },
                None,
                NOW_MS + 9,
            )
            .await,
            Failure::Invalid,
        );
        assert_eq!(read(&connection, &queued.id).await, queued);
        assert!(connection.is_autocommit().unwrap());
    });
}
