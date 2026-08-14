use crate::storage::metadata::{EventSource, TimelineEvent};
use serde::Serialize;
use std::{
    path::Path,
    sync::mpsc::{self, Receiver, SyncSender},
    thread::JoinHandle,
    time::Duration,
};

const COMMAND_CAPACITY: usize = 256;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRecording {
    pub id: String,
    pub stream_id: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub path: String,
    pub init_offset: u64,
    pub init_len: u64,
    pub finalized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFragment {
    pub recording_id: String,
    pub sequence: u64,
    pub start_ms: i64,
    pub duration_ms: u64,
    pub byte_offset: u64,
    pub byte_len: u64,
    pub random_access: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogStats {
    pub recording_files: u64,
    pub finalized_files: u64,
    pub active_files: u64,
    pub fragments: u64,
    pub fragment_bytes: u64,
    pub events: u64,
    pub open_events: u64,
    pub event_thumbnails: u64,
}

#[derive(Clone)]
pub struct RecordingCatalogHandle {
    tx: SyncSender<Command>,
}

pub struct RecordingCatalog {
    handle: RecordingCatalogHandle,
    thread: Option<JoinHandle<()>>,
}

enum Command {
    UpsertRecording {
        recording: CatalogRecording,
        reply: SyncSender<anyhow::Result<()>>,
    },
    InsertFragment {
        fragment: CatalogFragment,
        reply: SyncSender<anyhow::Result<()>>,
    },
    FragmentsInRange {
        stream_id: String,
        start_ms: i64,
        end_ms: i64,
        reply: SyncSender<anyhow::Result<Vec<CatalogFragment>>>,
    },
    InsertEvent {
        event: TimelineEvent,
        reply: SyncSender<anyhow::Result<()>>,
    },
    CloseEvent {
        id: String,
        end_time_ms: i64,
        reply: SyncSender<anyhow::Result<()>>,
    },
    AttachEventThumbnail {
        id: String,
        thumbnail_filename: String,
        reply: SyncSender<anyhow::Result<()>>,
    },
    DetachEventThumbnail {
        id: String,
        reply: SyncSender<anyhow::Result<()>>,
    },
    EventsInRange {
        camera_id: String,
        start_ms: i64,
        end_ms: i64,
        reply: SyncSender<anyhow::Result<Vec<TimelineEvent>>>,
    },
    EventById {
        id: String,
        reply: SyncSender<anyhow::Result<Option<TimelineEvent>>>,
    },
    Stats {
        reply: SyncSender<anyhow::Result<CatalogStats>>,
    },
    Shutdown,
}

impl RecordingCatalog {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Turso catalog path is not valid UTF-8"))?;
        let database = pollster::block_on(turso::Builder::new_local(path).build())?;
        let connection = database.connect()?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        pollster::block_on(initialize_schema(&connection))?;

        let (tx, rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let thread = std::thread::Builder::new()
            .name("recording-catalog".to_owned())
            .spawn(move || run_catalog(connection, rx))?;

        Ok(Self {
            handle: RecordingCatalogHandle { tx },
            thread: Some(thread),
        })
    }

    pub fn handle(&self) -> RecordingCatalogHandle {
        self.handle.clone()
    }

    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        let _ = self.handle.tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("recording catalog thread panicked");
        }
    }
}

impl Drop for RecordingCatalog {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

impl RecordingCatalogHandle {
    pub fn upsert_recording(&self, recording: CatalogRecording) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::UpsertRecording { recording, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn insert_fragment(&self, fragment: CatalogFragment) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::InsertFragment { fragment, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn fragments_in_range(
        &self,
        stream_id: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> anyhow::Result<Vec<CatalogFragment>> {
        if start_ms >= end_ms {
            anyhow::bail!("fragment query start must be before end");
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::FragmentsInRange {
                stream_id: stream_id.to_owned(),
                start_ms,
                end_ms,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn insert_event(&self, event: TimelineEvent) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::InsertEvent { event, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn close_event(&self, id: &str, end_time_ms: i64) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::CloseEvent {
                id: id.to_owned(),
                end_time_ms,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn attach_event_thumbnail(&self, id: &str, thumbnail_filename: &str) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::AttachEventThumbnail {
                id: id.to_owned(),
                thumbnail_filename: thumbnail_filename.to_owned(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn detach_event_thumbnail(&self, id: &str) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::DetachEventThumbnail {
                id: id.to_owned(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn events_in_range(
        &self,
        camera_id: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> anyhow::Result<Vec<TimelineEvent>> {
        if start_ms >= end_ms {
            anyhow::bail!("event query start must be before end");
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::EventsInRange {
                camera_id: camera_id.to_owned(),
                start_ms,
                end_ms,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn event_by_id(&self, id: &str) -> anyhow::Result<Option<TimelineEvent>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::EventById {
                id: id.to_owned(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn stats(&self) -> anyhow::Result<CatalogStats> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::Stats { reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }
}

fn run_catalog(connection: turso::Connection, rx: Receiver<Command>) {
    while let Ok(command) = rx.recv() {
        match command {
            Command::UpsertRecording { recording, reply } => {
                let _ = reply.send(pollster::block_on(upsert_recording(&connection, recording)));
            }
            Command::InsertFragment { fragment, reply } => {
                let _ = reply.send(pollster::block_on(insert_fragment(&connection, fragment)));
            }
            Command::FragmentsInRange {
                stream_id,
                start_ms,
                end_ms,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(fragments_in_range(
                    &connection,
                    &stream_id,
                    start_ms,
                    end_ms,
                )));
            }
            Command::InsertEvent { event, reply } => {
                let _ = reply.send(pollster::block_on(insert_event(&connection, event)));
            }
            Command::CloseEvent {
                id,
                end_time_ms,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(close_event(
                    &connection,
                    &id,
                    end_time_ms,
                )));
            }
            Command::AttachEventThumbnail {
                id,
                thumbnail_filename,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(attach_event_thumbnail(
                    &connection,
                    &id,
                    &thumbnail_filename,
                )));
            }
            Command::DetachEventThumbnail { id, reply } => {
                let _ = reply.send(pollster::block_on(detach_event_thumbnail(&connection, &id)));
            }
            Command::EventsInRange {
                camera_id,
                start_ms,
                end_ms,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(events_in_range(
                    &connection,
                    &camera_id,
                    start_ms,
                    end_ms,
                )));
            }
            Command::EventById { id, reply } => {
                let _ = reply.send(pollster::block_on(event_by_id(&connection, &id)));
            }
            Command::Stats { reply } => {
                let _ = reply.send(pollster::block_on(catalog_stats(&connection)));
            }
            Command::Shutdown => break,
        }
    }
}

async fn initialize_schema(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS recording_files (
                 id TEXT PRIMARY KEY,
                 stream_id TEXT NOT NULL,
                 started_at_ms INTEGER NOT NULL,
                 ended_at_ms INTEGER,
                 path TEXT NOT NULL UNIQUE,
                 init_offset INTEGER NOT NULL,
                 init_len INTEGER NOT NULL,
                 finalized INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS recording_files_stream_time
                 ON recording_files(stream_id, started_at_ms);
             CREATE TABLE IF NOT EXISTS recording_fragments (
                 recording_id TEXT NOT NULL REFERENCES recording_files(id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 start_ms INTEGER NOT NULL,
                 duration_ms INTEGER NOT NULL,
                 byte_offset INTEGER NOT NULL,
                 byte_len INTEGER NOT NULL,
                 random_access INTEGER NOT NULL,
                 PRIMARY KEY(recording_id, sequence)
             );
             CREATE INDEX IF NOT EXISTS recording_fragments_time
                 ON recording_fragments(start_ms);
             CREATE TABLE IF NOT EXISTS recording_events (
                 id TEXT PRIMARY KEY,
                 camera_id TEXT NOT NULL,
                 stream TEXT,
                 source TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 start_time_ms INTEGER NOT NULL,
                 end_time_ms INTEGER,
                 confidence REAL,
                 bbox_json TEXT,
                 zone TEXT,
                 thumbnail_filename TEXT
             );
             CREATE INDEX IF NOT EXISTS recording_events_camera_time
                 ON recording_events(camera_id, start_time_ms, end_time_ms);",
        )
        .await?;
    Ok(())
}

async fn upsert_recording(
    connection: &turso::Connection,
    recording: CatalogRecording,
) -> anyhow::Result<()> {
    connection
        .execute(
            "INSERT INTO recording_files (
                 id, stream_id, started_at_ms, ended_at_ms, path,
                 init_offset, init_len, finalized
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 stream_id = excluded.stream_id,
                 started_at_ms = excluded.started_at_ms,
                 ended_at_ms = excluded.ended_at_ms,
                 path = excluded.path,
                 init_offset = excluded.init_offset,
                 init_len = excluded.init_len,
                 finalized = excluded.finalized",
            turso::params![
                recording.id,
                recording.stream_id,
                recording.started_at_ms,
                recording.ended_at_ms,
                recording.path,
                to_i64(recording.init_offset, "recording init offset")?,
                to_i64(recording.init_len, "recording init length")?,
                i64::from(recording.finalized),
            ],
        )
        .await?;
    Ok(())
}

async fn insert_fragment(
    connection: &turso::Connection,
    fragment: CatalogFragment,
) -> anyhow::Result<()> {
    connection
        .execute(
            "INSERT INTO recording_fragments (
                 recording_id, sequence, start_ms, duration_ms,
                 byte_offset, byte_len, random_access
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            turso::params![
                fragment.recording_id,
                to_i64(fragment.sequence, "fragment sequence")?,
                fragment.start_ms,
                to_i64(fragment.duration_ms, "fragment duration")?,
                to_i64(fragment.byte_offset, "fragment byte offset")?,
                to_i64(fragment.byte_len, "fragment byte length")?,
                i64::from(fragment.random_access),
            ],
        )
        .await?;
    Ok(())
}

async fn fragments_in_range(
    connection: &turso::Connection,
    stream_id: &str,
    start_ms: i64,
    end_ms: i64,
) -> anyhow::Result<Vec<CatalogFragment>> {
    let mut rows = connection
        .query(
            "SELECT f.recording_id, f.sequence, f.start_ms, f.duration_ms,
                    f.byte_offset, f.byte_len, f.random_access
             FROM recording_fragments AS f
             JOIN recording_files AS r ON r.id = f.recording_id
             WHERE r.stream_id = ?1
               AND f.start_ms < ?3
               AND f.start_ms + f.duration_ms > ?2
             ORDER BY f.start_ms, f.sequence",
            turso::params![stream_id, start_ms, end_ms],
        )
        .await?;
    let mut fragments = Vec::new();
    while let Some(row) = rows.next().await? {
        fragments.push(CatalogFragment {
            recording_id: row.get(0)?,
            sequence: to_u64(row.get(1)?, "fragment sequence")?,
            start_ms: row.get(2)?,
            duration_ms: to_u64(row.get(3)?, "fragment duration")?,
            byte_offset: to_u64(row.get(4)?, "fragment byte offset")?,
            byte_len: to_u64(row.get(5)?, "fragment byte length")?,
            random_access: row.get::<i64>(6)? != 0,
        });
    }
    Ok(fragments)
}

async fn insert_event(connection: &turso::Connection, event: TimelineEvent) -> anyhow::Result<()> {
    if event.kind.is_empty() {
        anyhow::bail!("event kind must not be empty");
    }
    if event
        .end_time_ms
        .is_some_and(|end_time_ms| end_time_ms < event.start_time_ms)
    {
        anyhow::bail!("event end must not precede its start");
    }
    let bbox_json = event
        .bbox
        .map(|bbox| serde_json::to_string(&bbox))
        .transpose()?;
    connection
        .execute(
            "INSERT INTO recording_events (
                 id, camera_id, stream, source, kind, start_time_ms,
                 end_time_ms, confidence, bbox_json, zone, thumbnail_filename
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            turso::params![
                event.id,
                event.camera_id,
                event.stream,
                event.source.as_str(),
                event.kind,
                event.start_time_ms,
                event.end_time_ms,
                event.confidence,
                bbox_json,
                event.zone,
                event.thumbnail_filename,
            ],
        )
        .await?;
    Ok(())
}

async fn close_event(
    connection: &turso::Connection,
    id: &str,
    end_time_ms: i64,
) -> anyhow::Result<()> {
    let changed = connection
        .execute(
            "UPDATE recording_events
             SET end_time_ms = ?2
             WHERE id = ?1 AND start_time_ms <= ?2",
            turso::params![id, end_time_ms],
        )
        .await?;
    if changed == 0 {
        anyhow::bail!("event was not found or its end precedes its start");
    }
    Ok(())
}

async fn attach_event_thumbnail(
    connection: &turso::Connection,
    id: &str,
    thumbnail_filename: &str,
) -> anyhow::Result<()> {
    if thumbnail_filename.is_empty() {
        anyhow::bail!("event thumbnail filename must not be empty");
    }
    let changed = connection
        .execute(
            "UPDATE recording_events SET thumbnail_filename = ?2 WHERE id = ?1",
            turso::params![id, thumbnail_filename],
        )
        .await?;
    if changed == 0 {
        anyhow::bail!("event was not found");
    }
    Ok(())
}

async fn detach_event_thumbnail(connection: &turso::Connection, id: &str) -> anyhow::Result<()> {
    connection
        .execute(
            "UPDATE recording_events SET thumbnail_filename = NULL WHERE id = ?1",
            turso::params![id],
        )
        .await?;
    Ok(())
}

async fn events_in_range(
    connection: &turso::Connection,
    camera_id: &str,
    start_ms: i64,
    end_ms: i64,
) -> anyhow::Result<Vec<TimelineEvent>> {
    let mut rows = connection
        .query(
            "SELECT id, camera_id, stream, source, kind, start_time_ms,
                    end_time_ms, confidence, bbox_json, zone, thumbnail_filename
             FROM recording_events
             WHERE camera_id = ?1
               AND start_time_ms < ?3
               AND COALESCE(end_time_ms, start_time_ms + 1) > ?2
             ORDER BY start_time_ms, id",
            turso::params![camera_id, start_ms, end_ms],
        )
        .await?;
    let mut events = Vec::new();
    while let Some(row) = rows.next().await? {
        events.push(event_from_row(&row)?);
    }
    Ok(events)
}

async fn event_by_id(
    connection: &turso::Connection,
    id: &str,
) -> anyhow::Result<Option<TimelineEvent>> {
    let mut rows = connection
        .query(
            "SELECT id, camera_id, stream, source, kind, start_time_ms,
                    end_time_ms, confidence, bbox_json, zone, thumbnail_filename
             FROM recording_events
             WHERE id = ?1",
            turso::params![id],
        )
        .await?;
    rows.next()
        .await?
        .map(|row| event_from_row(&row))
        .transpose()
}

async fn catalog_stats(connection: &turso::Connection) -> anyhow::Result<CatalogStats> {
    let mut rows = connection
        .query(
            "SELECT
                 (SELECT COUNT(*) FROM recording_files),
                 (SELECT COUNT(*) FROM recording_files WHERE finalized = 1),
                 (SELECT COUNT(*) FROM recording_files WHERE finalized = 0),
                 (SELECT COUNT(*) FROM recording_fragments),
                 (SELECT COALESCE(SUM(byte_len), 0) FROM recording_fragments),
                 (SELECT COUNT(*) FROM recording_events),
                 (SELECT COUNT(*) FROM recording_events WHERE end_time_ms IS NULL),
                 (SELECT COUNT(*) FROM recording_events WHERE thumbnail_filename IS NOT NULL)",
            turso::params![],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("catalog statistics query returned no row"))?;
    Ok(CatalogStats {
        recording_files: to_u64(row.get(0)?, "recording file count")?,
        finalized_files: to_u64(row.get(1)?, "finalized recording count")?,
        active_files: to_u64(row.get(2)?, "active recording count")?,
        fragments: to_u64(row.get(3)?, "fragment count")?,
        fragment_bytes: to_u64(row.get(4)?, "fragment bytes")?,
        events: to_u64(row.get(5)?, "event count")?,
        open_events: to_u64(row.get(6)?, "open event count")?,
        event_thumbnails: to_u64(row.get(7)?, "event thumbnail count")?,
    })
}

fn event_from_row(row: &turso::Row) -> anyhow::Result<TimelineEvent> {
    let source = row.get::<String>(3)?;
    let source = EventSource::parse(&source)
        .ok_or_else(|| anyhow::anyhow!("unknown event source '{source}'"))?;
    let bbox_json = row.get::<Option<String>>(8)?;
    let bbox = bbox_json.as_deref().map(serde_json::from_str).transpose()?;
    Ok(TimelineEvent {
        id: row.get(0)?,
        camera_id: row.get(1)?,
        stream: row.get(2)?,
        source,
        kind: row.get(4)?,
        start_time_ms: row.get(5)?,
        end_time_ms: row.get(6)?,
        confidence: row.get(7)?,
        bbox,
        zone: row.get(9)?,
        thumbnail_filename: row.get(10)?,
    })
}

fn to_i64(value: u64, name: &str) -> anyhow::Result<i64> {
    i64::try_from(value).map_err(|_| anyhow::anyhow!("{name} exceeds Turso INTEGER range"))
}

fn to_u64(value: i64, name: &str) -> anyhow::Result<u64> {
    u64::try_from(value).map_err(|_| anyhow::anyhow!("{name} is negative in the Turso catalog"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-output")
            .join(name);
        if path.exists() {
            std::fs::remove_dir_all(&path).unwrap();
        }
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn catalog_persists_and_queries_fragments_by_stream_time() {
        let root = test_dir("turso-catalog");
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front-door/main".to_owned(),
                started_at_ms: 1_000,
                ended_at_ms: None,
                path: "front-door/main/recording-1.mp4".to_owned(),
                init_offset: 0,
                init_len: 512,
                finalized: false,
            })
            .unwrap();
        handle
            .insert_fragment(CatalogFragment {
                recording_id: "recording-1".to_owned(),
                sequence: 1,
                start_ms: 2_000,
                duration_ms: 2_000,
                byte_offset: 512,
                byte_len: 8_192,
                random_access: true,
            })
            .unwrap();

        let fragments = handle
            .fragments_in_range("front-door/main", 2_500, 3_000)
            .unwrap();
        assert_eq!(
            fragments,
            vec![CatalogFragment {
                recording_id: "recording-1".to_owned(),
                sequence: 1,
                start_ms: 2_000,
                duration_ms: 2_000,
                byte_offset: 512,
                byte_len: 8_192,
                random_access: true,
            }]
        );
        assert!(
            handle
                .fragments_in_range("back-yard/main", 2_500, 3_000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            handle.stats().unwrap(),
            CatalogStats {
                recording_files: 1,
                finalized_files: 0,
                active_files: 1,
                fragments: 1,
                fragment_bytes: 8_192,
                events: 0,
                open_events: 0,
                event_thumbnails: 0,
            }
        );

        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_tracks_event_lifecycle_and_time_overlap() {
        let root = test_dir("turso-events");
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        handle
            .insert_event(TimelineEvent {
                id: "event-1".to_owned(),
                camera_id: "front-door".to_owned(),
                stream: Some("sub".to_owned()),
                source: EventSource::Camera,
                kind: "motion".to_owned(),
                start_time_ms: 2_000,
                end_time_ms: None,
                confidence: None,
                bbox: None,
                zone: None,
                thumbnail_filename: None,
            })
            .unwrap();
        handle.close_event("event-1", 4_000).unwrap();
        handle
            .attach_event_thumbnail("event-1", "event-1.jpg")
            .unwrap();

        let events = handle.events_in_range("front-door", 2_500, 3_000).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].end_time_ms, Some(4_000));
        assert_eq!(events[0].thumbnail_filename.as_deref(), Some("event-1.jpg"));
        assert_eq!(
            handle.event_by_id("event-1").unwrap(),
            events.first().cloned()
        );
        assert!(
            handle
                .events_in_range("front-door", 4_000, 5_000)
                .unwrap()
                .is_empty()
        );
        assert!(
            handle
                .events_in_range("back-yard", 2_500, 3_000)
                .unwrap()
                .is_empty()
        );
        handle.detach_event_thumbnail("event-1").unwrap();
        assert_eq!(
            handle
                .event_by_id("event-1")
                .unwrap()
                .and_then(|event| event.thumbnail_filename),
            None
        );

        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
}
