use anyhow::{Context, Result, ensure};

pub(super) async fn request(connection: &turso::Connection, json: Option<&str>) -> Result<bool> {
    let mut rows = connection
        .query(
            "SELECT request_pending,substr(requested_json,1,524289)
        FROM recording_retention_runtime WHERE singleton=1",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .context("retention runtime state missing")?;
    let requested: Option<String> = row.get(1)?;
    ensure!(
        requested
            .as_ref()
            .is_none_or(|value| value.len() <= super::SETTINGS_BYTES_MAX),
        "retention settings metadata limit exceeded"
    );
    let pending = row.get::<i64>(0)? != 0;
    drop(rows);
    if pending && requested.as_deref() != json {
        return Ok(false);
    }
    connection
        .execute(
            "UPDATE recording_retention_runtime SET requested_json=?1,
        request_pending=(settings_json IS NOT ?1) WHERE singleton=1",
            turso::params![json],
        )
        .await?;
    Ok(true)
}

pub(super) async fn activation_pending(connection: &turso::Connection) -> Result<bool> {
    let mut rows = connection
        .query(
            "SELECT request_pending,complete FROM recording_retention_runtime WHERE singleton=1",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .context("retention runtime state missing")?;
    Ok(row.get::<i64>(0)? != 0 || row.get::<i64>(1)? != 1)
}

pub(super) async fn candidates(
    connection: &turso::Connection,
    limit: usize,
) -> Result<Vec<String>> {
    if activation_pending(connection).await? {
        return Ok(Vec::new());
    }
    let mut rows = connection
        .query(
            "SELECT generation,expiry_cursor_ms,substr(expiry_cursor_id,1,257)
        FROM recording_retention_runtime WHERE singleton=1",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .context("retention runtime state missing")?;
    let generation: i64 = row.get(0)?;
    let cursor_ms: i64 = row.get(1)?;
    let cursor_id: String = row.get(2)?;
    if !cursor_id.is_empty() {
        super::super::validate_identity(&cursor_id)?;
    }
    drop(rows);
    let now = super::super::now_ms()?;
    let mut records = Vec::with_capacity(limit);
    let mut rows = connection
        .query(
            "SELECT recording_id,expiry_at_ms FROM recording_retention_decisions
        INDEXED BY recording_retention_runtime_deadline WHERE runtime_generation=?1
        AND expiry_eligible=1 AND expiry_at_ms=?2 AND expiry_at_ms<=?3 AND recording_id>?4
        ORDER BY recording_id LIMIT ?5",
            turso::params![generation, cursor_ms, now, cursor_id, limit as i64],
        )
        .await?;
    collect(&mut rows, &mut records, limit).await?;
    drop(rows);
    if records.len() < limit {
        let mut rows = connection
            .query(
                "SELECT recording_id,expiry_at_ms FROM recording_retention_decisions
            INDEXED BY recording_retention_runtime_deadline WHERE runtime_generation=?1
            AND expiry_eligible=1 AND expiry_at_ms>?2 AND expiry_at_ms<=?3 ORDER BY expiry_at_ms,recording_id LIMIT ?4",
                turso::params![generation, cursor_ms, now, (limit - records.len()) as i64],
            )
            .await?;
        collect(&mut rows, &mut records, limit).await?;
    }
    let (id, time) = records
        .last()
        .map(|(id, time)| (id.as_str(), *time))
        .unwrap_or(("", i64::MIN));
    connection.execute("UPDATE recording_retention_runtime SET expiry_cursor_ms=?1,expiry_cursor_id=?2 WHERE singleton=1",
        turso::params![time,id]).await?;
    Ok(records.into_iter().map(|(id, _)| id).collect())
}

async fn collect(
    rows: &mut turso::Rows,
    records: &mut Vec<(String, i64)>,
    limit: usize,
) -> Result<()> {
    while let Some(row) = rows.next().await? {
        ensure!(
            records.len() < limit,
            "retention expiry candidate bound exceeded"
        );
        let id: String = row.get(0)?;
        super::super::validate_identity(&id)?;
        records.push((id, row.get(1)?));
    }
    Ok(())
}
