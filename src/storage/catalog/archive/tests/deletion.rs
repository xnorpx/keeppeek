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
        assert!(
            confirm(&connection, &not_due, policy(Some(2)))
                .await
                .is_err()
        );
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
        run(&connection, Deletion::Retired(id.clone())).await?;
        run(&connection, Deletion::Acknowledged(id.clone())).await?;
        assert_eq!(required(&connection, &id).await?.state, "deleted");
        connection
            .execute("UPDATE recording_files SET protected=1", ())
            .await?;
        Ok(())
    })
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
        let observed = page(&connection, Some(1)).await?;
        assert!(observed.items[0].verified_ms.is_some());
        assert_eq!(observed.items[0].state, "retained");
        Ok(())
    })
}
