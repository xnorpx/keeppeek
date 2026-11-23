//! Bounded, administrator-confirmed deletion of redundant verified archive versions.

use super::*;
use crate::storage::archive::settings::ATTEMPTS_MAX;
use crate::storage::catalog::{current_unix_time_ms, encode_lower_hex, maintenance::jobs::Nonce};
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod eligibility;
mod execution;
const PAGE_MAX: u16 = 16;
const REVERIFICATION_BACKOFF_MS: i64 = 60_000;
const PLAN_LIFETIME_MS: i64 = 10 * 60 * 1000;
const HISTORY_MS: i64 = 30 * 86_400_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum Selection {
    Explicit(String),
    Retention { after: String, limit: u16 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Policy {
    pub target: String,
    pub days: Option<u32>,
    pub configuration_revision: String,
}

impl Policy {
    pub(crate) fn from_settings(
        settings: &crate::storage::archive::settings::Settings,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            target: settings.target_id.clone(),
            days: settings.retention_days,
            configuration_revision: encode_lower_hex(Sha256::digest(serde_json::to_vec(settings)?)),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Item {
    pub job_id: String,
    pub object_id: String,
    pub kind: String,
    pub bytes: u64,
    pub verified_ms: Option<i64>,
    pub state: String,
    version: Option<String>,
    source_identity: String,
    digest: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snapshot {
    policy: Policy,
    selection: Selection,
    items: Vec<Item>,
    next: String,
}

#[derive(Debug)]
pub(crate) struct Plan {
    pub target: String,
    pub id: String,
    pub revision: String,
    pub nonce: Option<Nonce>,
    pub expires_ms: i64,
    pub items: Vec<Item>,
    pub next: String,
    pub confirmed: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkItem {
    pub operation: String,
    pub phase: String,
    pub attempts: u32,
}

#[derive(Debug)]
pub(crate) enum Request {
    Preview {
        actor: String,
        policy: Policy,
        selection: Selection,
    },
    Confirm {
        actor: String,
        policy: Policy,
        plan: String,
        revision: String,
        nonce: Nonce,
    },
    ReplayConfirmation {
        actor: String,
        plan: String,
        revision: String,
        nonce: Nonce,
    },
    Claim {
        job: String,
        policy: Policy,
    },
    Retired(String),
    Acknowledged(String),
    Blocked(String),
    UnprotectedPrimary(String),
    Failed(String),
    EnsureNoActiveClaim(String),
    NextLegacy {
        target: String,
        maximum_attempts: u32,
        include_unrequested: bool,
    },
    LegacyFailed(String),
    Reverified {
        job: String,
        version: String,
    },
}

pub(super) async fn dispatch(
    connection: &turso::Connection,
    readers: &readers::Registry,
    request: Request,
) -> anyhow::Result<Reply> {
    match request {
        Request::Preview {
            actor,
            policy,
            selection,
        } => preview(connection, readers, actor, policy, selection).await,
        Request::Confirm {
            actor,
            policy,
            plan,
            revision,
            nonce,
        } => {
            confirm(
                connection,
                readers,
                actor,
                Some(policy),
                plan,
                revision,
                nonce,
            )
            .await
        }
        Request::ReplayConfirmation {
            actor,
            plan,
            revision,
            nonce,
        } => confirm(connection, readers, actor, None, plan, revision, nonce).await,
        Request::Claim { job, policy } => {
            execution::claim(connection, readers, &job, &policy).await
        }
        Request::Retired(id) => execution::retired(connection, &id).await,
        Request::Acknowledged(id) => execution::acknowledged(connection, &id).await,
        Request::Blocked(id) => execution::blocked(connection, &id, false).await,
        Request::UnprotectedPrimary(id) => execution::blocked(connection, &id, true).await,
        Request::Failed(id) => execution::failed(connection, &id).await,
        Request::EnsureNoActiveClaim(target) => {
            let mut rows=connection.query("SELECT 1 FROM archive_deletion_items d JOIN archive_replicas a ON a.job_id=d.job_id
                WHERE a.target_id=?1 AND d.phase IN('claimed','retired') LIMIT 1",[target]).await?;
            ensure!(
                rows.next().await?.is_none(),
                "confirmed archive retirement still requires this target"
            );
            Ok(Reply::Done)
        }
        Request::NextLegacy {
            target,
            maximum_attempts,
            include_unrequested,
        } => {
            ensure!(
                (1..=ATTEMPTS_MAX).contains(&maximum_attempts),
                "invalid archive reverification retry limit"
            );
            let mut rows = connection.query("SELECT a.job_id FROM archive_replicas a
                LEFT JOIN archive_reverification_retries r ON r.job_id=a.job_id
                WHERE a.target_id=?1 AND a.kind!='probe' AND a.state='ready' AND a.action='' AND a.cancelled=0
                AND NOT EXISTS(SELECT 1 FROM archive_verified_at v WHERE v.job_id=a.job_id AND v.version=a.version)
                AND NOT EXISTS(SELECT 1 FROM archive_deletion_items d WHERE d.job_id=a.job_id AND d.phase IN('queued','claimed','retired'))
                AND COALESCE(r.attempts,0)<?2 AND COALESCE(r.retry_after_ms,0)<=?3
                AND (?4=1 OR (r.attempts=0 AND r.retry_after_ms=0))
                ORDER BY CASE WHEN COALESCE(r.attempts,0)=0 THEN 0 ELSE 1 END,a.job_id LIMIT 1",
                turso::params![target, i64::from(maximum_attempts), current_unix_time_ms(), i64::from(include_unrequested)]).await?;
            let id = rows
                .next()
                .await?
                .map(|row| row.get::<String>(0))
                .transpose()?;
            drop(rows);
            Ok(Reply::Job(if let Some(id) = id {
                load(connection, &id).await?.map(Box::new)
            } else {
                None
            }))
        }
        Request::LegacyFailed(id) => {
            let now = current_unix_time_ms();
            connection
                .execute(
                    "INSERT INTO archive_reverification_retries(job_id,attempts,retry_after_ms)
                SELECT job_id,1,?2+?3 FROM archive_replicas WHERE job_id=?1 AND state='ready'
                    AND NOT EXISTS(SELECT 1 FROM archive_verified_at v WHERE v.job_id=?1)
                ON CONFLICT(job_id) DO UPDATE SET attempts=min(?4,attempts+1),
                    retry_after_ms=?2+?3*min(?4,attempts+1)",
                    (
                        id.as_str(),
                        now,
                        REVERIFICATION_BACKOFF_MS,
                        i64::from(ATTEMPTS_MAX),
                    ),
                )
                .await?;
            connection.execute("UPDATE archive_replicas SET error=?2 WHERE job_id=?1 AND state='ready' AND NOT EXISTS(SELECT 1 FROM archive_verified_at WHERE job_id=?1)",(id.as_str(), super::REVERIFICATION_FAILURE)).await?;
            Ok(Reply::Done)
        }
        Request::Reverified { job, version } => {
            execution::reverified(connection, &job, &version).await
        }
    }
}

async fn preview(
    connection: &turso::Connection,
    readers: &readers::Registry,
    actor: String,
    policy: Policy,
    selection: Selection,
) -> anyhow::Result<Reply> {
    ensure!(
        !actor.is_empty() && actor.len() <= 256,
        "invalid archive deletion actor"
    );
    ensure!(
        !policy.configuration_revision.is_empty() && policy.configuration_revision.len() <= 256,
        "invalid archive configuration revision"
    );
    let now = current_unix_time_ms();
    let (ids, next) = eligibility::select(connection, &policy, &selection).await?;
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        items.push(eligibility::inspect(connection, readers, &policy, &selection, &id, now).await?);
    }
    if !items.iter().any(|item| item.state == "eligible") {
        return Ok(Reply::Deletion(Box::new(Plan {
            target: policy.target,
            id: String::new(),
            revision: String::new(),
            nonce: None,
            expires_ms: 0,
            items,
            next,
            confirmed: false,
        })));
    }
    prune(connection, now).await?;
    let encoded = serde_json::to_string(&Snapshot {
        policy: policy.clone(),
        selection,
        items: items.clone(),
        next: next.clone(),
    })?;
    ensure!(
        encoded.len() <= 65_536,
        "archive deletion preview exceeds its bound"
    );
    let revision = encode_lower_hex(Sha256::digest(encoded.as_bytes()));
    let id = uuid::Uuid::new_v4().to_string();
    let nonce = Nonce::generate();
    let expires_ms = now
        .checked_add(PLAN_LIFETIME_MS)
        .context("archive deletion clock overflow")?;
    connection.execute("INSERT INTO archive_deletion_plans(id,actor,target_id,revision,nonce_hash,expires_ms,snapshot) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        turso::params![id.as_str(), actor, policy.target.as_str(), revision.as_str(), nonce.digest(), expires_ms, encoded]).await?;
    Ok(Reply::Deletion(Box::new(Plan {
        target: policy.target,
        id,
        revision,
        nonce: Some(nonce),
        expires_ms,
        items,
        next,
        confirmed: false,
    })))
}

struct Stored {
    snapshot: Snapshot,
    revision: String,
    nonce_hash: String,
    expires_ms: i64,
    confirmed: bool,
}

async fn stored(connection: &turso::Connection, actor: &str, id: &str) -> anyhow::Result<Stored> {
    let mut rows = connection.query("SELECT snapshot,revision,nonce_hash,expires_ms,confirmed_ms IS NOT NULL FROM archive_deletion_plans WHERE id=?1 AND actor=?2", (id,actor)).await?;
    let row = rows
        .next()
        .await?
        .context("archive deletion preview unavailable")?;
    Ok(Stored {
        snapshot: serde_json::from_str(&row.get::<String>(0)?)?,
        revision: row.get(1)?,
        nonce_hash: row.get(2)?,
        expires_ms: row.get(3)?,
        confirmed: row.get::<i64>(4)? != 0,
    })
}

async fn confirm(
    connection: &turso::Connection,
    readers: &readers::Registry,
    actor: String,
    policy: Option<Policy>,
    id: String,
    revision: String,
    nonce: Nonce,
) -> anyhow::Result<Reply> {
    let record = stored(connection, &actor, &id).await?;
    ensure!(
        revision == record.revision && nonce.digest() == record.nonce_hash,
        "archive deletion confirmation does not match preview"
    );
    let mut snapshot = record.snapshot;
    let now = current_unix_time_ms();
    if !record.confirmed {
        let Some(policy) = policy else {
            return Ok(Reply::Done);
        };
        ensure!(
            (record.expires_ms.saturating_sub(PLAN_LIFETIME_MS)..record.expires_ms).contains(&now)
                && snapshot.policy == policy,
            "archive deletion preview expired or settings changed"
        );
        ensure!(
            snapshot.items.iter().any(|item| item.state == "eligible"),
            "archive deletion preview has no eligible replicas"
        );
        for item in snapshot
            .items
            .iter()
            .filter(|item| item.state == "eligible")
        {
            let current = eligibility::inspect(
                connection,
                readers,
                &policy,
                &snapshot.selection,
                &item.job_id,
                now,
            )
            .await?;
            ensure!(
                current == *item,
                "archive deletion selection changed; preview again"
            );
            connection.execute("INSERT INTO archive_deletion_items(job_id,plan_id,operation,phase) VALUES(?1,?2,?3,'queued')
                ON CONFLICT(job_id) DO UPDATE SET plan_id=excluded.plan_id, operation=excluded.operation,phase='queued',attempts=0 WHERE phase='blocked'",
                turso::params![item.job_id.as_str(), id.as_str(), uuid::Uuid::new_v4().to_string()]).await?;
            connection
                .execute(
                    "UPDATE archive_replicas SET error=NULL WHERE job_id=?1",
                    [item.job_id.as_str()],
                )
                .await?;
        }
        connection
            .execute(
                "UPDATE archive_deletion_plans SET confirmed_ms=?2 WHERE id=?1",
                (id.as_str(), now),
            )
            .await?;
    }
    for item in snapshot
        .items
        .iter_mut()
        .filter(|item| item.state == "eligible")
    {
        let mut rows = connection
            .query(
                "SELECT phase FROM archive_deletion_items WHERE job_id=?1 AND plan_id=?2",
                (item.job_id.as_str(), id.as_str()),
            )
            .await?;
        item.state = if let Some(row) = rows.next().await? {
            row.get(0)?
        } else {
            "blocked".into()
        };
    }
    Ok(Reply::Deletion(Box::new(Plan {
        target: snapshot.policy.target,
        id,
        revision,
        nonce: None,
        expires_ms: record.expires_ms,
        items: snapshot.items,
        next: snapshot.next,
        confirmed: true,
    })))
}

async fn prune(connection: &turso::Connection, now: i64) -> anyhow::Result<()> {
    let before = now.saturating_sub(HISTORY_MS);
    // At most 32 plans and 512 cascade-owned item rows are removed in one writer transaction.
    connection.execute("DELETE FROM archive_deletion_plans WHERE id IN (
        SELECT p.id FROM archive_deletion_plans p WHERE (p.confirmed_ms IS NULL AND p.expires_ms<=?1)
        OR (p.confirmed_ms<?2 AND NOT EXISTS(SELECT 1 FROM archive_deletion_items d WHERE d.plan_id=p.id
            AND d.phase NOT IN('acknowledged','blocked'))) ORDER BY p.confirmed_ms IS NOT NULL, p.expires_ms, p.id LIMIT 32)",(now,before)).await?;
    let mut rows = connection.query("SELECT count(*),count(*) FILTER(WHERE confirmed_ms IS NULL) FROM archive_deletion_plans",()).await?;
    let count = rows
        .next()
        .await?
        .context("archive deletion plan count unavailable")?;
    ensure!(
        count.get::<i64>(0)? < 4096 && count.get::<i64>(1)? < 128,
        "archive deletion preview limit reached"
    );
    Ok(())
}
