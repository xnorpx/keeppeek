use super::{EVENT_DETAIL_COLUMNS, TimelineEvent, event_from_row, maximum_search_revision};
use std::{collections::BTreeMap, fmt::Write as _};

/// The caller owns a single IMMEDIATE transaction, including the search revision.
pub(super) async fn apply(
    connection: &turso::Connection,
    endings: &[(String, i64)],
) -> anyhow::Result<Vec<Option<TimelineEvent>>> {
    let placeholders = vec!["?"; endings.len()].join(",");
    let mut rows = connection.query(
        format!("SELECT {EVENT_DETAIL_COLUMNS} FROM recording_events WHERE end_time_ms IS NULL AND id IN ({placeholders})"),
        turso::params_from_iter(endings.iter().map(|(id, _)| id.as_str())),
    ).await?;
    let mut open = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let event = event_from_row(&row, true)?;
        open.insert(event.id.clone(), event);
    }
    drop(rows);
    let mut committed = Vec::with_capacity(endings.len());
    for (id, end) in endings {
        // A duplicate later in this batch observes the same already-closed result as a retry.
        let update = open
            .remove(id)
            .map(|mut event| -> anyhow::Result<_> {
                anyhow::ensure!(
                    *end >= event.start_time_ms,
                    "event {id} end {end} ms precedes its start {} ms",
                    event.start_time_ms
                );
                event.revision = event
                    .revision
                    .checked_add(1)
                    .filter(|value| i64::try_from(*value).is_ok())
                    .ok_or_else(|| anyhow::anyhow!("event revision exceeded its limit"))?;
                event.end_time_ms = Some(*end);
                Ok(event)
            })
            .transpose()?;
        committed.push(update);
    }
    if committed.iter().any(Option::is_some) {
        persist(connection, &committed).await?;
    }
    Ok(committed)
}

async fn persist(
    connection: &turso::Connection,
    events: &[Option<TimelineEvent>],
) -> anyhow::Result<()> {
    let mut revision = maximum_search_revision(connection).await?;
    let mut ids = Vec::with_capacity(events.len());
    let mut end_times = String::from("CASE id");
    let mut revisions = String::from("CASE id");
    let mut values = Vec::with_capacity(3 * events.len());
    for event in events.iter().flatten() {
        revision = revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("event search revision exceeded its limit"))?;
        let param = values.len() + 1;
        ids.push(format!("?{param}"));
        write!(end_times, " WHEN ?{param} THEN ?{}", param + 1)?;
        write!(revisions, " WHEN ?{param} THEN ?{}", param + 2)?;
        values.extend([
            turso::Value::Text(event.id.clone()),
            turso::Value::Integer(event.end_time_ms.expect("newly closed event")),
            turso::Value::Integer(revision),
        ]);
    }
    let sql = format!(
        "UPDATE recording_events SET end_time_ms = {end_times} END,
        revision = revision + 1, search_revision = {revisions} END WHERE id IN ({})",
        ids.join(",")
    );
    let changed = connection
        .execute(sql, turso::params_from_iter(values))
        .await?;
    anyhow::ensure!(
        changed == ids.len() as u64,
        "native event closure count changed during transaction"
    );
    let state_rows = connection
        .execute(
            "UPDATE recording_event_search_state SET revision = ?1 WHERE id = 1",
            turso::params![revision],
        )
        .await?;
    anyhow::ensure!(state_rows == 1, "missing event search revision");
    for event in events.iter().flatten() {
        super::analytics::delta::sync_event_upsert(connection, event).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
