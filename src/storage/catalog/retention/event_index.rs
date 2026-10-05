//! Bounded temporal candidates derived transactionally from canonical events.

use super::Snapshot;
use crate::storage::metadata::TimelineEvent;
use crate::storage::retention::MAX_EVENTS;
use anyhow::{Result, ensure};
use std::time::Instant;

const CANDIDATES: &str = "SELECT substr(e.id,1,257), e.camera_id, e.stream,
    substr(e.source,1,65), substr(e.kind,1,257), e.start_time_ms,e.end_time_ms,
    NULL,NULL,NULL,NULL,e.revision,NULL,'[]',NULL,'',NULL,NULL,NULL,i.bucket
    FROM recording_retention_event_index AS i INDEXED BY recording_retention_event_span
    LEFT JOIN recording_events AS e ON e.id=i.event_id
    WHERE i.camera_id=?1 AND i.level=?3 AND i.scope = ?2
        AND i.bucket BETWEEN ?4 AND ?5 ORDER BY i.bucket,i.event_id LIMIT ?6";

pub(super) async fn matching(
    connection: &turso::Connection,
    snapshot: &Snapshot,
) -> Result<Vec<TimelineEvent>> {
    ensure_ready(connection, &snapshot.camera_id).await?;
    let started = Instant::now();
    let mut statement = connection.prepare(CANDIDATES).await?;
    let mut events = Vec::with_capacity(MAX_EVENTS);
    let mut visited = 0;
    let stream_scope = format!(":{}", snapshot.stream_id);
    // ponytail: At most 130 indexed seeks replace traversal of the event archive.
    for level in 0..=64 {
        let low = if level == 64 {
            0
        } else {
            snapshot.interval.start_ms() >> level
        };
        let high = if level == 64 {
            0
        } else {
            (snapshot.interval.end_ms() - 1) >> level
        };
        for scope in ["", stream_scope.as_str()] {
            ensure!(
                started.elapsed() < super::super::BUSY_TIMEOUT,
                "retention event query budget exhausted"
            );
            let mut rows = statement
                .query(turso::params![
                    snapshot.camera_id.as_str(),
                    scope,
                    i64::from(level),
                    low,
                    high,
                    (MAX_EVENTS + 1 - visited) as i64
                ])
                .await?;
            while let Some(row) = rows.next().await? {
                ensure!(
                    visited < MAX_EVENTS,
                    "retention event snapshot limit exceeded"
                );
                visited += 1;
                ensure!(
                    started.elapsed() < super::super::BUSY_TIMEOUT,
                    "retention event query budget exhausted"
                );
                let event = super::super::event_from_row(&row, false)?;
                validate_candidate(&event, snapshot, level, row.get(19)?)?;
                if overlaps(&event, snapshot) {
                    events.push(event);
                }
            }
        }
    }
    Ok(events)
}

fn validate_candidate(
    event: &TimelineEvent,
    snapshot: &Snapshot,
    level: u32,
    bucket: i64,
) -> Result<()> {
    super::validate_identity(&event.id)?;
    super::validate_identity(&event.kind)?;
    ensure!(
        event.camera_id == snapshot.camera_id
            && event
                .stream
                .as_ref()
                .is_none_or(|stream| stream == &snapshot.stream_id),
        "retention event index identity mismatch"
    );
    ensure!(
        Bin::new(event.start_time_ms, event.end_time_ms)? == Bin { level, bucket },
        "retention event index geometry mismatch"
    );
    Ok(())
}

fn overlaps(event: &TimelineEvent, snapshot: &Snapshot) -> bool {
    event.start_time_ms < snapshot.interval.end_ms()
        && event.end_time_ms.is_none_or(|end| {
            end > snapshot.interval.start_ms()
                || (end == event.start_time_ms
                    && event.start_time_ms >= snapshot.interval.start_ms())
        })
}

pub(super) async fn reconcile(connection: &turso::Connection, limit: usize) -> Result<bool> {
    let started = Instant::now();
    let mut state = connection.query(
        "SELECT substr(cursor,1,257),complete FROM recording_retention_index_state WHERE singleton=1", (),
    ).await?;
    let state = state
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("retention event index state is missing"))?;
    let cursor: Option<String> = state.get(0)?;
    let migrated = state.get::<i64>(1)? == 1;
    if let Some(cursor) = &cursor {
        super::validate_identity(cursor)?;
    }
    let mut rows = connection
        .query(
            reconciliation_sql(migrated, cursor.is_some()),
            turso::params![cursor.as_deref().unwrap_or(""), limit as i64],
        )
        .await?;
    let mut staged = Vec::with_capacity(limit);
    while let Some(row) = rows.next().await? {
        ensure!(staged.len() < limit, "retention event index batch exceeded");
        let id: String = row.get(0)?;
        super::validate_identity(&id)?;
        let camera: String = row.get(1)?;
        let stream: Option<String> = row.get(2)?;
        super::validate_identity(&camera)?;
        if let Some(stream) = &stream {
            super::validate_identity(stream)?;
        }
        staged.push((id, camera, stream, Bin::new(row.get(3)?, row.get(4)?)?));
    }
    drop(rows);
    for (id, camera, stream, bin) in &staged {
        ensure!(
            started.elapsed() < super::super::BUSY_TIMEOUT,
            "retention event index budget exhausted"
        );
        store(connection, id, camera, stream.as_deref(), *bin).await?;
    }
    if !migrated {
        let next = staged
            .last()
            .map(|event| event.0.as_str())
            .or(cursor.as_deref());
        connection.execute("UPDATE recording_retention_index_state SET cursor=?1,complete=?2 WHERE singleton=1",
            turso::params![next, i64::from(staged.len() < limit)]).await?;
    }
    let mut pending = connection.query(
        "SELECT 1 FROM recording_retention_event_index INDEXED BY recording_retention_event_pending
         WHERE level=-1 LIMIT 1", (),
    ).await?;
    Ok((migrated || staged.len() < limit) && pending.next().await?.is_none())
}

const fn reconciliation_sql(migrated: bool, has_cursor: bool) -> &'static str {
    if migrated {
        "SELECT substr(e.id,1,257),substr(e.camera_id,1,257),substr(e.stream,1,257),e.start_time_ms,e.end_time_ms
         FROM recording_retention_event_index AS i INDEXED BY recording_retention_event_pending
         LEFT JOIN recording_events AS e ON e.id=i.event_id
         WHERE i.level=-1 ORDER BY i.event_id LIMIT ?2"
    } else if has_cursor {
        "SELECT substr(id,1,257),substr(camera_id,1,257),substr(stream,1,257),start_time_ms,end_time_ms FROM recording_events
         WHERE id>?1 ORDER BY id LIMIT ?2"
    } else {
        "SELECT substr(id,1,257),substr(camera_id,1,257),substr(stream,1,257),start_time_ms,end_time_ms FROM recording_events
         WHERE id>=?1 ORDER BY id LIMIT ?2"
    }
}

async fn store(
    connection: &turso::Connection,
    id: &str,
    camera: &str,
    stream: Option<&str>,
    bin: Bin,
) -> Result<()> {
    let scope = stream.map_or_else(String::new, |stream| format!(":{stream}"));
    connection
        .execute(
            "INSERT INTO recording_retention_event_index(event_id,camera_id,scope,level,bucket)
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT(event_id) DO UPDATE SET camera_id=excluded.camera_id,
             scope=excluded.scope,level=excluded.level,bucket=excluded.bucket",
            turso::params![id, camera, scope, i64::from(bin.level), bin.bucket],
        )
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bin {
    level: u32,
    bucket: i64,
}

impl Bin {
    fn new(start: i64, end: Option<i64>) -> Result<Self> {
        ensure!(
            end.is_none_or(|end| end >= start),
            "retention event ends before it starts"
        );
        let inclusive_end = match end {
            Some(end) if end > start => end - 1,
            Some(_) => start,
            None => i64::MAX,
        };
        let level = 64 - (start.cast_unsigned() ^ inclusive_end.cast_unsigned()).leading_zeros();
        let bucket = if level == 64 { 0 } else { start >> level };
        Ok(Self { level, bucket })
    }
}

pub(in crate::storage::catalog) async fn initialize(connection: &turso::Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS recording_retention_event_index (
            event_id TEXT NOT NULL PRIMARY KEY REFERENCES recording_events(id) ON DELETE CASCADE,
            camera_id TEXT NOT NULL, scope TEXT NOT NULL,
            level INTEGER NOT NULL CHECK (typeof(level)='integer' AND level BETWEEN -1 AND 64),
            bucket INTEGER NOT NULL CHECK (typeof(bucket)='integer')
         );
         CREATE INDEX IF NOT EXISTS recording_retention_event_span
             ON recording_retention_event_index(camera_id,level,scope,bucket,event_id);
         CREATE INDEX IF NOT EXISTS recording_retention_event_pending
             ON recording_retention_event_index(event_id) WHERE level = -1;
         CREATE TABLE IF NOT EXISTS recording_retention_index_state (
             singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
             cursor TEXT, complete INTEGER NOT NULL CHECK (complete IN (0,1))
         );
         INSERT OR IGNORE INTO recording_retention_index_state(singleton,cursor,complete)
             SELECT 1,NULL,NOT EXISTS (SELECT 1 FROM recording_events LIMIT 1);
         CREATE TRIGGER IF NOT EXISTS recording_retention_index_insert
         AFTER INSERT ON recording_events BEGIN
             INSERT INTO recording_retention_event_index(event_id,camera_id,scope,level,bucket)
             VALUES (NEW.id,NEW.camera_id,CASE WHEN NEW.stream IS NULL THEN '' ELSE ':' || NEW.stream END,-1,0);
         END;
         CREATE TRIGGER IF NOT EXISTS recording_retention_index_update
         AFTER UPDATE OF id,camera_id,stream,start_time_ms,end_time_ms ON recording_events BEGIN
             DELETE FROM recording_retention_event_index WHERE event_id=OLD.id;
             INSERT INTO recording_retention_event_index(event_id,camera_id,scope,level,bucket)
             VALUES (NEW.id,NEW.camera_id,CASE WHEN NEW.stream IS NULL THEN '' ELSE ':' || NEW.stream END,-1,0);
         END;
         CREATE TRIGGER IF NOT EXISTS recording_retention_index_delete
         AFTER DELETE ON recording_events BEGIN
             DELETE FROM recording_retention_event_index WHERE event_id=OLD.id;
         END;",
        )
        .await?;
    Ok(())
}

pub(in crate::storage::catalog) async fn write(
    connection: &turso::Connection,
    event: &TimelineEvent,
) -> Result<()> {
    let bin = Bin::new(event.start_time_ms, event.end_time_ms)?;
    store(
        connection,
        &event.id,
        &event.camera_id,
        event.stream.as_deref(),
        bin,
    )
    .await
}

pub(super) async fn ensure_ready(connection: &turso::Connection, camera: &str) -> Result<()> {
    let mut state = connection
        .query(
            "SELECT complete FROM recording_retention_index_state WHERE singleton=1",
            (),
        )
        .await?;
    let row = state
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("retention event index state is missing"))?;
    ensure!(
        row.get::<i64>(0)? == 1,
        "retention event index migration is incomplete"
    );
    let mut pending = connection.query(
        "SELECT 1 FROM recording_retention_event_index INDEXED BY recording_retention_event_span
         WHERE camera_id=?1 AND level=-1 LIMIT 1", turso::params![camera],
    ).await?;
    ensure!(
        pending.next().await?.is_none(),
        "retention event index has pending canonical changes"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Bin;

    #[test]
    fn native_candidates_use_the_temporal_index_without_sorting() {
        let root = std::env::temp_dir().join(format!("retention-plan-{}", uuid::Uuid::new_v4()));
        let catalog =
            super::super::super::RecordingCatalog::open(&root.join("catalog.db")).unwrap();
        let handle = catalog.handle();
        let owner = handle.retention.upgrade().unwrap();
        {
            let connection = owner.connection.lock().unwrap();
            for scope in ["", ":main"] {
                let mut rows = pollster::block_on(connection.query(
                    &format!("EXPLAIN QUERY PLAN {}", super::CANDIDATES),
                    turso::params!["front", scope, 0_i64, 0_i64, 10000_i64, 257_i64],
                ))
                .unwrap();
                let mut found = false;
                for _ in 0..16 {
                    let Some(row) = pollster::block_on(rows.next()).unwrap() else {
                        break;
                    };
                    let detail: String = row.get(3).unwrap();
                    assert!(
                        !detail.contains("SORT") && !detail.contains("SCAN i"),
                        "{detail}"
                    );
                    if detail.contains("recording_retention_event_span") {
                        assert!(
                            detail.contains("scope=?") && detail.contains("bucket>"),
                            "{detail}"
                        );
                        found = true;
                    }
                }
                assert!(found);
            }
        }
        drop(owner);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_overlapping_interval_has_a_candidate_bucket() {
        let boundaries = [
            i64::MIN,
            -65537,
            -65536,
            -2,
            -1,
            0,
            1,
            2,
            65535,
            65536,
            i64::MAX,
        ];
        for &start in &boundaries {
            for end in boundaries.iter().copied().map(Some).chain([None]) {
                if end.is_some_and(|end| end < start) {
                    continue;
                }
                let bin = Bin::new(start, end).unwrap();
                for &query_start in &boundaries {
                    for &query_end in &boundaries {
                        if query_start >= query_end || start >= query_end {
                            continue;
                        }
                        let overlaps = end.is_none_or(|end| {
                            end > query_start || (end == start && start >= query_start)
                        });
                        if !overlaps {
                            continue;
                        }
                        if bin.level == 64 {
                            assert_eq!(bin.bucket, 0);
                        } else {
                            assert!((query_start >> bin.level) <= bin.bucket);
                            assert!(bin.bucket <= ((query_end - 1) >> bin.level));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn pulse_open_and_signed_boundaries_are_explicit() {
        assert_eq!(
            Bin::new(-1, Some(-1)).unwrap(),
            Bin {
                level: 0,
                bucket: -1
            }
        );
        assert_eq!(
            Bin::new(-1, Some(1)).unwrap(),
            Bin {
                level: 64,
                bucket: 0
            }
        );
        assert_eq!(
            Bin::new(i64::MAX, None).unwrap(),
            Bin {
                level: 0,
                bucket: i64::MAX
            }
        );
        assert!(Bin::new(1, Some(0)).is_err());
    }
}
