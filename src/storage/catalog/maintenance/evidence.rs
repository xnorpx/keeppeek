//! Captures bounded evidence relationships and actual coverage gaps in a catalog snapshot.

use super::{
    MAX_RECORDINGS, MAX_SCAN_RECORDINGS, Recording, Scope, check_deadline, validate_identifier,
};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Interval {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Preserves the independent bookmark revision used by destructive previews.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Evidence {
    pub bookmark_revision: u64,
    pub bookmarks: Vec<String>,
    pub gaps: Vec<Interval>,
}

pub(super) async fn read(
    connection: &turso::Connection,
    scope: &Scope,
    recordings: &[Recording],
    deadline: Instant,
) -> anyhow::Result<Evidence> {
    let bookmark_revision = revision(connection).await?;
    let source = match scope {
        Scope::Recording { source_id, .. } | Scope::TimeRange { source_id, .. } => source_id,
    };
    let start = recordings
        .iter()
        .map(|recording| recording.started_at_ms)
        .min()
        .unwrap_or(0);
    let end = recordings
        .iter()
        .filter_map(|recording| recording.ended_at_ms)
        .max()
        .unwrap_or(start);
    let mut rows = connection
        .query(
            "SELECT event_id, event_start_ms FROM event_bookmarks
         WHERE source_id = ?1 AND active = 1 AND event_start_ms >= ?2 AND event_start_ms < ?3
         ORDER BY event_start_ms, event_id LIMIT ?4",
            turso::params![
                source.as_str(),
                start,
                end,
                i64::try_from(MAX_SCAN_RECORDINGS + 1)?
            ],
        )
        .await?;
    let mut bookmarks = Vec::new();
    let mut scanned = 0;
    while let Some(row) = rows.next().await? {
        check_deadline(deadline)?;
        scanned += 1;
        anyhow::ensure!(
            scanned <= MAX_SCAN_RECORDINGS,
            "bookmark scope exceeds inspection limit"
        );
        let timestamp: i64 = row.get(1)?;
        if recordings.iter().any(|recording| {
            timestamp >= recording.started_at_ms
                && recording.ended_at_ms.is_some_and(|end| timestamp < end)
        }) {
            anyhow::ensure!(
                bookmarks.len() < MAX_RECORDINGS,
                "bookmark scope exceeds preview limit"
            );
            let id: String = row.get(0)?;
            validate_identifier(&id)?;
            bookmarks.push(id);
        }
    }
    drop(rows);
    let gaps = coverage_gaps(connection, recordings, start, end, deadline).await?;
    Ok(Evidence {
        bookmark_revision,
        bookmarks,
        gaps,
    })
}

pub(super) async fn revision(connection: &turso::Connection) -> anyhow::Result<u64> {
    let mut rows = connection
        .query("SELECT revision FROM event_bookmark_state WHERE id = 1", ())
        .await?;
    let revision = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("bookmark revision is unavailable"))?
        .get::<i64>(0)?;
    Ok(u64::try_from(revision)?)
}

async fn coverage_gaps(
    connection: &turso::Connection,
    recordings: &[Recording],
    start: i64,
    end: i64,
    deadline: Instant,
) -> anyhow::Result<Vec<Interval>> {
    let mut coverage = Vec::new();
    for recording in recordings {
        check_deadline(deadline)?;
        let remaining = MAX_SCAN_RECORDINGS - coverage.len();
        let mut rows = connection.query(
            "SELECT start_ms, end_ms FROM recording_coverage_ranges WHERE recording_id = ?1 ORDER BY start_ms LIMIT ?2",
            turso::params![recording.recording_id.as_str(), i64::try_from(remaining + 1)?],
        ).await?;
        while let Some(row) = rows.next().await? {
            check_deadline(deadline)?;
            anyhow::ensure!(
                coverage.len() < MAX_SCAN_RECORDINGS,
                "coverage scope exceeds inspection limit"
            );
            let interval = Interval {
                start_ms: row.get(0)?,
                end_ms: row.get(1)?,
            };
            anyhow::ensure!(
                interval.start_ms >= 0 && interval.end_ms > interval.start_ms,
                "invalid maintenance coverage"
            );
            coverage.push(interval);
        }
    }
    coverage.sort_unstable_by_key(|interval| interval.start_ms);
    let mut cursor = start;
    let mut gaps = Vec::new();
    for interval in coverage {
        if interval.start_ms > cursor && cursor < end {
            anyhow::ensure!(
                gaps.len() < MAX_RECORDINGS,
                "coverage gaps exceed preview limit"
            );
            gaps.push(Interval {
                start_ms: cursor,
                end_ms: interval.start_ms.min(end),
            });
        }
        cursor = cursor.max(interval.end_ms);
    }
    if cursor < end {
        anyhow::ensure!(
            gaps.len() < MAX_RECORDINGS,
            "coverage gaps exceed preview limit"
        );
        gaps.push(Interval {
            start_ms: cursor,
            end_ms: end,
        });
    }
    Ok(gaps)
}
