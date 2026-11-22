//! Re-evaluates the catalog conditions that keep one independently verified primary copy available.
use super::*;

pub(super) async fn select(
    connection: &turso::Connection,
    policy: &Policy,
    selection: &Selection,
) -> anyhow::Result<(Vec<String>, String)> {
    let (after, limit) = match selection {
        Selection::Explicit(id) => {
            crate::storage::archive::target::ObjectId::parse(id)?;
            return Ok((vec![id.clone()], String::new()));
        }
        Selection::Retention { after, limit } => (after, limit),
    };
    ensure!(
        policy.days.is_some() && (1..=PAGE_MAX).contains(limit),
        "archive retention page unavailable"
    );
    if !after.is_empty() {
        crate::storage::archive::target::ObjectId::parse(after)?;
    }
    let mut rows = connection.query("SELECT job_id FROM archive_replicas WHERE job_id>?1 AND target_id=?2 AND kind IN('recording','export') AND state='ready' ORDER BY job_id LIMIT ?3",
        turso::params![after.as_str(), policy.target.as_str(), i64::from(*limit)+1]).await?;
    let mut ids = Vec::with_capacity(usize::from(*limit) + 1);
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    let next = if ids.len() > usize::from(*limit) {
        ids.pop();
        ids.last().cloned().unwrap_or_default()
    } else {
        String::new()
    };
    Ok((ids, next))
}

pub(super) async fn inspect(
    connection: &turso::Connection,
    readers: &readers::Registry,
    policy: &Policy,
    selection: &Selection,
    id: &str,
    now: i64,
) -> anyhow::Result<Item> {
    let job = load_internal(connection, id, false)
        .await?
        .context("archive replica unavailable")?;
    ensure!(
        job.binding.id == policy.target && matches!(job.kind.as_str(), "recording" | "export"),
        "archive replica unavailable"
    );
    let verified_ms = job.verified_ms;
    let state = if job.state != "ready"
        || !job.action.is_empty()
        || job.cancelled
        || job.deletion.as_ref().is_some_and(|d| d.phase != "blocked")
    {
        "busy"
    } else if job.local_evicted != 0 {
        "restore_required"
    } else if matches!(selection, Selection::Retention { .. }) && verified_ms.is_none() {
        "verification_date_unknown"
    } else if matches!(selection, Selection::Retention { .. })
        && verified_ms.is_none_or(|ms| {
            policy
                .days
                .is_none_or(|days| now.saturating_sub(ms) < i64::from(days) * 86_400_000)
        })
    {
        "retained"
    } else {
        guards(connection, readers, &job, false).await?
    };
    Ok(Item {
        job_id: job.id,
        object_id: job.recording_id,
        kind: job.kind,
        bytes: job.bytes,
        verified_ms,
        state: state.into(),
        version: job.version,
        source_identity: job.source_identity,
        digest: job.digest,
    })
}

pub(super) async fn guards(
    connection: &turso::Connection,
    readers: &readers::Registry,
    job: &Job,
    recovering_claim: bool,
) -> anyhow::Result<&'static str> {
    if validate_source_location(connection, job, "published")
        .await
        .is_err()
    {
        return Ok("primary_unavailable");
    }
    let archive_path = job.binding.root.join(format!("{}.mp4", job.object_id));
    if readers.conflicts(&job.recording_id, &job.source_path)?
        || readers.conflicts(
            &format!("archive-export:{}", job.recording_id),
            &archive_path.to_string_lossy(),
        )?
    {
        return Ok("active_reader");
    }
    if job.kind == "recording" {
        let mut rows = connection
            .query(
                "SELECT protected,retention_pending,retention_error IS NOT NULL FROM recording_files WHERE id=?1",
                [job.recording_id.as_str()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok("primary_unavailable");
        };
        if row.get::<i64>(0)? != 0 || row.get::<i64>(1)? != 0 || row.get::<i64>(2)? != 0 {
            return Ok("preservation_hold");
        }
        drop(rows);
        if ensure_source_active(connection, &job.recording_id)
            .await
            .is_err()
            || (!recovering_claim
                && crate::storage::catalog::retention::runtime::ensure_admission_ready(connection)
                    .await
                    .is_err())
            || crate::storage::catalog::retention::ensure_cleanup_allowed(
                connection,
                &job.recording_id,
            )
            .await
            .is_err()
        {
            return Ok("preservation_hold");
        }
    }
    Ok("eligible")
}
