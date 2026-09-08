use super::{
    Action, CANCEL_RETENTION_MS, Failure, Intent, Job, MAX_JOBS, MAX_PLANS, Moment, Nonce,
    PLAN_TTL_MS, Reason, Request, Scope, Snapshot, State, execute_with_clock,
};
use crate::storage::catalog::{BUSY_TIMEOUT, initialize_schema};
use std::time::Instant;

const NOW_MS: i64 = 1_800_000_000_000;

mod ledger_tests;

#[test]
fn monotonic_expiry_wins_when_wall_clock_still_appears_valid() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, inspect(&connection).await, NOW_MS)
            .await
            .unwrap();
        let confirmation_request = request(confirmation(&prepared), None);
        assert_failure(
            execute_with_clock(&connection, confirmation_request, || {
                Ok(Moment {
                    epoch: "test-catalog".to_owned(),
                    utc_ms: NOW_MS + 1,
                    elapsed_ms: PLAN_TTL_MS,
                })
            })
            .await,
            Failure::Expired,
        );
        let expired = execute_with_clock(
            &connection,
            request(Action::Read { id: prepared.id }, None),
            || {
                Ok(Moment {
                    epoch: "test-catalog".to_owned(),
                    utc_ms: NOW_MS + 1,
                    elapsed_ms: PLAN_TTL_MS,
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(expired.state, State::Expired);
        assert_eq!(expired.confirmed_at_ms, None);
    });
}

#[test]
fn catalog_restart_invalidates_only_unconfirmed_intent() {
    pollster::block_on(async {
        let connection = fixture().await;
        let snapshot = inspect(&connection).await;
        let unconfirmed = prepare(&connection, snapshot.clone(), NOW_MS)
            .await
            .unwrap();
        let confirmed = prepare(&connection, snapshot, NOW_MS).await.unwrap();
        let queued = run(&connection, confirmation(&confirmed), None, NOW_MS + 1)
            .await
            .unwrap();
        let new_epoch = || {
            Ok(Moment {
                epoch: "restarted-catalog".to_owned(),
                utc_ms: NOW_MS + 2,
                elapsed_ms: 0,
            })
        };
        assert_failure(
            execute_with_clock(
                &connection,
                request(confirmation(&unconfirmed), None),
                new_epoch,
            )
            .await,
            Failure::Expired,
        );
        let retrieved = execute_with_clock(
            &connection,
            request(confirmation(&confirmed), None),
            new_epoch,
        )
        .await
        .unwrap();
        assert_eq!(retrieved, queued);
        let expired = execute_with_clock(
            &connection,
            request(Action::Read { id: unconfirmed.id }, None),
            new_epoch,
        )
        .await
        .unwrap();
        assert_eq!(expired.state, State::Expired);
    });
}

#[test]
fn clock_is_sampled_after_transaction_acquisition() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, inspect(&connection).await, NOW_MS)
            .await
            .unwrap();
        let confirmed =
            execute_with_clock(&connection, request(confirmation(&prepared), None), || {
                assert!(!connection.is_autocommit().unwrap());
                Ok(Moment {
                    epoch: "test-catalog".to_owned(),
                    utc_ms: NOW_MS + 1,
                    elapsed_ms: 1,
                })
            })
            .await
            .unwrap();
        assert_eq!(confirmed.confirmed_at_ms, Some(NOW_MS + 1));
        assert!(connection.is_autocommit().unwrap());
    });
}

#[test]
fn clock_rollback_before_plan_creation_rejects_confirmation() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, inspect(&connection).await, NOW_MS)
            .await
            .unwrap();
        assert_failure(
            run(&connection, confirmation(&prepared), None, NOW_MS - 1).await,
            Failure::Expired,
        );
        assert_eq!(read(&connection, &prepared.id).await.state, State::Prepared);
    });
}

#[test]
fn stale_prepare_and_confirm_rollback_without_touching_recording_state() {
    pollster::block_on(async {
        let connection = fixture().await;
        let snapshot = inspect(&connection).await;
        connection
            .execute("UPDATE recording_files SET protected = 1", ())
            .await
            .unwrap();
        assert_failure(
            prepare(&connection, snapshot, NOW_MS).await,
            Failure::Conflict,
        );
        assert_eq!(intent_count(&connection).await, 0);
        connection
            .execute("UPDATE recording_files SET protected = 0", ())
            .await
            .unwrap();
        let prepared = prepare(&connection, inspect(&connection).await, NOW_MS)
            .await
            .unwrap();
        connection
            .execute("UPDATE recording_files SET protected = 1", ())
            .await
            .unwrap();
        assert_failure(
            run(&connection, confirmation(&prepared), None, NOW_MS).await,
            Failure::Conflict,
        );
        let job = read(&connection, &prepared.id).await;
        assert_eq!(job.state, State::Prepared);
        assert_eq!(job.confirmed_at_ms, None);
        assert!(connection.is_autocommit().unwrap());
        assert!(inspect(&connection).await.recordings[0].protected);
    });
}

#[test]
fn active_protected_pending_unknown_end_and_empty_scopes_cannot_be_prepared() {
    pollster::block_on(async {
        let connection = fixture().await;
        for update in [
            "UPDATE recording_files SET finalized = 0",
            "UPDATE recording_files SET protected = 1",
            "UPDATE recording_files SET cleanup_pending = 1",
            "UPDATE recording_files SET ended_at_ms = NULL",
        ] {
            connection.execute_batch(update).await.unwrap();
            assert_failure(
                prepare(&connection, inspect(&connection).await, NOW_MS).await,
                Failure::Blocked,
            );
            assert!(connection.is_autocommit().unwrap());
            connection
                .execute_batch(
                    "UPDATE recording_files SET finalized = 1, protected = 0,
                    cleanup_pending = 0, ended_at_ms = 2000",
                )
                .await
                .unwrap();
        }
        connection
            .execute_batch("DELETE FROM recording_files")
            .await
            .unwrap();
        assert_failure(
            prepare(&connection, inspect(&connection).await, NOW_MS).await,
            Failure::NotFound,
        );
        assert_eq!(intent_count(&connection).await, 0);
    });
}

#[test]
fn wrong_nonce_and_revision_are_rejected_and_expiry_is_exclusive() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, inspect(&connection).await, NOW_MS)
            .await
            .unwrap();
        let wrong_nonce = Action::Confirm {
            id: prepared.id.clone(),
            nonce: Nonce::generate(),
            expected_revision: prepared.revision,
        };
        assert_failure(
            run(&connection, wrong_nonce, None, NOW_MS).await,
            Failure::NotFound,
        );
        let wrong_revision = Action::Confirm {
            id: prepared.id.clone(),
            nonce: prepared.confirmation.clone().unwrap(),
            expected_revision: prepared.revision + 1,
        };
        assert_failure(
            run(&connection, wrong_revision, None, NOW_MS).await,
            Failure::Conflict,
        );
        assert_failure(
            run(
                &connection,
                confirmation(&prepared),
                None,
                prepared.expires_at_ms,
            )
            .await,
            Failure::Expired,
        );
        assert_eq!(read(&connection, &prepared.id).await.state, State::Prepared);
        let confirmed = run(
            &connection,
            confirmation(&prepared),
            None,
            prepared.expires_at_ms - 1,
        )
        .await
        .unwrap();
        assert_eq!(confirmed.state, State::Queued);
        assert!(confirmed.confirmation.is_none());
    });
}

#[test]
fn queued_retry_is_idempotent_after_expiry_and_unrelated_catalog_changes() {
    pollster::block_on(async {
        let connection = fixture().await;
        let original = inspect(&connection).await;
        let prepared = prepare(&connection, original.clone(), NOW_MS)
            .await
            .unwrap();
        let queued = run(&connection, confirmation(&prepared), None, NOW_MS + 1)
            .await
            .unwrap();
        assert_eq!(queued.reason, Reason::Privacy);
        assert_eq!(queued.confirmed_at_ms, Some(NOW_MS + 1));
        assert_eq!(inspect(&connection).await, original);
        connection
            .execute("UPDATE recording_files SET protected = 1", ())
            .await
            .unwrap();
        let retried = run(
            &connection,
            confirmation(&prepared),
            None,
            NOW_MS + PLAN_TTL_MS + 1,
        )
        .await
        .unwrap();
        assert_eq!(queued, retried);
        assert_eq!(intent_count(&connection).await, 1);
    });
}

#[test]
fn cancel_is_durable_idempotent_and_never_resurrects_a_confirmed_job() {
    pollster::block_on(async {
        let connection = fixture().await;
        let original = inspect(&connection).await;
        let prepared = prepare(&connection, original.clone(), NOW_MS)
            .await
            .unwrap();
        run(&connection, confirmation(&prepared), None, NOW_MS + 1)
            .await
            .unwrap();
        let cancel = Action::Cancel {
            id: prepared.id.clone(),
        };
        let cancelled = run(&connection, cancel.clone(), None, NOW_MS + 2)
            .await
            .unwrap();
        assert_eq!(cancelled.state, State::Cancelled);
        assert_eq!(cancelled.cancelled_at_ms, Some(NOW_MS + 2));
        assert_eq!(
            run(&connection, cancel, None, NOW_MS + 10).await.unwrap(),
            cancelled
        );
        assert_eq!(read(&connection, &prepared.id).await, cancelled);
        assert_failure(
            run(&connection, confirmation(&prepared), None, NOW_MS + 3).await,
            Failure::InvalidState,
        );
        assert_eq!(inspect(&connection).await, original);
    });
}

#[test]
fn quotas_reject_new_work_without_evicting_confirmed_jobs() {
    pollster::block_on(async {
        let connection = fixture().await;
        let snapshot = inspect(&connection).await;
        let mut plans = Vec::new();
        for _ in 0..MAX_PLANS {
            plans.push(
                prepare(&connection, snapshot.clone(), NOW_MS)
                    .await
                    .unwrap(),
            );
        }
        assert_failure(
            prepare(&connection, snapshot.clone(), NOW_MS).await,
            Failure::Quota,
        );
        for plan in &plans {
            run(&connection, confirmation(plan), None, NOW_MS + 1)
                .await
                .unwrap();
        }
        for _ in MAX_PLANS..MAX_JOBS {
            let plan = prepare(&connection, snapshot.clone(), NOW_MS)
                .await
                .unwrap();
            run(&connection, confirmation(&plan), None, NOW_MS + 1)
                .await
                .unwrap();
        }
        let excess = prepare(&connection, snapshot.clone(), NOW_MS)
            .await
            .unwrap();
        assert_failure(
            run(&connection, confirmation(&excess), None, NOW_MS + 2).await,
            Failure::Quota,
        );
        assert_eq!(read(&connection, &excess.id).await.state, State::Prepared);
        assert_eq!(read(&connection, &plans[0].id).await.state, State::Queued);
        assert_eq!(intent_count(&connection).await, MAX_JOBS + 1);
        assert_eq!(inspect(&connection).await, snapshot);
    });
}

#[test]
fn expired_preparations_and_old_cancelled_history_are_pruned_but_queued_jobs_are_not() {
    pollster::block_on(async {
        let connection = fixture().await;
        let snapshot = inspect(&connection).await;
        let abandoned = prepare(&connection, snapshot.clone(), NOW_MS)
            .await
            .unwrap();
        let queued = prepare(&connection, snapshot.clone(), NOW_MS)
            .await
            .unwrap();
        run(&connection, confirmation(&queued), None, NOW_MS)
            .await
            .unwrap();
        let cancelled = prepare(&connection, snapshot.clone(), NOW_MS)
            .await
            .unwrap();
        run(
            &connection,
            Action::Cancel {
                id: cancelled.id.clone(),
            },
            None,
            NOW_MS,
        )
        .await
        .unwrap();
        let cutoff = NOW_MS + CANCEL_RETENTION_MS;
        prepare(&connection, snapshot, cutoff).await.unwrap();
        assert_eq!(intent_count(&connection).await, 2);
        assert_failure(
            run(&connection, Action::Read { id: abandoned.id }, None, cutoff).await,
            Failure::NotFound,
        );
        assert_failure(
            run(&connection, Action::Read { id: cancelled.id }, None, cutoff).await,
            Failure::NotFound,
        );
        assert_eq!(read(&connection, &queued.id).await.state, State::Queued);
    });
}

#[test]
fn nonce_debug_is_redacted_and_catalog_stores_only_a_digest() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, inspect(&connection).await, NOW_MS)
            .await
            .unwrap();
        let nonce = prepared.confirmation.as_ref().unwrap();
        assert!(!format!("{prepared:?}").contains(nonce.as_str()));
        assert!(!format!("{:?}", confirmation(&prepared)).contains(nonce.as_str()));
        assert_eq!(Nonce::parse(nonce.as_str()).unwrap(), *nonce);
        for invalid in ["", "short", &"z".repeat(64), &"0".repeat(65)] {
            assert_eq!(Nonce::parse(invalid), Err(Failure::Invalid));
        }
        let mut rows = connection
            .query("SELECT nonce_hash FROM recording_maintenance_intents", ())
            .await
            .unwrap();
        let digest: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(digest.len(), 64);
        assert_ne!(digest, nonce.as_str());
        assert!(read(&connection, &prepared.id).await.confirmation.is_none());
    });
}

#[test]
fn failed_confirmation_rolls_back_and_the_same_identity_can_be_retried() {
    pollster::block_on(async {
        let connection = fixture().await;
        let prepared = prepare(&connection, inspect(&connection).await, NOW_MS)
            .await
            .unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_intent_update AFTER UPDATE ON recording_maintenance_intents
             BEGIN SELECT RAISE(ABORT, 'injected intent failure'); END;",
            )
            .await
            .unwrap();
        assert!(
            run(&connection, confirmation(&prepared), None, NOW_MS)
                .await
                .is_err()
        );
        assert!(connection.is_autocommit().unwrap());
        assert_eq!(read(&connection, &prepared.id).await.state, State::Prepared);
        connection
            .execute_batch("DROP TRIGGER reject_intent_update")
            .await
            .unwrap();
        assert_eq!(
            run(&connection, confirmation(&prepared), None, NOW_MS)
                .await
                .unwrap()
                .state,
            State::Queued
        );
    });
}

#[test]
fn expired_queued_requests_do_not_start_transactions() {
    pollster::block_on(async {
        let connection = fixture().await;
        let request = Request {
            actor: "admin".to_owned(),
            action: Action::Read { id: "0".repeat(32) },
            snapshot: None,
            deadline: Instant::now(),
        };
        let error = execute_at(&connection, request, NOW_MS).await.unwrap_err();
        assert!(error.to_string().contains("deadline expired"));
        assert!(connection.is_autocommit().unwrap());
    });
}

async fn fixture() -> turso::Connection {
    let database = turso::Builder::new_local(":memory:").build().await.unwrap();
    let connection = database.connect().unwrap();
    initialize_schema(&connection).await.unwrap();
    connection.execute_batch(
        "INSERT INTO recording_files
         (id, stream_id, source_id, logical_stream_id, started_at_ms, ended_at_ms,
          path, init_offset, init_len, finalized, file_bytes)
         VALUES ('recording', 'front/sub', 'front', 'sub', 1000, 2000, 'synthetic.mp4', 0, 8, 1, 64);"
    ).await.unwrap();
    connection
}

async fn inspect(connection: &turso::Connection) -> Snapshot {
    super::super::snapshot(
        connection,
        Scope::Recording {
            source_id: "front".to_owned(),
            stream_id: "sub".to_owned(),
            recording_id: "recording".to_owned(),
        },
        Instant::now() + BUSY_TIMEOUT,
    )
    .await
    .unwrap()
}

async fn prepare(
    connection: &turso::Connection,
    snapshot: Snapshot,
    now_ms: i64,
) -> anyhow::Result<Job> {
    let action = Action::Prepare(Intent {
        scope: snapshot.scope.clone(),
        expected_revision: snapshot.revision,
        reason: Reason::Privacy,
    });
    run(connection, action, Some(snapshot), now_ms).await
}

async fn read(connection: &turso::Connection, id: &str) -> Job {
    run(connection, Action::Read { id: id.to_owned() }, None, NOW_MS)
        .await
        .unwrap()
}

fn confirmation(job: &Job) -> Action {
    Action::Confirm {
        id: job.id.clone(),
        nonce: job.confirmation.clone().unwrap(),
        expected_revision: job.revision,
    }
}

async fn run(
    connection: &turso::Connection,
    action: Action,
    snapshot: Option<Snapshot>,
    now_ms: i64,
) -> anyhow::Result<Job> {
    execute_at(connection, request(action, snapshot), now_ms).await
}

fn request(action: Action, snapshot: Option<Snapshot>) -> Request {
    Request {
        actor: "admin".to_owned(),
        action,
        snapshot,
        deadline: Instant::now() + BUSY_TIMEOUT,
    }
}

async fn intent_count(connection: &turso::Connection) -> i64 {
    let mut rows = connection
        .query("SELECT count(*) FROM recording_maintenance_intents", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn execute_at(
    connection: &turso::Connection,
    request: Request,
    now_ms: i64,
) -> anyhow::Result<Job> {
    execute_with_clock(connection, request, || {
        Ok(Moment {
            epoch: "test-catalog".to_owned(),
            utc_ms: now_ms,
            elapsed_ms: now_ms.saturating_sub(NOW_MS).max(0),
        })
    })
    .await
}

fn assert_failure(result: anyhow::Result<Job>, failure: Failure) {
    assert_eq!(
        result.unwrap_err().downcast_ref::<Failure>(),
        Some(&failure)
    );
}
