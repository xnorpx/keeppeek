use super::super::Progress;
use anyhow::{Context, Result, ensure};

struct Job {
    camera: String,
    lower: i64,
    upper: i64,
    cursor_ms: Option<i64>,
    cursor_id: Option<String>,
    high_water_ms: Option<i64>,
    high_water_id: Option<String>,
}

async fn load_job(connection: &turso::Connection, generation: i64) -> Result<Option<Job>> {
    let mut rows = connection
        .query(
            "SELECT camera_id,lower_ms,upper_ms,cursor_ms,cursor_id,high_water_ms,high_water_id
        FROM recording_retention_camera_work WHERE generation<=?1 ORDER BY last_processed,camera_id LIMIT 1",
            [generation],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let job = Job {
        camera: row.get(0)?,
        lower: row.get(1)?,
        upper: row.get(2)?,
        cursor_ms: row.get(3)?,
        cursor_id: row.get(4)?,
        high_water_ms: row.get(5)?,
        high_water_id: row.get(6)?,
    };
    drop(rows);
    super::super::super::validate_identity(&job.camera)?;
    ensure!(
        job.lower <= job.upper,
        "invalid retention event work interval"
    );
    for id in [job.cursor_id.as_deref(), job.high_water_id.as_deref()]
        .into_iter()
        .flatten()
    {
        super::super::super::validate_identity(id)?;
    }
    Ok(Some(job))
}

pub(super) async fn step(
    connection: &turso::Connection,
    generation: i64,
) -> Result<Option<Progress>> {
    let Some(mut job) = load_job(connection, generation).await? else {
        return Ok(None);
    };
    turn(connection, &job.camera).await?;
    let Some(span) = span(connection, &job.camera).await? else {
        finish(connection, &job.camera).await?;
        return Ok(Some(Progress {
            pending: true,
            ..Progress::default()
        }));
    };
    if !initialize_water(connection, &mut job).await? {
        finish(connection, &job.camera).await?;
        return Ok(Some(Progress {
            pending: true,
            ..Progress::default()
        }));
    }
    let lower = job.lower.saturating_sub(span);
    let Some(row) = candidate(connection, &job, lower).await? else {
        finish(connection, &job.camera).await?;
        return Ok(Some(Progress {
            pending: true,
            ..Progress::default()
        }));
    };
    let id: String = row.get(0)?;
    let start: i64 = row.get(1)?;
    super::super::super::validate_identity(&id)?;
    let high_ms = job
        .high_water_ms
        .context("retention work high-water time missing")?;
    let high_id = job
        .high_water_id
        .as_deref()
        .context("retention work high-water ID missing")?;
    if (start, id.as_str()) > (high_ms, high_id) {
        finish(connection, &job.camera).await?;
        return Ok(Some(Progress {
            pending: true,
            ..Progress::default()
        }));
    }
    let end: Option<i64> = row.get(2)?;
    let eligible = row.get::<i64>(3)? == 1 && row.get::<i64>(4)? == 0 && row.get::<i64>(5)? == 0;
    let mut progress = if eligible && end.is_some_and(|end| end > job.lower) {
        super::evaluate(connection, &id, generation).await?
    } else {
        Progress::default()
    };
    connection.execute("UPDATE recording_retention_camera_work SET cursor_ms=?1,cursor_id=?2 WHERE camera_id=?3",
        turso::params![start,id,job.camera]).await?;
    progress.pending = true;
    Ok(Some(progress))
}

async fn span(connection: &turso::Connection, camera: &str) -> Result<Option<i64>> {
    let mut rows = connection
        .query(
            "SELECT span_ms FROM recording_retention_camera_spans WHERE camera_id=?1",
            [camera],
        )
        .await?;
    let span = rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?;
    ensure!(
        span.is_none_or(|span| span > 0),
        "invalid retention recording span"
    );
    Ok(span)
}

async fn turn(connection: &turso::Connection, camera: &str) -> Result<()> {
    connection
        .execute(
            "UPDATE recording_retention_runtime SET work_clock=work_clock+1 WHERE singleton=1",
            (),
        )
        .await?;
    connection.execute("UPDATE recording_retention_camera_work
        SET last_processed=(SELECT work_clock FROM recording_retention_runtime WHERE singleton=1) WHERE camera_id=?1",[camera]).await?;
    Ok(())
}

async fn initialize_water(connection: &turso::Connection, job: &mut Job) -> Result<bool> {
    if job.high_water_ms.is_some() && job.high_water_id.is_some() {
        return Ok(true);
    }
    let mut rows=connection.query("SELECT started_at_ms,id FROM recording_files INDEXED BY recording_retention_camera_files
        WHERE source_id=?1 ORDER BY started_at_ms DESC,id DESC LIMIT 1",[job.camera.as_str()]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    let time: i64 = row.get(0)?;
    let id: String = row.get(1)?;
    super::super::super::validate_identity(&id)?;
    job.high_water_ms = Some(time);
    job.high_water_id = Some(id.clone());
    drop(rows);
    connection.execute("UPDATE recording_retention_camera_work SET high_water_ms=?1,high_water_id=?2 WHERE camera_id=?3",
        turso::params![time,id,job.camera.as_str()]).await?;
    Ok(true)
}

async fn candidate(
    connection: &turso::Connection,
    job: &Job,
    lower: i64,
) -> Result<Option<turso::Row>> {
    let cursor = job.cursor_ms.unwrap_or(lower);
    let mut rows=connection.query("SELECT id,started_at_ms,ended_at_ms,finalized,cleanup_pending,
        EXISTS(SELECT 1 FROM storage_recording_retirements WHERE recording_id=recording_files.id AND complete=0)
        FROM recording_files INDEXED BY recording_retention_camera_files
        WHERE source_id=?1 AND started_at_ms=?2 AND started_at_ms<?4 AND id>?3 ORDER BY id LIMIT 1",
        turso::params![job.camera.as_str(),cursor,job.cursor_id.as_deref().unwrap_or(""),job.upper]).await?;
    if let Some(row) = rows.next().await? {
        return Ok(Some(row));
    }
    drop(rows);
    let mut rows=connection.query("SELECT id,started_at_ms,ended_at_ms,finalized,cleanup_pending,
        EXISTS(SELECT 1 FROM storage_recording_retirements WHERE recording_id=recording_files.id AND complete=0)
        FROM recording_files INDEXED BY recording_retention_camera_files
        WHERE source_id=?1 AND started_at_ms>?2 AND started_at_ms<?3
        ORDER BY started_at_ms,id LIMIT 1",turso::params![job.camera.as_str(),cursor,job.upper]).await?;
    Ok(rows.next().await?)
}

async fn finish(connection: &turso::Connection, camera: &str) -> Result<()> {
    let mut rows = connection
        .query(
            "SELECT next_lower_ms,queued_lower_ms FROM recording_retention_camera_work WHERE camera_id=?1",
            [camera],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .context("retention camera work missing")?;
    let next: Option<i64> = row.get(0)?;
    let queued: Option<i64> = row.get(1)?;
    drop(rows);
    if next.is_some() {
        connection.execute("UPDATE recording_retention_camera_work SET lower_ms=next_lower_ms,upper_ms=next_upper_ms,
            next_lower_ms=NULL,next_upper_ms=NULL,cursor_ms=NULL,cursor_id=NULL,high_water_ms=NULL,high_water_id=NULL
            WHERE camera_id=?1",[camera]).await?;
    } else if queued.is_some() {
        connection.execute("UPDATE recording_retention_camera_work SET lower_ms=queued_lower_ms,upper_ms=queued_upper_ms,
            queued_lower_ms=NULL,queued_upper_ms=NULL,cursor_ms=NULL,cursor_id=NULL,high_water_ms=NULL,high_water_id=NULL,
            generation=(SELECT generation+request_pending FROM recording_retention_runtime WHERE singleton=1)
            WHERE camera_id=?1",[camera]).await?;
    } else {
        connection
            .execute(
                "DELETE FROM recording_retention_camera_work WHERE camera_id=?1",
                [camera],
            )
            .await?;
    }
    Ok(())
}
