use super::*;
use crate::storage::catalog::{
    archive::deletion::{Plan, Policy, Request as Deletion, Selection},
    current_unix_time_ms,
};

fn policy(days: Option<u32>) -> Policy {
    Policy {
        target: "cold".into(),
        days,
        configuration_revision: "config-v1".into(),
    }
}

async fn run(connection: &turso::Connection, request: Deletion) -> anyhow::Result<Reply> {
    execute(
        connection,
        &Arc::new(readers::Registry::default()),
        Request::Deletion(request),
        Instant::now() + std::time::Duration::from_secs(5),
    )
    .await
}

async fn page(connection: &turso::Connection, days: Option<u32>) -> anyhow::Result<Plan> {
    let r = run(
        connection,
        Deletion::Preview {
            actor: "admin".into(),
            policy: policy(days),
            selection: Selection::Retention {
                after: String::new(),
                limit: 16,
            },
        },
    )
    .await?;
    let Reply::Deletion(plan) = r else {
        anyhow::bail!("unexpected archive preview reply")
    };
    Ok(*plan)
}

async fn confirm(
    connection: &turso::Connection,
    plan: &Plan,
    policy: Policy,
) -> anyhow::Result<Reply> {
    run(
        connection,
        Deletion::Confirm {
            actor: "admin".into(),
            policy,
            plan: plan.id.clone(),
            revision: plan.revision.clone(),
            nonce: plan.nonce.clone().unwrap(),
        },
    )
    .await
}

async fn explicit_plan(connection: &turso::Connection) -> anyhow::Result<Plan> {
    let Reply::Deletion(plan) = run(
        connection,
        Deletion::Preview {
            actor: "admin".into(),
            policy: policy(None),
            selection: Selection::Explicit(admission().job_id),
        },
    )
    .await?
    else {
        anyhow::bail!("expected explicit deletion preview")
    };
    Ok(*plan)
}

#[test]
fn ineligible_explicit_previews_preserve_the_global_plan_quota() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        ready_for_eviction(&connection).await?;
        for _ in 0..=128 {
            let preview = explicit_plan(&connection).await?;
            assert_eq!(preview.items[0].state, "busy");
            assert!(
                preview.id.is_empty() && preview.revision.is_empty() && preview.nonce.is_none()
            );
        }
        let mut rows = connection
            .query("SELECT COUNT(*) FROM archive_deletion_plans", ())
            .await?;
        assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 0);
        drop(rows);
        connection
            .execute("UPDATE archive_replicas SET action=''", ())
            .await?;
        let eligible = explicit_plan(&connection).await?;
        assert_eq!(eligible.items[0].state, "eligible");
        assert!(eligible.nonce.is_some());
        Ok(())
    })
}

#[test]
fn confirmed_replay_checks_the_receipt_and_never_admits_a_new_batch() -> anyhow::Result<()> {
    use crate::storage::catalog::maintenance::jobs::Nonce;
    pollster::block_on(async {
        let connection = fixture().await?;
        ready_for_eviction(&connection).await?;
        connection
            .execute("UPDATE archive_replicas SET action=''", ())
            .await?;
        let plan = explicit_plan(&connection).await?;
        let nonce = plan.nonce.clone().unwrap();
        let replay = |actor: &str, revision: &str, nonce: Nonce| Deletion::ReplayConfirmation {
            actor: actor.into(),
            plan: plan.id.clone(),
            revision: revision.into(),
            nonce,
        };
        assert!(matches!(
            run(&connection, replay("admin", &plan.revision, nonce.clone())).await?,
            Reply::Done
        ));
        confirm(&connection, &plan, policy(None)).await?;
        let Reply::Deletion(receipt) =
            run(&connection, replay("admin", &plan.revision, nonce.clone())).await?
        else {
            anyhow::bail!("confirmed replay must return its receipt")
        };
        assert!(receipt.confirmed && receipt.nonce.is_none());
        assert_eq!(receipt.target, "cold");
        assert_eq!(receipt.items[0].state, "queued");
        for invalid in [
            replay("another-admin", &plan.revision, nonce.clone()),
            replay("admin", "another-revision", nonce),
            replay("admin", &plan.revision, Nonce::generate()),
        ] {
            assert!(run(&connection, invalid).await.is_err());
        }
        Ok(())
    })
}

#[test]
fn archive_retention_age_starts_at_durable_verification_and_each_batch_requires_confirmation()
-> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        ready_for_eviction(&connection).await?;
        connection
            .execute("UPDATE archive_replicas SET action=''", ())
            .await?;
        assert!(page(&connection, None).await.is_err());
        let not_due = page(&connection, Some(2)).await?;
        assert_eq!(not_due.items[0].state, "retained");
        assert!(not_due.nonce.is_none());
        connection
            .execute(
                "UPDATE archive_verified_at SET verified_ms=?1",
                [current_unix_time_ms() - 2 * 86_400_000],
            )
            .await?;
        let due = page(&connection, Some(2)).await?;
        assert_eq!(due.items[0].state, "eligible");
        assert!(confirm(&connection, &due, policy(Some(10))).await.is_err());
        connection
            .execute("UPDATE recording_files SET protected=1", ())
            .await?;
        assert!(confirm(&connection, &due, policy(Some(2))).await.is_err());
        assert_eq!(
            page(&connection, Some(2)).await?.items[0].state,
            "preservation_hold"
        );
        connection
            .execute("UPDATE recording_files SET protected=0", ())
            .await?;
        connection
            .execute(
                "UPDATE archive_deletion_plans SET expires_ms=1 WHERE id=?1",
                [due.id.as_str()],
            )
            .await?;
        assert!(confirm(&connection, &due, policy(Some(2))).await.is_err());
        let active = page(&connection, Some(2)).await?;
        confirm(&connection, &active, policy(Some(2))).await?;
        let id = admission().job_id;
        assert_eq!(required(&connection, &id).await?.state, "ready");
        connection
            .execute(
                "UPDATE recording_files SET source_id='camera' WHERE id='recording'",
                (),
            )
            .await?;
        connection.execute("INSERT INTO recording_retention_camera_work(camera_id,lower_ms,upper_ms,generation)
            SELECT source_id,0,1000,0 FROM recording_files WHERE id='recording'", ()).await?;
        assert!(
            run(
                &connection,
                Deletion::Claim {
                    job: id.clone(),
                    policy: policy(Some(2))
                }
            )
            .await
            .is_err()
        );
        run(&connection, Deletion::Blocked(id.clone())).await?;
        assert!(required(&connection, &id).await?.error.is_some());
        assert_eq!(
            page(&connection, Some(2)).await?.items[0].state,
            "preservation_hold"
        );
        connection
            .execute("DELETE FROM recording_retention_camera_work", ())
            .await?;
        let new_plan = page(&connection, Some(2)).await?;
        confirm(&connection, &new_plan, policy(Some(2))).await?;
        assert!(required(&connection, &id).await?.error.is_none());
        run(
            &connection,
            Deletion::Claim {
                job: id.clone(),
                policy: policy(Some(2)),
            },
        )
        .await?;
        assert_eq!(required(&connection, &id).await?.state, "deleting");
        assert!(
            connection
                .execute("UPDATE recording_files SET protected=1", ())
                .await
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE storage_volume_allocations SET state='cancelled'",
                    ()
                )
                .await
                .is_err()
        );
        run(
            &connection,
            Deletion::Claim {
                job: id.clone(),
                policy: policy(Some(2)),
            },
        )
        .await?;
        assert!(
            run(&connection, Deletion::EnsureNoActiveClaim("cold".into()))
                .await
                .is_err()
        );
        connection.execute_batch("INSERT INTO recording_files(id,stream_id,source_id,started_at_ms,ended_at_ms,path,init_offset,init_len,finalized,finalized_at_ms,file_identity,file_bytes)
            SELECT 'z-other',stream_id,'other-camera',1,2,'/hot/z-other.mp4',0,1,1,2,'other-file',4 FROM recording_files WHERE id='recording';
            UPDATE recording_retention_runtime SET complete=0,cursor=NULL,high_water='z-other';").await?;
        use crate::storage::catalog::retention::runtime::test_reconciliation_turn;
        assert_eq!(test_reconciliation_turn(&connection).await?.evaluated, 0);
        assert_eq!(test_reconciliation_turn(&connection).await?.evaluated, 1);
        test_reconciliation_turn(&connection).await?;
        let mut rows = connection
            .query(
                "SELECT recording_id FROM archive_deletion_deferred_retention",
                (),
            )
            .await?;
        assert_eq!(rows.next().await?.unwrap().get::<String>(0)?, "recording");
        drop(rows);
        // A single archive-only claim must not hold back a new recording policy for the fleet.
        connection
            .execute(
                "UPDATE recording_retention_runtime SET request_pending=1,requested_json=?1",
                [r#"{"default":{"continuous_days":2}}"#],
            )
            .await?;
        for _ in 0..12 {
            test_reconciliation_turn(&connection).await?;
        }
        let mut activation = connection.query("SELECT generation,request_pending,complete,
                (SELECT retention_generation FROM recording_files WHERE id='z-other'),
                (SELECT generation FROM archive_deletion_deferred_retention WHERE recording_id='recording')
                FROM recording_retention_runtime WHERE singleton=1", ()).await?;
        let row = activation.next().await?.unwrap();
        let generation = row.get::<i64>(0)?;
        assert!(generation > 0);
        assert_eq!((row.get::<i64>(1)?, row.get::<i64>(2)?), (0, 1));
        assert_eq!(
            (row.get::<i64>(3)?, row.get::<i64>(4)?),
            (generation, generation)
        );
        drop(activation);
        run(&connection, Deletion::Retired(id.clone())).await?;
        run(&connection, Deletion::Acknowledged(id.clone())).await?;
        run(&connection, Deletion::EnsureNoActiveClaim("cold".into())).await?;
        assert_eq!(test_reconciliation_turn(&connection).await?.evaluated, 1);
        let mut deferred = connection
            .query("SELECT 1 FROM archive_deletion_deferred_retention", ())
            .await?;
        assert!(deferred.next().await?.is_none());
        drop(deferred);
        assert_eq!(required(&connection, &id).await?.state, "deleted");
        check_retired_copy_selection(&connection).await?;
        connection
            .execute("UPDATE recording_files SET protected=1", ())
            .await?;
        Ok(())
    })
}

async fn check_retired_copy_selection(connection: &turso::Connection) -> anyhow::Result<()> {
    use crate::storage::catalog::locations::{self, Kind, Object};
    let next_id = "z-eligible";
    connection.execute_batch(
        "INSERT INTO recording_files(id,stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,
             finalized,finalized_at_ms,file_identity,file_bytes)
         SELECT 'z-eligible',stream_id,1,2,'/hot/cccccccc-cccc-4ccc-8ccc-cccccccccccc.mp4',0,1,1,2,'later-file',4 FROM recording_files WHERE id='recording';
         INSERT INTO storage_volume_allocations(operation,kind,object_id,volume_id,generation,
             relative_key,destination_path,bytes,intent_bytes,materialized_bytes,state,file_identity,digest,location_revision)
         SELECT 'later-source',kind,'z-eligible',volume_id,generation,'cccccccc-cccc-4ccc-8ccc-cccccccccccc.mp4','/hot/cccccccc-cccc-4ccc-8ccc-cccccccccccc.mp4',4,4,4,'published','later-file',zeroblob(32),1
         FROM storage_volume_allocations WHERE operation='source-operation';"
    ).await?;
    let query = |filtered, after: Option<&str>| {
        let request = locations::Request::Objects(locations::objects::Page {
            volume: "hot".into(),
            after: after.map(|id| Object {
                kind: Kind::Recording,
                id: id.to_owned(),
            }),
            limit: 1,
            archive_copy_candidates_only: filtered,
        });
        async move {
            let locations::Reply::Objects(rows) = locations::execute(
                connection,
                request,
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .await?
            else {
                anyhow::bail!("unexpected object page");
            };
            Ok::<_, anyhow::Error>(
                rows.into_iter()
                    .map(|row| row.object.id)
                    .collect::<Vec<_>>(),
            )
        }
    };
    assert_eq!(query(false, None).await?, ["recording"]);
    assert_eq!(query(false, Some("recording")).await?, [next_id]);
    assert_eq!(query(true, None).await?, [next_id]);
    assert!(query(true, Some(next_id)).await?.is_empty());
    let Reply::Candidates(automatic) = candidates(connection, "", i64::MAX, 1).await? else {
        anyhow::bail!("unexpected automatic candidate page");
    };
    assert_eq!(automatic.len(), 1);
    assert_eq!(automatic[0].0, next_id);
    let manual = Admission {
        job_id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc".into(),
        ..admission()
    };
    assert!(enqueue(connection, &manual).await.is_err());
    assert_eq!(
        required(connection, &admission().job_id).await?.state,
        "deleted"
    );
    Ok(())
}

#[test]
fn archive_retention_keeps_legacy_ready_replicas_until_a_new_verified_observation()
-> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        ready_for_eviction(&connection).await?;
        connection
            .execute_batch(
                "UPDATE archive_replicas SET action=''; DELETE FROM archive_verified_at;",
            )
            .await?;
        assert_eq!(
            page(&connection, Some(1)).await?.items[0].state,
            "verification_date_unknown"
        );
        let id = admission().job_id;
        assert!(
            run(
                &connection,
                Deletion::Reverified {
                    job: id.clone(),
                    version: "wrong".into()
                }
            )
            .await
            .is_err()
        );
        run(
            &connection,
            Deletion::Reverified {
                job: id,
                version: "archive-version".into(),
            },
        )
        .await?;
        let Reply::Job(None) = run(
            &connection,
            Deletion::NextLegacy {
                target: "cold".into(),
                include_unrequested: true,
                maximum_attempts: 1,
            },
        )
        .await?
        else {
            anyhow::bail!("verified job was offered for reverification")
        };
        let observed = page(&connection, Some(1)).await?;
        assert!(observed.items[0].verified_ms.is_some());
        assert_eq!(observed.items[0].state, "retained");
        Ok(())
    })
}

#[test]
fn archive_retention_prunes_expired_previews_before_old_confirmations() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        ready_for_eviction(&connection).await?;
        connection.execute_batch("UPDATE archive_replicas SET action=''; UPDATE archive_verified_at SET verified_ms=1;").await?;
        for i in 0..160 {
            let confirmed = (i < 32).then_some(1_i64);
            let expires = if confirmed.is_some() { 2 } else { 3 };
            connection.execute("INSERT INTO archive_deletion_plans(id,actor,target_id,revision,nonce_hash,expires_ms,confirmed_ms,snapshot)
                VALUES(?1,'admin','cold','revision',?2,?3,?4,'{}')", turso::params![i.to_string(),"a".repeat(64),expires,confirmed]).await?;
        }
        let page = page(&connection, Some(1)).await?;
        assert!(page.nonce.is_some());
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM archive_deletion_plans WHERE confirmed_ms IS NULL",
                (),
            )
            .await?;
        assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 97);
        Ok(())
    })
}

#[test]
fn archive_retention_backoff_retries_are_persisted_and_administrator_can_reset_them()
-> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        ready_for_eviction(&connection).await?;
        connection
            .execute_batch(
                "UPDATE archive_replicas SET action=''; DELETE FROM archive_verified_at;",
            )
            .await?;
        let id = admission().job_id;
        let next = |include_unrequested| Deletion::NextLegacy {
            target: "cold".into(),
            include_unrequested,
            maximum_attempts: 1,
        };
        assert!(matches!(
            run(&connection, next(true)).await?,
            Reply::Job(Some(_))
        ));
        assert!(matches!(
            run(&connection, next(false)).await?,
            Reply::Job(None)
        ));
        connection.execute("UPDATE archive_replicas SET error='Archive deletion blocked; check the primary and preview again' WHERE job_id=?1",[id.as_str()]).await?;
        let blocked = required(&connection, &id).await?;
        assert_eq!(blocked.error_code(), "archive_deletion_blocked");
        assert!(!blocked.reverification_failed());
        assert!(retry(&connection, &id).await.is_err());
        run(&connection, Deletion::LegacyFailed(id.clone())).await?;
        assert!(matches!(run(&connection, next(true)).await?, Reply::Job(None)));
        let failed = required(&connection, &id).await?;
        assert!(failed.reverification_failed());
        assert_eq!(failed.error_code(), "archive_reverification_failed");
        connection
            .execute(
                "UPDATE archive_reverification_retries SET retry_after_ms=0",
                (),
            )
            .await?;
        assert!(matches!(
            run(&connection, next(true)).await?,
            Reply::Job(None)
        ));
        super::super::retry(&connection, &id).await?;
        assert!(matches!(
            run(&connection, next(false)).await?,
            Reply::Job(Some(_))
        ));
        Ok(())
    })
}
