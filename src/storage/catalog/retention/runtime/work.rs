use super::{Progress, SETTINGS_BYTES_MAX, Settings};
use crate::storage::retention::Policy;

mod events;
use anyhow::{Context, Result, ensure};

struct State {
    generation: i64,
    requested: Option<String>,
    request_pending: bool,
    complete: bool,
    cursor: Option<String>,
    high_water: Option<String>,
    spans_ready: bool,
    work_clock: i64,
}

async fn state(connection: &turso::Connection) -> Result<State> {
    let mut rows = connection.query("SELECT generation,settings_json IS NOT NULL,
        substr(requested_json,1,524289),request_pending,complete,substr(cursor,1,257),substr(high_water,1,257)
        ,spans_ready,work_clock FROM recording_retention_runtime WHERE singleton=1", ()).await?;
    let row = rows
        .next()
        .await?
        .context("retention runtime state missing")?;
    let state = State {
        generation: row.get(0)?,
        requested: row.get(2)?,
        request_pending: row.get::<i64>(3)? != 0,
        complete: row.get::<i64>(4)? != 0,
        cursor: row.get(5)?,
        high_water: row.get(6)?,
        spans_ready: row.get::<i64>(7)? != 0,
        work_clock: row.get(8)?,
    };
    ensure!(
        state
            .requested
            .as_ref()
            .is_none_or(|json| json.len() <= SETTINGS_BYTES_MAX),
        "retention settings metadata limit exceeded"
    );
    for id in [state.cursor.as_deref(), state.high_water.as_deref()]
        .into_iter()
        .flatten()
    {
        super::super::validate_identity(id)?;
    }
    Ok(state)
}

pub(super) async fn step(connection: &turso::Connection) -> Result<Progress> {
    let state = state(connection).await?;
    if requires_events(connection).await?
        && !super::super::event_index::reconcile(connection, 16).await?
    {
        return Ok(Progress {
            pending: true,
            ..Progress::default()
        });
    }
    if !state.complete {
        if state.spans_ready
            && state.work_clock % 2 == 1
            && let Some(progress) = events::step(connection, state.generation).await?
        {
            return Ok(progress);
        }
        return backfill(connection, &state).await;
    }
    if let Some(id) = pending_file(connection, state.generation).await? {
        let mut progress = evaluate(connection, &id, state.generation).await?;
        progress.pending = true;
        return Ok(progress);
    }
    if let Some(progress) = events::step(connection, state.generation).await? {
        return Ok(progress);
    }
    if state.request_pending {
        if claims_pending(connection).await? {
            return Ok(Progress {
                pending: true,
                ..Progress::default()
            });
        }
        activate(connection, &state).await?;
        return Ok(Progress {
            pending: true,
            ..Progress::default()
        });
    }
    Ok(Progress::default())
}

async fn requires_events(connection: &turso::Connection) -> Result<bool> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM recording_retention_policies WHERE uses_events=1 LIMIT 1",
            (),
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

async fn pending_file(connection: &turso::Connection, generation: i64) -> Result<Option<String>> {
    let mut rows = connection
        .query(
            "SELECT id FROM recording_files INDEXED BY recording_retention_pending_generation
            WHERE retention_pending=1 AND retention_generation<=?1 AND cleanup_pending=0
                AND NOT EXISTS(SELECT 1 FROM storage_recording_retirements WHERE recording_id=recording_files.id AND complete=0)
            ORDER BY retention_generation,id LIMIT 1",
            [generation],
        )
        .await?;
    rows.next()
        .await?
        .map(|row| row.get(0))
        .transpose()
        .map_err(Into::into)
}

async fn claims_pending(connection: &turso::Connection) -> Result<bool> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM recording_files WHERE cleanup_pending=1 LIMIT 1",
            (),
        )
        .await?;
    if rows.next().await?.is_some() {
        return Ok(true);
    }
    let mut rows = connection
        .query(
            "SELECT 1 FROM storage_recording_retirements WHERE complete=0 LIMIT 1",
            (),
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

async fn activate(connection: &turso::Connection, state: &State) -> Result<()> {
    let settings = state
        .requested
        .as_deref()
        .map(serde_json::from_str::<Settings>)
        .transpose()?;
    if let Some(settings) = &settings {
        settings.validate()?;
    }
    connection
        .execute("DELETE FROM recording_retention_policies", ())
        .await?;
    if let Some(settings) = &settings {
        if settings.default.is_some() {
            install_policy(
                connection,
                "",
                settings
                    .policy_for("")?
                    .as_ref()
                    .context("missing retention default")?,
            )
            .await?;
        }
        for camera in settings.cameras.keys() {
            install_policy(
                connection,
                camera,
                settings
                    .policy_for(camera)?
                    .as_ref()
                    .context("missing camera policy")?,
            )
            .await?;
        }
    }
    super::schema::sync_event_triggers(connection).await?;
    let generation = state
        .generation
        .checked_add(1)
        .context("retention generation exhausted")?;
    connection.execute("UPDATE recording_retention_runtime SET generation=?1,settings_json=requested_json,
        request_pending=0,cursor=NULL,high_water=(SELECT id FROM recording_files ORDER BY id DESC LIMIT 1),
        complete=?2,expiry_cursor_ms=-9223372036854775808,expiry_cursor_id='' WHERE singleton=1",turso::params![generation,i64::from(settings.is_none())]).await?;
    Ok(())
}

async fn install_policy(
    connection: &turso::Connection,
    camera: &str,
    policy: &Policy,
) -> Result<()> {
    let json = serde_json::to_string(policy)?;
    ensure!(
        json.len() <= 8192,
        "retention policy metadata limit exceeded"
    );
    let uses_events = policy.requires_event_evidence();
    connection
        .execute(
            "INSERT INTO recording_retention_policies VALUES(?1,?2,?3)",
            turso::params![camera, json, i64::from(uses_events)],
        )
        .await?;
    Ok(())
}

async fn backfill(connection: &turso::Connection, state: &State) -> Result<Progress> {
    let mut rows = connection
        .query(
            "SELECT id,finalized,cleanup_pending FROM recording_files
        WHERE id>?1 AND id<=?2 ORDER BY id LIMIT 1",
            turso::params![
                state.cursor.as_deref().unwrap_or(""),
                state.high_water.as_deref().unwrap_or("")
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        drop(rows);
        connection
            .execute(
                "UPDATE recording_retention_runtime SET spans_ready=1,work_clock=work_clock+1,
                    cursor=CASE WHEN overflow_next=1 THEN NULL ELSE cursor END,
                    high_water=CASE WHEN overflow_next=1 THEN (SELECT id FROM recording_files ORDER BY id DESC LIMIT 1) ELSE high_water END,
                    complete=CASE WHEN overflow_next=1 THEN 0 ELSE 1 END,overflow=overflow_next,overflow_next=0 WHERE singleton=1",
                (),
            )
            .await?;
        return Ok(Progress {
            pending: true,
            ..Progress::default()
        });
    };
    let id: String = row.get(0)?;
    super::super::validate_identity(&id)?;
    let finalized = row.get::<i64>(1)? != 0;
    let claimed = row.get::<i64>(2)? != 0;
    drop(rows);
    if finalized {
        record_span(connection, &id).await?;
    }
    let mut progress = if finalized && !claimed {
        evaluate(connection, &id, state.generation).await?
    } else {
        Progress::default()
    };
    connection
        .execute(
            "UPDATE recording_retention_runtime SET cursor=?1,work_clock=work_clock+1 WHERE singleton=1",
            [id],
        )
        .await?;
    progress.pending = true;
    Ok(progress)
}

async fn policy_for(connection: &turso::Connection, id: &str) -> Result<Option<Policy>> {
    let mut rows = connection
        .query(
            "SELECT substr(p.policy_json,1,8193) FROM recording_files r
        JOIN recording_retention_policies p ON p.camera_id=COALESCE(r.source_id,'') WHERE r.id=?1",
            [id],
        )
        .await?;
    let json = rows
        .next()
        .await?
        .map(|row| row.get::<String>(0))
        .transpose()?;
    drop(rows);
    let json = if json.is_some() {
        json
    } else {
        let mut rows=connection.query("SELECT substr(policy_json,1,8193) FROM recording_retention_policies WHERE camera_id=''",()).await?;
        rows.next()
            .await?
            .map(|row| row.get::<String>(0))
            .transpose()?
    };
    json.map(|json| {
        ensure!(
            json.len() <= 8192,
            "retention policy metadata limit exceeded"
        );
        Ok(serde_json::from_str(&json)?)
    })
    .transpose()
}

async fn evaluate(connection: &turso::Connection, id: &str, generation: i64) -> Result<Progress> {
    let Some(policy) = policy_for(connection, id).await? else {
        finish_file(connection, id, generation, None).await?;
        return Ok(Progress {
            evaluated: 1,
            ..Progress::default()
        });
    };
    let result = commit_file(connection, id, &policy).await;
    match result {
        Ok(()) => {
            connection.execute("UPDATE recording_retention_decisions SET runtime_generation=?1 WHERE recording_id=?2",
                turso::params![generation,id]).await?;
            finish_file(connection, id, generation, None).await?;
            Ok(Progress {
                evaluated: 1,
                ..Progress::default()
            })
        }
        Err(error) => {
            tracing::warn!(recording_id=id,%error,"retention evaluation quarantined recording");
            finish_file(connection, id, generation, Some("evaluation_failed")).await?;
            Ok(Progress {
                quarantined: 1,
                ..Progress::default()
            })
        }
    }
}

async fn commit_file(connection: &turso::Connection, id: &str, policy: &Policy) -> Result<()> {
    let snapshot = super::super::recording_snapshot(connection, id).await?;
    ensure!(
        snapshot
            .interval
            .end_ms()
            .checked_sub(snapshot.interval.start_ms())
            .is_some(),
        "recording span exceeds the retention query range"
    );
    record_span(connection, id).await?;
    let previous = super::super::read_previous(connection, id).await?;
    let revision = match previous {
        Some(previous) => previous
            .decision
            .policy_revision
            .checked_add(1)
            .context("retention revision exhausted")?,
        None => 1,
    };
    super::super::commit_snapshot(
        connection,
        id,
        revision,
        policy,
        policy.requires_event_evidence(),
    )
    .await?;
    Ok(())
}

async fn finish_file(
    connection: &turso::Connection,
    id: &str,
    generation: i64,
    error: Option<&str>,
) -> Result<()> {
    connection.execute("UPDATE recording_files SET retention_pending=?1,retention_generation=?2,retention_error=?3 WHERE id=?4",
        turso::params![if error.is_some(){2}else{0},generation,error,id]).await?;
    Ok(())
}

async fn record_span(connection: &turso::Connection, id: &str) -> Result<()> {
    connection.execute("INSERT INTO recording_retention_camera_spans(camera_id,span_ms)
        SELECT source_id,ended_at_ms-started_at_ms FROM recording_files WHERE id=?1 AND source_id IS NOT NULL
            AND ended_at_ms>started_at_ms AND typeof(ended_at_ms-started_at_ms)='integer'
        ON CONFLICT(camera_id) DO UPDATE SET span_ms=max(span_ms,excluded.span_ms)",[id]).await?;
    Ok(())
}
