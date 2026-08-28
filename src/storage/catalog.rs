use crate::{
    operational_events::{
        OperationalEvent, OperationalEventKey, OperationalEventKind, OperationalEvidence,
        OperationalSeverity,
    },
    storage::{
        metadata::{
            EventAttachment, EventSource, TimelineEvent, canonical_event_attachment, event_icon,
        },
        search::{
            EventEmbedding, EventImageFilter, EventMetadataQuery, EventSearchHit, EventSearchPage,
            EventSearchTerm, EventSemanticSearchQuery, EventTextSearchQuery, normalize_search_text,
        },
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::Duration,
};

const COMMAND_CAPACITY: usize = 256;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SEARCH_PAGE_TOKEN_BYTES: usize = 4_096;
const MAX_EVENT_ATTACHMENTS: usize = 32;
const MAX_ATTACHMENT_ID_BYTES: usize = 64;
const MAX_ATTACHMENT_TYPE_BYTES: usize = 64;
const MAX_CONTENT_TYPE_BYTES: usize = 128;
const MAX_ATTACHMENT_TEXT_CHARS: usize = 4_096;
#[cfg(not(test))]
const MAX_SEMANTIC_CANDIDATES: i64 = 10_000;
#[cfg(test)]
const MAX_SEMANTIC_CANDIDATES: i64 = 10;
const RESOLVE_EVENT_KEYFRAME_SQL: &str = "SELECT l.event_id, l.stream_id, e.start_time_ms,
                        l.recording_id, l.fragment_sequence, f.start_ms,
                        r.path, k.byte_offset, k.byte_len
         FROM recording_event_keyframes AS l
         JOIN recording_events AS e ON e.id = l.event_id
         JOIN recording_keyframes AS k
             ON k.recording_id = l.recording_id
            AND k.fragment_sequence = l.fragment_sequence
         JOIN recording_fragments AS f
             ON f.recording_id = l.recording_id
            AND f.sequence = l.fragment_sequence
         JOIN recording_files AS r ON r.id = l.recording_id
         WHERE l.event_id = ?1 AND l.stream_id = ?2";
const EVENT_SEARCH_COLUMNS: &str = "e.id, e.camera_id, e.stream, e.source, e.kind,
         e.start_time_ms, e.end_time_ms, e.confidence, e.bbox_json, e.zone,
         e.thumbnail_filename, e.revision, e.bbox_attachment_id, e.attachments_json,
         e.canonical_attachment_id, e.icon_key, e.rejected_icon_key,
         (SELECT t.display_value
          FROM recording_event_search_terms AS t
          WHERE t.event_id = e.id AND t.field = 'text'
          ORDER BY t.normalized_value
          LIMIT 1) AS text";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EventSearchCursor {
    Metadata {
        fingerprint: String,
        event_snapshot_rowid: i64,
        search_revision: i64,
        last_start_time_ms: i64,
        last_event_id: String,
    },
    Text {
        fingerprint: String,
        event_snapshot_rowid: i64,
        search_revision: i64,
        last_start_time_ms: i64,
        last_event_id: String,
    },
    Semantic {
        fingerprint: String,
        event_snapshot_rowid: i64,
        embedding_snapshot_rowid: i64,
        search_revision: i64,
        last_distance_bits: u64,
        last_start_time_ms: i64,
        last_event_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRecording {
    pub id: String,
    pub stream_id: String,
    pub source_id: Option<String>,
    pub logical_stream_id: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMediaFragment {
    pub recording_id: String,
    pub recording_started_at_ms: i64,
    pub path: String,
    pub init_offset: u64,
    pub init_len: u64,
    pub sequence: u64,
    pub start_ms: i64,
    pub duration_ms: u64,
    pub byte_offset: u64,
    pub byte_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogKeyframe {
    pub recording_id: String,
    pub fragment_sequence: u64,
    pub byte_offset: u64,
    pub byte_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEventKeyframeLink {
    pub event_id: String,
    pub stream_id: String,
    pub recording_id: String,
    pub fragment_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventKeyframeLocation {
    pub event_id: String,
    pub stream_id: String,
    pub event_time_ms: i64,
    pub recording_id: String,
    pub fragment_sequence: u64,
    pub fragment_start_ms: i64,
    pub path: String,
    pub byte_offset: u64,
    pub byte_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMediaObjectLocation {
    pub recording_id: String,
    pub fragment_sequence: u64,
    pub path: String,
    pub initialization_offset: u64,
    pub initialization_len: u64,
    pub fragment_offset: u64,
    pub fragment_len: u64,
    pub keyframe_offset: u64,
    pub keyframe_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogCleanupCandidate {
    pub recording_id: String,
    pub path: PathBuf,
    pub file_bytes: u64,
    pub pending: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct EventPreviewRequest {
    pub event_id: String,
    pub source_id: String,
    pub stream_id: String,
    pub recording_stream_id: String,
    pub event_time_ms: i64,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct EventPreviewResolution {
    pub event_id: String,
    pub keyframes: Vec<EventKeyframeLocation>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogStats {
    pub recording_files: u64,
    pub finalized_files: u64,
    pub active_files: u64,
    pub protected_files: u64,
    pub recording_bytes: u64,
    pub fragments: u64,
    pub fragment_bytes: u64,
    pub events: u64,
    pub open_events: u64,
    pub event_thumbnails: u64,
    pub oldest_recording_at_ms: Option<i64>,
    pub newest_recording_at_ms: Option<i64>,
}

#[derive(Clone)]
pub struct RecordingCatalogHandle {
    tx: SyncSender<Command>,
    search_tx: SyncSender<SearchCommand>,
}

pub struct RecordingCatalog {
    handle: RecordingCatalogHandle,
    thread: Option<JoinHandle<()>>,
    maintenance_shutdown: Arc<AtomicBool>,
    maintenance: Option<JoinHandle<()>>,
    search_thread: Option<JoinHandle<()>>,
}

struct LegacyRecording {
    id: String,
    path: PathBuf,
    finalized: bool,
    cleanup_pending: bool,
    needs_keyframe_backfill: bool,
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
    InsertFragmentWithKeyframe {
        fragment: CatalogFragment,
        keyframe: CatalogKeyframe,
        reply: SyncSender<anyhow::Result<()>>,
    },
    UpdateRecordingPath {
        recording_id: String,
        path: String,
        finalized: bool,
        reply: SyncSender<anyhow::Result<()>>,
    },
    FragmentsInRange {
        stream_id: String,
        start_ms: i64,
        end_ms: i64,
        reply: SyncSender<anyhow::Result<Vec<CatalogFragment>>>,
    },
    MediaFragmentsInRange {
        stream_id: String,
        start_ms: i64,
        end_ms: i64,
        reply: SyncSender<anyhow::Result<Vec<CatalogMediaFragment>>>,
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
        byte_len: u64,
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
    UpsertOperationalEvent {
        event: OperationalEvent,
        reply: SyncSender<anyhow::Result<()>>,
    },
    OperationalEventsInRange {
        camera_id: String,
        start_ms: i64,
        end_ms: i64,
        reply: SyncSender<anyhow::Result<Vec<OperationalEvent>>>,
    },
    OpenOperationalEvents {
        reply: SyncSender<anyhow::Result<Vec<OperationalEvent>>>,
    },
    LinkEventKeyframe {
        link: CatalogEventKeyframeLink,
        reply: SyncSender<anyhow::Result<()>>,
    },
    ResolveEventKeyframe {
        event_id: String,
        stream_id: String,
        reply: SyncSender<anyhow::Result<Option<EventKeyframeLocation>>>,
    },
    ResolveMediaObject {
        source_id: String,
        logical_stream_id: String,
        legacy_recording_stream_id: Option<String>,
        recording_id: String,
        fragment_sequence: u64,
        reply: SyncSender<anyhow::Result<Option<CatalogMediaObjectLocation>>>,
    },
    BackfillKeyframes {
        recording_id: String,
        keyframes: Vec<CatalogKeyframe>,
        reply: SyncSender<anyhow::Result<()>>,
    },
    BackfillRecordingIdentity {
        recording_stream_id: String,
        source_id: String,
        logical_stream_id: String,
        reply: SyncSender<anyhow::Result<()>>,
    },
    DeleteRecording {
        recording_id: String,
        reply: SyncSender<anyhow::Result<()>>,
    },
    DeleteRecordingsByPath {
        paths: Vec<String>,
        reply: SyncSender<anyhow::Result<()>>,
    },
    ClaimCleanupCandidate {
        reply: SyncSender<anyhow::Result<Option<CatalogCleanupCandidate>>>,
    },
    PendingCleanupCandidate {
        reply: SyncSender<anyhow::Result<Option<CatalogCleanupCandidate>>>,
    },
    CompleteCleanup {
        recording_id: String,
        reply: SyncSender<anyhow::Result<()>>,
    },
    CancelCleanup {
        recording_id: String,
        reply: SyncSender<anyhow::Result<()>>,
    },
    SetRecordingProtected {
        recording_id: String,
        protected: bool,
        reply: SyncSender<anyhow::Result<()>>,
    },
    ReplaceEventSearchTerms {
        event_id: String,
        terms: Vec<EventSearchTerm>,
        reply: SyncSender<anyhow::Result<()>>,
    },
    SetEventEmbedding {
        event_id: String,
        embedding: EventEmbedding,
        reply: SyncSender<anyhow::Result<()>>,
    },
    Stats {
        reply: SyncSender<anyhow::Result<CatalogStats>>,
    },
    Shutdown,
}

enum SearchCommand {
    Metadata {
        query: EventMetadataQuery,
        reply: SyncSender<anyhow::Result<EventSearchPage>>,
    },
    Text {
        query: EventTextSearchQuery,
        reply: SyncSender<anyhow::Result<EventSearchPage>>,
    },
    Semantic {
        query: EventSemanticSearchQuery,
        reply: SyncSender<anyhow::Result<EventSearchPage>>,
    },
    Availability {
        stream_id: String,
        start_ms: i64,
        end_ms: i64,
        bucket_ms: u64,
        reply: SyncSender<anyhow::Result<Vec<(i64, i64)>>>,
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
        let search_connection = database.connect()?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        search_connection.busy_timeout(BUSY_TIMEOUT)?;
        pollster::block_on(initialize_schema(&connection))?;
        let legacy_recordings =
            pollster::block_on(legacy_recordings_without_keyframes(&connection))?;
        pollster::block_on(backfill_recording_file_sizes(
            &connection,
            &legacy_recordings,
        ))?;

        let (tx, rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (search_tx, search_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let handle = RecordingCatalogHandle { tx, search_tx };
        let thread = std::thread::Builder::new()
            .name("recording-catalog".to_owned())
            .spawn(move || run_catalog(connection, rx))?;
        let search_thread = std::thread::Builder::new()
            .name("recording-catalog-search".to_owned())
            .spawn(move || run_search_catalog(search_connection, search_rx))?;
        let maintenance_shutdown = Arc::new(AtomicBool::new(false));
        let maintenance = (!legacy_recordings.is_empty())
            .then(|| {
                let handle = handle.clone();
                let shutdown = maintenance_shutdown.clone();
                std::thread::Builder::new()
                    .name("recording-catalog-backfill".to_owned())
                    .spawn(move || backfill_legacy_recordings(handle, legacy_recordings, shutdown))
            })
            .transpose()?;

        Ok(Self {
            handle,
            thread: Some(thread),
            maintenance_shutdown,
            maintenance,
            search_thread: Some(search_thread),
        })
    }

    pub fn handle(&self) -> RecordingCatalogHandle {
        self.handle.clone()
    }

    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    pub fn wait_for_maintenance(&mut self) {
        if let Some(maintenance) = self.maintenance.take()
            && maintenance.join().is_err()
        {
            tracing::error!("recording catalog backfill thread panicked");
        }
    }

    fn shutdown_inner(&mut self) {
        self.maintenance_shutdown.store(true, Ordering::Release);
        self.wait_for_maintenance();
        let _ = self.handle.search_tx.send(SearchCommand::Shutdown);
        if let Some(search_thread) = self.search_thread.take()
            && search_thread.join().is_err()
        {
            tracing::error!("recording catalog search thread panicked");
        }
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

pub(crate) fn rewrite_recording_paths(
    catalog_path: &Path,
    routes: &[(PathBuf, PathBuf)],
) -> anyhow::Result<()> {
    if routes.is_empty() || !catalog_path.exists() {
        return Ok(());
    }
    let path = catalog_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Turso catalog path is not valid UTF-8"))?;
    let database = pollster::block_on(turso::Builder::new_local(path).build())?;
    let connection = database.connect()?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    pollster::block_on(async {
        initialize_schema(&connection).await?;
        let mut rows = connection
            .query("SELECT id, path, finalized FROM recording_files", ())
            .await?;
        let mut rewrites = Vec::new();
        while let Some(row) = rows.next().await? {
            let recording_id = row.get::<String>(0)?;
            let current_path = PathBuf::from(row.get::<String>(1)?);
            let finalized = row.get::<i64>(2)? != 0;
            let destination = routes
                .iter()
                .filter_map(|(from, to)| {
                    current_path
                        .strip_prefix(from)
                        .ok()
                        .map(|relative| (from.components().count(), to.join(relative)))
                })
                .max_by_key(|(specificity, _)| *specificity)
                .map(|(_, destination)| destination);
            if let Some(destination) = destination
                && destination != current_path
            {
                rewrites.push((recording_id, destination, finalized));
            }
        }
        drop(rows);
        if rewrites.is_empty() {
            return Ok(());
        }
        connection.execute_batch("BEGIN IMMEDIATE").await?;
        let result = async {
            for (recording_id, destination, finalized) in rewrites {
                let destination = destination
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("migrated recording path is not valid UTF-8"))?;
                let file_bytes = if finalized {
                    std::fs::metadata(destination).map_or(0, |metadata| metadata.len())
                } else {
                    0
                };
                connection
                    .execute(
                        "UPDATE recording_files
                         SET path = ?1, file_bytes = ?2
                         WHERE id = ?3",
                        turso::params![
                            destination,
                            to_i64(file_bytes, "migrated recording file bytes")?,
                            recording_id
                        ],
                    )
                    .await?;
            }
            anyhow::Ok(())
        }
        .await;
        match result {
            Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK").await;
                Err(error)
            }
        }
    })
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

    pub fn insert_fragment_with_keyframe(
        &self,
        fragment: CatalogFragment,
        keyframe: CatalogKeyframe,
    ) -> anyhow::Result<()> {
        let same_recording = fragment.recording_id == keyframe.recording_id;
        let same_sequence = fragment.sequence == keyframe.fragment_sequence;
        if !(same_recording && same_sequence) {
            anyhow::bail!("keyframe must belong to the inserted fragment");
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::InsertFragmentWithKeyframe {
                fragment,
                keyframe,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn update_recording_path(
        &self,
        recording_id: &str,
        path: &Path,
        finalized: bool,
    ) -> anyhow::Result<()> {
        let path = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("recording path is not valid UTF-8"))?;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::UpdateRecordingPath {
                recording_id: recording_id.to_owned(),
                path: path.to_owned(),
                finalized,
                reply,
            })
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

    pub fn media_fragments_in_range(
        &self,
        stream_id: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> anyhow::Result<Vec<CatalogMediaFragment>> {
        if start_ms >= end_ms {
            anyhow::bail!("media fragment query start must be before end");
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::MediaFragmentsInRange {
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

    pub(crate) fn availability_ranges_in_range(
        &self,
        stream_id: &str,
        start_ms: i64,
        end_ms: i64,
        bucket_ms: u64,
    ) -> anyhow::Result<Vec<(i64, i64)>> {
        if start_ms >= end_ms || bucket_ms == 0 {
            anyhow::bail!("availability range and bucket must be positive");
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.search_tx
            .send(SearchCommand::Availability {
                stream_id: stream_id.to_owned(),
                start_ms,
                end_ms,
                bucket_ms,
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

    pub fn attach_event_thumbnail(
        &self,
        id: &str,
        thumbnail_filename: &str,
        byte_len: u64,
    ) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::AttachEventThumbnail {
                id: id.to_owned(),
                thumbnail_filename: thumbnail_filename.to_owned(),
                byte_len,
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

    pub(crate) fn upsert_operational_event(&self, event: OperationalEvent) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::UpsertOperationalEvent { event, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn operational_events_in_range(
        &self,
        camera_id: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> anyhow::Result<Vec<OperationalEvent>> {
        if start_ms >= end_ms {
            anyhow::bail!("operational event query start must be before end");
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::OperationalEventsInRange {
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

    pub(crate) fn open_operational_events(&self) -> anyhow::Result<Vec<OperationalEvent>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::OpenOperationalEvents { reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn link_event_keyframe(&self, link: CatalogEventKeyframeLink) -> anyhow::Result<()> {
        if link.event_id.is_empty() || link.stream_id.is_empty() {
            anyhow::bail!("event and stream identifiers must not be empty");
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::LinkEventKeyframe { link, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub fn resolve_event_keyframe(
        &self,
        event_id: &str,
        stream_id: &str,
    ) -> anyhow::Result<Option<EventKeyframeLocation>> {
        if event_id.is_empty() || stream_id.is_empty() {
            anyhow::bail!("event and stream identifiers must not be empty");
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::ResolveEventKeyframe {
                event_id: event_id.to_owned(),
                stream_id: stream_id.to_owned(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn resolve_media_object(
        &self,
        source_id: &str,
        logical_stream_id: &str,
        legacy_recording_stream_id: Option<&str>,
        recording_id: &str,
        fragment_sequence: u64,
    ) -> anyhow::Result<Option<CatalogMediaObjectLocation>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::ResolveMediaObject {
                source_id: source_id.to_owned(),
                logical_stream_id: logical_stream_id.to_owned(),
                legacy_recording_stream_id: legacy_recording_stream_id.map(str::to_owned),
                recording_id: recording_id.to_owned(),
                fragment_sequence,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    fn backfill_keyframes(
        &self,
        recording_id: &str,
        keyframes: Vec<CatalogKeyframe>,
    ) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::BackfillKeyframes {
                recording_id: recording_id.to_owned(),
                keyframes,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn backfill_recording_identity(
        &self,
        recording_stream_id: &str,
        source_id: &str,
        logical_stream_id: &str,
    ) -> anyhow::Result<()> {
        if recording_stream_id.is_empty() || source_id.is_empty() || logical_stream_id.is_empty() {
            anyhow::bail!("recording identity fields must not be empty");
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::BackfillRecordingIdentity {
                recording_stream_id: recording_stream_id.to_owned(),
                source_id: source_id.to_owned(),
                logical_stream_id: logical_stream_id.to_owned(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    fn delete_recording(&self, recording_id: &str) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::DeleteRecording {
                recording_id: recording_id.to_owned(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn delete_recordings_by_path(&self, paths: &[PathBuf]) -> anyhow::Result<()> {
        let paths = paths
            .iter()
            .map(|path| {
                path.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("recording path is not valid UTF-8"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::DeleteRecordingsByPath { paths, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn claim_cleanup_candidate(
        &self,
    ) -> anyhow::Result<Option<CatalogCleanupCandidate>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::ClaimCleanupCandidate { reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn pending_cleanup_candidate(
        &self,
    ) -> anyhow::Result<Option<CatalogCleanupCandidate>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::PendingCleanupCandidate { reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn complete_cleanup(&self, recording_id: &str) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::CompleteCleanup {
                recording_id: recording_id.to_owned(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn cancel_cleanup(&self, recording_id: &str) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::CancelCleanup {
                recording_id: recording_id.to_owned(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    /// Includes or excludes a finalized recording from automatic storage cleanup.
    pub fn set_recording_protected(
        &self,
        recording_id: &str,
        protected: bool,
    ) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::SetRecordingProtected {
                recording_id: recording_id.to_owned(),
                protected,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn replace_event_search_terms(
        &self,
        event_id: &str,
        terms: Vec<EventSearchTerm>,
    ) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::ReplaceEventSearchTerms {
                event_id: event_id.to_owned(),
                terms,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn set_event_embedding(
        &self,
        event_id: &str,
        embedding: EventEmbedding,
    ) -> anyhow::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .send(Command::SetEventEmbedding {
                event_id: event_id.to_owned(),
                embedding,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn search_event_metadata(
        &self,
        query: EventMetadataQuery,
    ) -> anyhow::Result<EventSearchPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.search_tx
            .send(SearchCommand::Metadata { query, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn search_event_text(
        &self,
        query: EventTextSearchQuery,
    ) -> anyhow::Result<EventSearchPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.search_tx
            .send(SearchCommand::Text { query, reply })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("recording catalog stopped before replying"))?
    }

    pub(crate) fn search_event_semantic(
        &self,
        query: EventSemanticSearchQuery,
    ) -> anyhow::Result<EventSearchPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.search_tx
            .send(SearchCommand::Semantic { query, reply })
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
            Command::InsertFragmentWithKeyframe {
                fragment,
                keyframe,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(insert_fragment_with_keyframe(
                    &connection,
                    fragment,
                    keyframe,
                )));
            }
            Command::UpdateRecordingPath {
                recording_id,
                path,
                finalized,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(update_recording_path(
                    &connection,
                    &recording_id,
                    &path,
                    finalized,
                )));
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
            Command::MediaFragmentsInRange {
                stream_id,
                start_ms,
                end_ms,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(media_fragments_in_range(
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
                byte_len,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(attach_event_thumbnail(
                    &connection,
                    &id,
                    &thumbnail_filename,
                    byte_len,
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
            Command::UpsertOperationalEvent { event, reply } => {
                let _ = reply.send(pollster::block_on(upsert_operational_event(
                    &connection,
                    event,
                )));
            }
            Command::OperationalEventsInRange {
                camera_id,
                start_ms,
                end_ms,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(operational_events_in_range(
                    &connection,
                    &camera_id,
                    start_ms,
                    end_ms,
                )));
            }
            Command::OpenOperationalEvents { reply } => {
                let _ = reply.send(pollster::block_on(open_operational_events(&connection)));
            }
            Command::LinkEventKeyframe { link, reply } => {
                let _ = reply.send(pollster::block_on(link_event_keyframe(&connection, link)));
            }
            Command::ResolveEventKeyframe {
                event_id,
                stream_id,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(resolve_event_keyframe(
                    &connection,
                    &event_id,
                    &stream_id,
                )));
            }
            Command::ResolveMediaObject {
                source_id,
                logical_stream_id,
                legacy_recording_stream_id,
                recording_id,
                fragment_sequence,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(resolve_media_object(
                    &connection,
                    &source_id,
                    &logical_stream_id,
                    legacy_recording_stream_id.as_deref(),
                    &recording_id,
                    fragment_sequence,
                )));
            }
            Command::BackfillKeyframes {
                recording_id,
                keyframes,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(insert_backfilled_keyframes(
                    &connection,
                    &recording_id,
                    keyframes,
                )));
            }
            Command::BackfillRecordingIdentity {
                recording_stream_id,
                source_id,
                logical_stream_id,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(backfill_recording_identity(
                    &connection,
                    &recording_stream_id,
                    &source_id,
                    &logical_stream_id,
                )));
            }
            Command::DeleteRecording {
                recording_id,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(delete_recording(
                    &connection,
                    &recording_id,
                )));
            }
            Command::DeleteRecordingsByPath { paths, reply } => {
                let _ = reply.send(pollster::block_on(delete_recordings_by_path(
                    &connection,
                    &paths,
                )));
            }
            Command::ClaimCleanupCandidate { reply } => {
                let _ = reply.send(pollster::block_on(claim_cleanup_candidate(&connection)));
            }
            Command::PendingCleanupCandidate { reply } => {
                let _ = reply.send(pollster::block_on(pending_cleanup_candidate(&connection)));
            }
            Command::CompleteCleanup {
                recording_id,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(complete_cleanup(
                    &connection,
                    &recording_id,
                )));
            }
            Command::CancelCleanup {
                recording_id,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(cancel_cleanup(
                    &connection,
                    &recording_id,
                )));
            }
            Command::SetRecordingProtected {
                recording_id,
                protected,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(set_recording_protected(
                    &connection,
                    &recording_id,
                    protected,
                )));
            }
            Command::ReplaceEventSearchTerms {
                event_id,
                terms,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(replace_event_search_terms(
                    &connection,
                    &event_id,
                    terms,
                )));
            }
            Command::SetEventEmbedding {
                event_id,
                embedding,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(set_event_embedding(
                    &connection,
                    &event_id,
                    embedding,
                )));
            }
            Command::Stats { reply } => {
                let _ = reply.send(pollster::block_on(catalog_stats(&connection)));
            }
            Command::Shutdown => break,
        }
    }
}

fn run_search_catalog(connection: turso::Connection, rx: Receiver<SearchCommand>) {
    while let Ok(command) = rx.recv() {
        match command {
            SearchCommand::Metadata { query, reply } => {
                let _ = reply.send(pollster::block_on(search_event_metadata(
                    &connection,
                    query,
                )));
            }
            SearchCommand::Text { query, reply } => {
                let _ = reply.send(pollster::block_on(search_event_text(&connection, query)));
            }
            SearchCommand::Semantic { query, reply } => {
                let _ = reply.send(pollster::block_on(search_event_semantic(
                    &connection,
                    query,
                )));
            }
            SearchCommand::Availability {
                stream_id,
                start_ms,
                end_ms,
                bucket_ms,
                reply,
            } => {
                let _ = reply.send(pollster::block_on(availability_ranges_in_range(
                    &connection,
                    &stream_id,
                    start_ms,
                    end_ms,
                    bucket_ms,
                )));
            }
            SearchCommand::Shutdown => break,
        }
    }
}

async fn legacy_recordings_without_keyframes(
    connection: &turso::Connection,
) -> anyhow::Result<Vec<LegacyRecording>> {
    let mut rows = connection
        .query(
                        "SELECT r.id, r.path, r.finalized, r.cleanup_pending,
                                        EXISTS (
                                                SELECT 1
                                                FROM recording_fragments AS f
                                                LEFT JOIN recording_keyframes AS k
                                                    ON k.recording_id = f.recording_id
                                                 AND k.fragment_sequence = f.sequence
                                                WHERE f.recording_id = r.id AND k.recording_id IS NULL
                                        )
             FROM recording_files AS r
             ORDER BY r.started_at_ms, r.id",
            (),
        )
        .await?;
    let mut recordings = Vec::new();
    while let Some(row) = rows.next().await? {
        recordings.push(LegacyRecording {
            id: row.get(0)?,
            path: PathBuf::from(row.get::<String>(1)?),
            finalized: row.get::<i64>(2)? != 0,
            cleanup_pending: row.get::<i64>(3)? != 0,
            needs_keyframe_backfill: row.get::<i64>(4)? != 0,
        });
    }
    Ok(recordings)
}

fn backfill_legacy_recordings(
    catalog: RecordingCatalogHandle,
    recordings: Vec<LegacyRecording>,
    shutdown: Arc<AtomicBool>,
) {
    for mut recording in recordings {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        if recording.cleanup_pending {
            continue;
        }
        if !recording.path.is_file() {
            let recovered = (!recording.finalized)
                .then(|| finalized_sibling(&recording.path))
                .flatten()
                .filter(|path| path.is_file());
            if let Some(path) = recovered {
                if let Err(error) = catalog.update_recording_path(&recording.id, &path, true) {
                    tracing::warn!(recording_id = recording.id, %error, "unable to recover finalized recording catalog path");
                    continue;
                }
                recording.path = path;
                recording.finalized = true;
            } else if recording.path.parent().is_some_and(Path::is_dir) {
                if let Err(error) = catalog.delete_recording(&recording.id) {
                    tracing::warn!(recording_id = recording.id, %error, "unable to remove stale recording catalog row");
                }
                continue;
            } else {
                tracing::warn!(
                    recording_id = recording.id,
                    path = %recording.path.display(),
                    "recording storage parent is unavailable; preserving catalog metadata",
                );
                continue;
            }
        }
        if !recording.finalized {
            continue;
        }
        if let Err(error) = catalog.update_recording_path(&recording.id, &recording.path, true) {
            tracing::warn!(recording_id = recording.id, %error, "unable to backfill recording file size");
            continue;
        }
        if !recording.needs_keyframe_backfill {
            continue;
        }
        let keyframes = match read_legacy_keyframes(&recording.path, &recording.id) {
            Ok(keyframes) => keyframes,
            Err(error) => {
                tracing::warn!(
                    recording_id = recording.id,
                    path = %recording.path.display(),
                    %error,
                    "unable to backfill legacy recording keyframes",
                );
                continue;
            }
        };
        if let Err(error) = catalog.backfill_keyframes(&recording.id, keyframes) {
            tracing::warn!(
                recording_id = recording.id,
                path = %recording.path.display(),
                %error,
                "unable to commit legacy recording keyframes",
            );
        }
    }
}

async fn backfill_recording_file_sizes(
    connection: &turso::Connection,
    recordings: &[LegacyRecording],
) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        for recording in recordings.iter().filter(|recording| recording.finalized) {
            let Ok(metadata) = std::fs::metadata(&recording.path) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            connection
                .execute(
                    "UPDATE recording_files SET file_bytes = ?1 WHERE id = ?2",
                    turso::params![
                        to_i64(metadata.len(), "recording file bytes")?,
                        recording.id.clone()
                    ],
                )
                .await?;
        }
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

fn finalized_sibling(active_path: &Path) -> Option<PathBuf> {
    let filename = active_path.file_name()?.to_str()?;
    filename
        .strip_suffix(".active")
        .map(|filename| active_path.with_file_name(filename))
}

fn read_legacy_keyframes(path: &Path, recording_id: &str) -> anyhow::Result<Vec<CatalogKeyframe>> {
    let reader = mp4::read_mp4(File::open(path)?)?;
    let (&track_id, _) = reader
        .tracks()
        .iter()
        .find(|(_, track)| {
            matches!(
                track.media_type(),
                Ok(mp4::MediaType::H264 | mp4::MediaType::H265)
            )
        })
        .ok_or_else(|| anyhow::anyhow!("recording has no supported video track"))?;
    Ok(reader
        .fragment_first_sample_locations(track_id)?
        .into_iter()
        .filter(|sample| sample.is_sync)
        .map(|sample| CatalogKeyframe {
            recording_id: recording_id.to_owned(),
            fragment_sequence: u64::from(sample.sequence_number),
            byte_offset: sample.location.offset,
            byte_len: u64::from(sample.location.size),
        })
        .collect())
}

async fn insert_backfilled_keyframes(
    connection: &turso::Connection,
    recording_id: &str,
    keyframes: Vec<CatalogKeyframe>,
) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        for keyframe in keyframes {
            if keyframe.recording_id != recording_id {
                anyhow::bail!("backfilled keyframe belongs to a different recording");
            }
            connection
                .execute(
                    "INSERT OR IGNORE INTO recording_keyframes (
                         recording_id, fragment_sequence, byte_offset, byte_len
                     ) VALUES (?1, ?2, ?3, ?4)",
                    turso::params![
                        recording_id,
                        to_i64(keyframe.fragment_sequence, "keyframe fragment sequence")?,
                        to_i64(keyframe.byte_offset, "keyframe byte offset")?,
                        to_i64(keyframe.byte_len, "keyframe byte length")?,
                    ],
                )
                .await?;
            reconcile_events_for_fragment(connection, recording_id, keyframe.fragment_sequence)
                .await?;
        }
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn backfill_recording_identity(
    connection: &turso::Connection,
    recording_stream_id: &str,
    source_id: &str,
    logical_stream_id: &str,
) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let changed = connection
            .execute(
                "UPDATE recording_files
                 SET source_id = COALESCE(source_id, ?2),
                     logical_stream_id = COALESCE(logical_stream_id, ?3)
                 WHERE stream_id = ?1
                                     AND (source_id IS NULL OR source_id = ?2)
                                     AND (logical_stream_id IS NULL OR logical_stream_id = ?3)
                   AND (source_id IS NULL OR logical_stream_id IS NULL)",
                turso::params![recording_stream_id, source_id, logical_stream_id],
            )
            .await?;
        if changed == 0 {
            return anyhow::Ok(());
        }
        let mut rows = connection
            .query(
                "SELECT r.id, k.fragment_sequence
                 FROM recording_files AS r
                 JOIN recording_keyframes AS k ON k.recording_id = r.id
                 JOIN recording_fragments AS f
                   ON f.recording_id = k.recording_id
                  AND f.sequence = k.fragment_sequence
                 WHERE r.stream_id = ?1
                   AND r.source_id = ?2
                   AND r.logical_stream_id = ?3
                 ORDER BY f.start_ms, r.started_at_ms, r.id, f.sequence",
                turso::params![recording_stream_id, source_id, logical_stream_id],
            )
            .await?;
        let mut fragments = Vec::new();
        while let Some(row) = rows.next().await? {
            fragments.push((
                row.get::<String>(0)?,
                to_u64(row.get(1)?, "identity backfill fragment sequence")?,
            ));
        }
        drop(rows);
        for (recording_id, fragment_sequence) in fragments {
            reconcile_events_for_fragment(connection, &recording_id, fragment_sequence).await?;
        }
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn delete_recording(
    connection: &turso::Connection,
    recording_id: &str,
) -> anyhow::Result<()> {
    connection
        .execute(
            "DELETE FROM recording_files WHERE id = ?1",
            turso::params![recording_id],
        )
        .await?;
    Ok(())
}

async fn delete_recordings_by_path(
    connection: &turso::Connection,
    paths: &[String],
) -> anyhow::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        for path in paths {
            connection
                .execute(
                    "DELETE FROM recording_files WHERE path = ?1",
                    turso::params![path.clone()],
                )
                .await?;
        }
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn claim_cleanup_candidate(
    connection: &turso::Connection,
) -> anyhow::Result<Option<CatalogCleanupCandidate>> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let mut rows = connection
            .query(
                "SELECT id, path, file_bytes, cleanup_pending
                 FROM recording_files
                 WHERE finalized = 1 AND protected = 0
                 ORDER BY cleanup_pending DESC, started_at_ms, id
                 LIMIT 1",
                (),
            )
            .await?;
        let candidate = rows
            .next()
            .await?
            .map(|row| {
                anyhow::Ok(CatalogCleanupCandidate {
                    recording_id: row.get(0)?,
                    path: PathBuf::from(row.get::<String>(1)?),
                    file_bytes: to_u64(row.get(2)?, "cleanup candidate file bytes")?,
                    pending: row.get::<i64>(3)? != 0,
                })
            })
            .transpose()?;
        drop(rows);
        if let Some(candidate) = &candidate
            && !candidate.pending
        {
            connection
                .execute(
                    "UPDATE recording_files SET cleanup_pending = 1 WHERE id = ?1",
                    turso::params![candidate.recording_id.clone()],
                )
                .await?;
        }
        anyhow::Ok(candidate)
    }
    .await;
    match result {
        Ok(candidate) => {
            connection.execute_batch("COMMIT").await?;
            Ok(candidate)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn pending_cleanup_candidate(
    connection: &turso::Connection,
) -> anyhow::Result<Option<CatalogCleanupCandidate>> {
    let mut rows = connection
        .query(
            "SELECT id, path, file_bytes
             FROM recording_files
             WHERE finalized = 1 AND protected = 0 AND cleanup_pending = 1
             ORDER BY started_at_ms, id
             LIMIT 1",
            (),
        )
        .await?;
    rows.next()
        .await?
        .map(|row| {
            anyhow::Ok(CatalogCleanupCandidate {
                recording_id: row.get(0)?,
                path: PathBuf::from(row.get::<String>(1)?),
                file_bytes: to_u64(row.get(2)?, "pending cleanup file bytes")?,
                pending: true,
            })
        })
        .transpose()
}

async fn complete_cleanup(
    connection: &turso::Connection,
    recording_id: &str,
) -> anyhow::Result<()> {
    connection
        .execute(
            "DELETE FROM recording_files WHERE id = ?1 AND cleanup_pending = 1",
            turso::params![recording_id],
        )
        .await?;
    Ok(())
}

async fn cancel_cleanup(connection: &turso::Connection, recording_id: &str) -> anyhow::Result<()> {
    connection
        .execute(
            "UPDATE recording_files SET cleanup_pending = 0 WHERE id = ?1",
            turso::params![recording_id],
        )
        .await?;
    Ok(())
}

async fn set_recording_protected(
    connection: &turso::Connection,
    recording_id: &str,
    protected: bool,
) -> anyhow::Result<()> {
    connection
        .execute(
            "UPDATE recording_files
             SET protected = ?1, cleanup_pending = CASE WHEN ?1 = 1 THEN 0 ELSE cleanup_pending END
             WHERE id = ?2",
            (i64::from(protected), recording_id),
        )
        .await?;
    Ok(())
}

pub(super) async fn initialize_schema(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS catalog_schema_migrations (
                 version INTEGER PRIMARY KEY,
                 applied_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS recording_event_search_state (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 revision INTEGER NOT NULL
             );
             INSERT OR IGNORE INTO recording_event_search_state (id, revision) VALUES (1, 0);
             CREATE TABLE IF NOT EXISTS recording_files (
                 id TEXT PRIMARY KEY,
                 stream_id TEXT NOT NULL,
                 source_id TEXT,
                 logical_stream_id TEXT,
                 started_at_ms INTEGER NOT NULL,
                 ended_at_ms INTEGER,
                 path TEXT NOT NULL UNIQUE,
                 init_offset INTEGER NOT NULL,
                 init_len INTEGER NOT NULL,
                 finalized INTEGER NOT NULL,
                 file_bytes INTEGER NOT NULL DEFAULT 0,
                 protected INTEGER NOT NULL DEFAULT 0,
                 cleanup_pending INTEGER NOT NULL DEFAULT 0
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
             CREATE INDEX IF NOT EXISTS recording_fragments_recording_time
                 ON recording_fragments(recording_id, start_ms);
             CREATE TABLE IF NOT EXISTS recording_keyframes (
                 recording_id TEXT NOT NULL,
                 fragment_sequence INTEGER NOT NULL,
                 byte_offset INTEGER NOT NULL,
                 byte_len INTEGER NOT NULL,
                 PRIMARY KEY(recording_id, fragment_sequence),
                 FOREIGN KEY(recording_id, fragment_sequence)
                     REFERENCES recording_fragments(recording_id, sequence) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS recording_events (
                 id TEXT PRIMARY KEY,
                 revision INTEGER NOT NULL DEFAULT 1,
                 camera_id TEXT NOT NULL,
                 stream TEXT,
                 source TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 start_time_ms INTEGER NOT NULL,
                 end_time_ms INTEGER,
                 confidence REAL,
                 bbox_json TEXT,
                 bbox_attachment_id TEXT,
                 zone TEXT,
                 attachments_json TEXT NOT NULL DEFAULT '[]',
                 canonical_attachment_id TEXT,
                 icon_key TEXT NOT NULL DEFAULT 'event',
                 rejected_icon_key TEXT,
                 thumbnail_filename TEXT,
                 search_revision INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS recording_events_camera_time
                 ON recording_events(camera_id, start_time_ms, end_time_ms);
             CREATE INDEX IF NOT EXISTS recording_events_time
                 ON recording_events(start_time_ms, id);
             CREATE TABLE IF NOT EXISTS operational_events (
                 id TEXT PRIMARY KEY,
                 camera_id TEXT NOT NULL,
                 stream_id TEXT,
                 kind TEXT NOT NULL,
                 severity TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 start_time_ms INTEGER NOT NULL,
                 end_time_ms INTEGER,
                 duration_ms INTEGER,
                 cause TEXT NOT NULL,
                 explanation TEXT NOT NULL,
                 affected_streams_json TEXT NOT NULL,
                 recording_interrupted INTEGER NOT NULL,
                 evidence_source TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS operational_events_camera_time
                 ON operational_events(camera_id, start_time_ms, end_time_ms);
             CREATE INDEX IF NOT EXISTS operational_events_open
                 ON operational_events(end_time_ms, camera_id, kind, stream_id);
             CREATE TABLE IF NOT EXISTS recording_event_keyframes (
                 event_id TEXT NOT NULL REFERENCES recording_events(id) ON DELETE CASCADE,
                 stream_id TEXT NOT NULL,
                 recording_id TEXT NOT NULL,
                 fragment_sequence INTEGER NOT NULL,
                 PRIMARY KEY(event_id, stream_id),
                 FOREIGN KEY(recording_id, fragment_sequence)
                     REFERENCES recording_keyframes(recording_id, fragment_sequence)
                     ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS recording_event_search_terms (
                 event_id TEXT NOT NULL REFERENCES recording_events(id) ON DELETE CASCADE,
                 field TEXT NOT NULL,
                 normalized_value TEXT NOT NULL,
                 display_value TEXT NOT NULL,
                 PRIMARY KEY(event_id, field, normalized_value)
             );
             CREATE INDEX IF NOT EXISTS recording_event_search_terms_lookup
                 ON recording_event_search_terms(normalized_value, field, event_id);
             CREATE TRIGGER IF NOT EXISTS recording_events_search_event_type
             AFTER INSERT ON recording_events
             BEGIN
                 INSERT OR IGNORE INTO recording_event_search_terms (
                     event_id, field, normalized_value, display_value
                 ) VALUES (NEW.id, 'event_type', lower(trim(NEW.kind)), NEW.kind);
             END;
             CREATE TABLE IF NOT EXISTS recording_event_embeddings (
                 event_id TEXT NOT NULL REFERENCES recording_events(id) ON DELETE CASCADE,
                 model_id TEXT NOT NULL,
                 dimensions INTEGER NOT NULL,
                 embedding BLOB NOT NULL,
                 PRIMARY KEY(event_id, model_id)
             );
             CREATE INDEX IF NOT EXISTS recording_event_embeddings_model
                 ON recording_event_embeddings(model_id, dimensions, event_id);",
        )
        .await?;
    ensure_column(connection, "recording_files", "source_id", "TEXT").await?;
    ensure_column(connection, "recording_files", "logical_stream_id", "TEXT").await?;
    ensure_column(
        connection,
        "recording_files",
        "file_bytes",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(
        connection,
        "recording_files",
        "protected",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(
        connection,
        "recording_files",
        "cleanup_pending",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(
        connection,
        "recording_events",
        "revision",
        "INTEGER NOT NULL DEFAULT 1",
    )
    .await?;
    ensure_column(connection, "recording_events", "bbox_attachment_id", "TEXT").await?;
    ensure_column(
        connection,
        "recording_events",
        "attachments_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )
    .await?;
    ensure_column(
        connection,
        "recording_events",
        "canonical_attachment_id",
        "TEXT",
    )
    .await?;
    let icon_key_added = ensure_column(
        connection,
        "recording_events",
        "icon_key",
        "TEXT NOT NULL DEFAULT 'event'",
    )
    .await?;
    ensure_column(connection, "recording_events", "rejected_icon_key", "TEXT").await?;
    ensure_column(
        connection,
        "recording_events",
        "search_revision",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    connection
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS recording_files_source_stream_time
                 ON recording_files(source_id, logical_stream_id, started_at_ms);
             CREATE INDEX IF NOT EXISTS recording_files_cleanup
                 ON recording_files(finalized, protected, cleanup_pending, started_at_ms);",
        )
        .await?;
    backfill_event_presentation(connection, icon_key_added).await?;
    apply_event_search_backfill(connection).await?;
    Ok(())
}

async fn ensure_column(
    connection: &turso::Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> anyhow::Result<bool> {
    let mut rows = connection
        .query(format!("PRAGMA table_info({table})"), ())
        .await?;
    while let Some(row) = rows.next().await? {
        if row.get::<String>(1)? == column {
            return Ok(false);
        }
    }
    connection
        .execute_batch(format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))
        .await?;
    Ok(true)
}

async fn backfill_event_presentation(
    connection: &turso::Connection,
    icon_key_added: bool,
) -> anyhow::Result<()> {
    connection
        .execute(
            "UPDATE recording_events
             SET attachments_json = printf(
                     '[{\"id\":\"thumbnail\",\"attachment_type\":\"thumbnail\",\"content_type\":\"image/jpeg\",\"byte_len\":null,\"ordinal\":0,\"timestamp_ms\":%lld,\"text\":null}]',
                     start_time_ms
                 ),
                 canonical_attachment_id = 'thumbnail',
                 bbox_attachment_id = CASE
                     WHEN bbox_json IS NOT NULL THEN 'thumbnail'
                     ELSE NULL
                 END
             WHERE thumbnail_filename IS NOT NULL
               AND canonical_attachment_id IS NULL
               AND attachments_json = '[]'",
            (),
        )
        .await?;
    if icon_key_added {
        connection
            .execute(
                "UPDATE recording_events
                 SET icon_key = CASE lower(trim(kind))
                     WHEN 'person' THEN 'person'
                     WHEN 'human' THEN 'person'
                     WHEN 'face' THEN 'person'
                     WHEN 'vehicle' THEN 'vehicle'
                     WHEN 'car' THEN 'vehicle'
                     WHEN 'truck' THEN 'vehicle'
                     WHEN 'animal' THEN 'animal'
                     WHEN 'pet' THEN 'animal'
                     WHEN 'package' THEN 'package'
                     WHEN 'motion' THEN 'motion'
                     WHEN 'doorbell' THEN 'doorbell'
                     WHEN 'sound' THEN 'sound'
                     WHEN 'audio' THEN 'sound'
                     WHEN 'story' THEN 'story'
                     ELSE CASE
                         WHEN lower(kind) LIKE '%outage%'
                           OR lower(kind) LIKE '%unavailable%' THEN 'alert'
                         ELSE 'event'
                     END
                 END",
                (),
            )
            .await?;
    }
    Ok(())
}

async fn apply_event_search_backfill(connection: &turso::Connection) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let applied = connection
            .query(
                "SELECT 1 FROM catalog_schema_migrations WHERE version = 2",
                (),
            )
            .await?
            .next()
            .await?
            .is_some();
        if !applied {
            connection
                .execute(
                    "INSERT OR IGNORE INTO recording_event_search_terms (
                         event_id, field, normalized_value, display_value
                     )
                     SELECT id, 'event_type', lower(trim(kind)), kind
                     FROM recording_events",
                    (),
                )
                .await?;
            connection
                .execute(
                    "INSERT INTO catalog_schema_migrations (version, applied_at_ms)
                     VALUES (2, unixepoch('subsec') * 1000)",
                    (),
                )
                .await?;
        }
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn upsert_recording(
    connection: &turso::Connection,
    recording: CatalogRecording,
) -> anyhow::Result<()> {
    connection
        .execute(
            "INSERT INTO recording_files (
                 id, stream_id, source_id, logical_stream_id, started_at_ms,
                 ended_at_ms, path, init_offset, init_len, finalized
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                 stream_id = excluded.stream_id,
                 source_id = excluded.source_id,
                 logical_stream_id = excluded.logical_stream_id,
                 started_at_ms = excluded.started_at_ms,
                 ended_at_ms = excluded.ended_at_ms,
                 path = excluded.path,
                 init_offset = excluded.init_offset,
                 init_len = excluded.init_len,
                 finalized = excluded.finalized",
            turso::params![
                recording.id,
                recording.stream_id,
                recording.source_id,
                recording.logical_stream_id,
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

async fn insert_fragment_with_keyframe(
    connection: &turso::Connection,
    fragment: CatalogFragment,
    keyframe: CatalogKeyframe,
) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        insert_fragment(connection, fragment).await?;
        connection
            .execute(
                "INSERT INTO recording_keyframes (
                     recording_id, fragment_sequence, byte_offset, byte_len
                 ) VALUES (?1, ?2, ?3, ?4)",
                turso::params![
                    keyframe.recording_id.clone(),
                    to_i64(keyframe.fragment_sequence, "keyframe fragment sequence")?,
                    to_i64(keyframe.byte_offset, "keyframe byte offset")?,
                    to_i64(keyframe.byte_len, "keyframe byte length")?,
                ],
            )
            .await?;
        reconcile_events_for_fragment(
            connection,
            &keyframe.recording_id,
            keyframe.fragment_sequence,
        )
        .await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn update_recording_path(
    connection: &turso::Connection,
    recording_id: &str,
    path: &str,
    finalized: bool,
) -> anyhow::Result<()> {
    let file_bytes = if finalized {
        std::fs::metadata(path).map_or(0, |metadata| metadata.len())
    } else {
        0
    };
    connection
        .execute(
            "UPDATE recording_files
             SET path = ?1, finalized = ?2, file_bytes = ?3
             WHERE id = ?4",
            (
                path,
                i64::from(finalized),
                to_i64(file_bytes, "recording file bytes")?,
                recording_id,
            ),
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

async fn media_fragments_in_range(
    connection: &turso::Connection,
    stream_id: &str,
    start_ms: i64,
    end_ms: i64,
) -> anyhow::Result<Vec<CatalogMediaFragment>> {
    let mut rows = connection
        .query(
            "SELECT f.recording_id, r.started_at_ms, r.path, r.init_offset, r.init_len,
                    f.sequence, f.start_ms, f.duration_ms, f.byte_offset, f.byte_len
             FROM recording_fragments AS f
             JOIN recording_files AS r ON r.id = f.recording_id
             WHERE r.stream_id = ?1
               AND f.start_ms < ?3
               AND f.start_ms + f.duration_ms > ?2
             ORDER BY f.start_ms, f.sequence",
            (stream_id, start_ms, end_ms),
        )
        .await?;
    let mut fragments = Vec::new();
    while let Some(row) = rows.next().await? {
        fragments.push(CatalogMediaFragment {
            recording_id: row.get(0)?,
            recording_started_at_ms: row.get(1)?,
            path: row.get(2)?,
            init_offset: to_u64(row.get(3)?, "initialization offset")?,
            init_len: to_u64(row.get(4)?, "initialization length")?,
            sequence: to_u64(row.get(5)?, "fragment sequence")?,
            start_ms: row.get(6)?,
            duration_ms: to_u64(row.get(7)?, "fragment duration")?,
            byte_offset: to_u64(row.get(8)?, "fragment byte offset")?,
            byte_len: to_u64(row.get(9)?, "fragment byte length")?,
        });
    }
    Ok(fragments)
}

async fn availability_ranges_in_range(
    connection: &turso::Connection,
    stream_id: &str,
    start_ms: i64,
    end_ms: i64,
    bucket_ms: u64,
) -> anyhow::Result<Vec<(i64, i64)>> {
    let bucket_ms = to_i64(bucket_ms, "availability bucket")?;
    let sql = if bucket_ms >= 86_400_000 {
        "SELECT bucket_start, MAX(bucket_end)
         FROM (
             SELECT MAX(?2, (r.started_at_ms / ?4) * ?4) AS bucket_start,
                    MIN(?3, ((COALESCE(r.ended_at_ms, ?3) + ?4 - 1) / ?4) * ?4)
                        AS bucket_end
             FROM recording_files AS r
             WHERE r.stream_id = ?1
               AND r.started_at_ms < ?3
               AND COALESCE(r.ended_at_ms, ?3) > ?2
         )
         GROUP BY bucket_start
         ORDER BY bucket_start"
    } else {
        "SELECT bucket_start, MAX(bucket_end)
         FROM (
             SELECT MAX(?2, (f.start_ms / ?4) * ?4) AS bucket_start,
                    MIN(?3, ((f.start_ms + f.duration_ms + ?4 - 1) / ?4) * ?4)
                        AS bucket_end
             FROM recording_fragments AS f
             JOIN recording_files AS r ON r.id = f.recording_id
             WHERE r.stream_id = ?1
               AND r.started_at_ms < ?3
               AND COALESCE(r.ended_at_ms, ?3) > ?2
               AND f.start_ms < ?3
               AND f.start_ms + f.duration_ms > ?2
         )
         GROUP BY bucket_start
         ORDER BY bucket_start"
    };
    let mut rows = connection
        .query(sql, (stream_id, start_ms, end_ms, bucket_ms))
        .await?;
    let mut ranges = Vec::new();
    while let Some(row) = rows.next().await? {
        ranges.push((row.get(0)?, row.get(1)?));
    }
    Ok(ranges)
}

async fn insert_event(connection: &turso::Connection, event: TimelineEvent) -> anyhow::Result<()> {
    let mut event = event;
    normalize_event_presentation(&mut event)?;
    if event.kind.is_empty() {
        anyhow::bail!("event kind must not be empty");
    }
    if event.revision == 0 {
        anyhow::bail!("event revision must be greater than zero");
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
    let attachments_json = serde_json::to_string(&event.attachments)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let existing = event_by_id(connection, &event.id).await?;
        if let Some(existing) = &existing {
            if event.revision <= existing.revision {
                anyhow::bail!(
                    "event revision {} does not exceed stored revision {}",
                    event.revision,
                    existing.revision
                );
            }
            if event.camera_id != existing.camera_id || event.source != existing.source {
                anyhow::bail!("event revision cannot change source identity");
            }
            connection
                .execute(
                    "UPDATE recording_events
                     SET camera_id = ?2, stream = ?3, source = ?4, kind = ?5,
                         start_time_ms = ?6, end_time_ms = ?7, confidence = ?8,
                         bbox_json = ?9, zone = ?10, thumbnail_filename = ?11,
                         revision = ?12, bbox_attachment_id = ?13,
                         attachments_json = ?14, canonical_attachment_id = ?15,
                         icon_key = ?16, rejected_icon_key = ?17
                     WHERE id = ?1",
                    turso::params![
                        event.id.clone(),
                        event.camera_id.clone(),
                        event.stream.clone(),
                        event.source.as_str(),
                        event.kind.clone(),
                        event.start_time_ms,
                        event.end_time_ms,
                        event.confidence,
                        bbox_json.clone(),
                        event.zone.clone(),
                        event.thumbnail_filename.clone(),
                        to_i64(event.revision, "event revision")?,
                        event.bbox_attachment_id.clone(),
                        attachments_json.clone(),
                        event.canonical_attachment_id.clone(),
                        event.icon_key.clone(),
                        event.rejected_icon_key.clone(),
                    ],
                )
                .await?;
            connection
                .execute(
                    "DELETE FROM recording_event_search_terms
                     WHERE event_id = ?1 AND field = 'event_type'",
                    turso::params![event.id.clone()],
                )
                .await?;
            connection
                .execute(
                    "INSERT INTO recording_event_search_terms (
                         event_id, field, normalized_value, display_value
                     ) VALUES (?1, 'event_type', lower(trim(?2)), ?3)",
                    turso::params![event.id.clone(), event.kind.clone(), event.kind.clone()],
                )
                .await?;
            record_event_search_mutation(connection, &event.id).await?;
        } else {
            if event.revision != 1 {
                anyhow::bail!("new events must start at revision one");
            }
            connection
                .execute(
                    "INSERT INTO recording_events (
                     id, camera_id, stream, source, kind, start_time_ms,
                     end_time_ms, confidence, bbox_json, zone, thumbnail_filename,
                     revision, bbox_attachment_id, attachments_json,
                     canonical_attachment_id, icon_key, rejected_icon_key
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                     ?12, ?13, ?14, ?15, ?16, ?17
                 )",
                    turso::params![
                        event.id.clone(),
                        event.camera_id.clone(),
                        event.stream.clone(),
                        event.source.as_str(),
                        event.kind,
                        event.start_time_ms,
                        event.end_time_ms,
                        event.confidence,
                        bbox_json,
                        event.zone,
                        event.thumbnail_filename,
                        to_i64(event.revision, "event revision")?,
                        event.bbox_attachment_id,
                        attachments_json,
                        event.canonical_attachment_id,
                        event.icon_key,
                        event.rejected_icon_key,
                    ],
                )
                .await?;
        }
        reconcile_keyframes_for_event(
            connection,
            &event.id,
            &event.camera_id,
            event.stream.as_deref(),
            event.start_time_ms,
        )
        .await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn reconcile_keyframes_for_event(
    connection: &turso::Connection,
    event_id: &str,
    source_id: &str,
    stream_id: Option<&str>,
    event_time_ms: i64,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT r.logical_stream_id, f.recording_id, f.sequence
             FROM recording_files AS r
             JOIN recording_fragments AS f ON f.recording_id = r.id
             JOIN recording_keyframes AS k
               ON k.recording_id = f.recording_id
              AND k.fragment_sequence = f.sequence
             WHERE r.source_id = ?1
               AND r.logical_stream_id IS NOT NULL
               AND (?2 IS NULL OR r.logical_stream_id = ?3)
               AND f.start_ms <= ?4
               AND f.start_ms + f.duration_ms > ?4
             ORDER BY r.logical_stream_id, f.start_ms DESC, r.started_at_ms DESC, f.sequence DESC",
            turso::params![source_id, stream_id, stream_id, event_time_ms],
        )
        .await?;
    let mut linked_streams = std::collections::HashSet::new();
    while let Some(row) = rows.next().await? {
        let logical_stream_id = row.get::<String>(0)?;
        if !linked_streams.insert(logical_stream_id.clone()) {
            continue;
        }
        connection
            .execute(
                "INSERT INTO recording_event_keyframes (
                     event_id, stream_id, recording_id, fragment_sequence
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(event_id, stream_id) DO UPDATE SET
                     recording_id = excluded.recording_id,
                     fragment_sequence = excluded.fragment_sequence",
                turso::params![
                    event_id,
                    logical_stream_id,
                    row.get::<String>(1)?,
                    row.get::<i64>(2)?,
                ],
            )
            .await?;
    }
    Ok(())
}

async fn reconcile_events_for_fragment(
    connection: &turso::Connection,
    recording_id: &str,
    fragment_sequence: u64,
) -> anyhow::Result<()> {
    let mut fragment_rows = connection
        .query(
            "SELECT r.source_id, r.logical_stream_id, f.start_ms, f.duration_ms
             FROM recording_files AS r
             JOIN recording_fragments AS f ON f.recording_id = r.id
             WHERE r.id = ?1 AND f.sequence = ?2",
            turso::params![
                recording_id,
                to_i64(fragment_sequence, "fragment sequence")?,
            ],
        )
        .await?;
    let Some(fragment) = fragment_rows.next().await? else {
        return Ok(());
    };
    let (Some(source_id), Some(stream_id)) = (
        fragment.get::<Option<String>>(0)?,
        fragment.get::<Option<String>>(1)?,
    ) else {
        return Ok(());
    };
    let start_ms = fragment.get::<i64>(2)?;
    let duration_ms = fragment.get::<i64>(3)?;
    let mut events = connection
        .query(
            "SELECT id
             FROM recording_events
             WHERE camera_id = ?1
               AND (stream IS NULL OR stream = ?2)
               AND start_time_ms >= ?3
               AND start_time_ms < ?3 + ?4",
            turso::params![source_id, stream_id.clone(), start_ms, duration_ms],
        )
        .await?;
    while let Some(event) = events.next().await? {
        connection
            .execute(
                "INSERT INTO recording_event_keyframes (
                     event_id, stream_id, recording_id, fragment_sequence
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(event_id, stream_id) DO UPDATE SET
                     recording_id = excluded.recording_id,
                     fragment_sequence = excluded.fragment_sequence",
                turso::params![
                    event.get::<String>(0)?,
                    stream_id.clone(),
                    recording_id,
                    to_i64(fragment_sequence, "fragment sequence")?,
                ],
            )
            .await?;
    }
    Ok(())
}

async fn close_event(
    connection: &turso::Connection,
    id: &str,
    end_time_ms: i64,
) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let changed = connection
            .execute(
                "UPDATE recording_events
                 SET end_time_ms = ?2, revision = revision + 1
                 WHERE id = ?1 AND start_time_ms <= ?2",
                turso::params![id, end_time_ms],
            )
            .await?;
        if changed == 0 {
            anyhow::bail!("event was not found or its end precedes its start");
        }
        record_event_search_mutation(connection, id).await
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn attach_event_thumbnail(
    connection: &turso::Connection,
    id: &str,
    thumbnail_filename: &str,
    byte_len: u64,
) -> anyhow::Result<()> {
    if thumbnail_filename.is_empty() {
        anyhow::bail!("event thumbnail filename must not be empty");
    }
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let mut event = event_by_id(connection, id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("event was not found"))?;
        let descriptor = EventAttachment {
            id: "thumbnail".to_owned(),
            attachment_type: "thumbnail".to_owned(),
            content_type: "image/jpeg".to_owned(),
            byte_len: Some(byte_len),
            ordinal: 0,
            timestamp_ms: Some(event.start_time_ms),
            text: None,
        };
        if let Some(existing) = event
            .attachments
            .iter_mut()
            .find(|attachment| attachment.id == descriptor.id)
        {
            *existing = descriptor;
        } else {
            event.attachments.push(descriptor);
        }
        normalize_event_presentation(&mut event)?;
        if event.bbox.is_some() && event.bbox_attachment_id.is_none() {
            event.bbox_attachment_id = Some("thumbnail".to_owned());
        }
        let attachments_json = serde_json::to_string(&event.attachments)?;
        connection
            .execute(
                "UPDATE recording_events
                 SET thumbnail_filename = ?2,
                     attachments_json = ?3,
                     canonical_attachment_id = ?4,
                     bbox_attachment_id = ?5,
                     revision = revision + 1
                 WHERE id = ?1",
                turso::params![
                    id,
                    thumbnail_filename,
                    attachments_json,
                    event.canonical_attachment_id,
                    event.bbox_attachment_id,
                ],
            )
            .await?;
        record_event_search_mutation(connection, id).await
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn detach_event_thumbnail(connection: &turso::Connection, id: &str) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let changed = connection
            .execute(
                "UPDATE recording_events SET thumbnail_filename = NULL WHERE id = ?1",
                turso::params![id],
            )
            .await?;
        if changed == 0 {
            anyhow::bail!("event was not found");
        }
        record_event_search_mutation(connection, id).await
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

fn normalize_event_presentation(event: &mut TimelineEvent) -> anyhow::Result<()> {
    if event.attachments.len() > MAX_EVENT_ATTACHMENTS {
        anyhow::bail!("event attachment count exceeds {MAX_EVENT_ATTACHMENTS}");
    }
    for attachment in &event.attachments {
        validate_bounded_ascii(
            &attachment.id,
            "event attachment ID",
            MAX_ATTACHMENT_ID_BYTES,
        )?;
        validate_bounded_ascii(
            &attachment.attachment_type,
            "event attachment type",
            MAX_ATTACHMENT_TYPE_BYTES,
        )?;
        validate_bounded_ascii(
            &attachment.content_type,
            "event attachment content type",
            MAX_CONTENT_TYPE_BYTES,
        )?;
        if attachment
            .text
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAX_ATTACHMENT_TEXT_CHARS)
        {
            anyhow::bail!("event attachment text is too long");
        }
    }
    let canonical =
        canonical_event_attachment(&event.attachments, event.canonical_attachment_id.as_deref())?;
    event.canonical_attachment_id = canonical.map(|attachment| attachment.id.clone());
    if let Some(bbox_attachment_id) = &event.bbox_attachment_id
        && !event
            .attachments
            .iter()
            .any(|attachment| attachment.id == *bbox_attachment_id)
    {
        anyhow::bail!("bounding-box attachment was not found in the event revision");
    }
    let icon = event_icon(Some(&event.icon_key), &event.kind);
    event.icon_key = icon.key.to_owned();
    if icon.rejected.is_some() {
        event.rejected_icon_key = icon.rejected;
    }
    Ok(())
}

fn validate_bounded_ascii(value: &str, name: &str, max_bytes: usize) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        anyhow::bail!("{name} must contain 1 to {max_bytes} visible ASCII characters");
    }
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
                    end_time_ms, confidence, bbox_json, zone, thumbnail_filename,
                    revision, bbox_attachment_id, attachments_json,
                    canonical_attachment_id, icon_key, rejected_icon_key
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
                    end_time_ms, confidence, bbox_json, zone, thumbnail_filename,
                    revision, bbox_attachment_id, attachments_json,
                    canonical_attachment_id, icon_key, rejected_icon_key
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

async fn upsert_operational_event(
    connection: &turso::Connection,
    event: OperationalEvent,
) -> anyhow::Result<()> {
    if event.id.is_empty() || event.revision == 0 {
        anyhow::bail!("operational event identity and revision must be present");
    }
    if event
        .end_time_ms
        .is_some_and(|end_time_ms| end_time_ms < event.start_time_ms)
    {
        anyhow::bail!("operational event end must not precede its start");
    }
    let affected_streams_json = serde_json::to_string(&event.evidence.affected_streams)?;
    connection
        .execute(
            "INSERT INTO operational_events (
                 id, camera_id, stream_id, kind, severity, revision, start_time_ms,
                 end_time_ms, duration_ms, cause, explanation, affected_streams_json,
                 recording_interrupted, evidence_source
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
                 stream_id = excluded.stream_id,
                 kind = excluded.kind,
                 severity = excluded.severity,
                 revision = excluded.revision,
                 end_time_ms = excluded.end_time_ms,
                 duration_ms = excluded.duration_ms,
                 cause = excluded.cause,
                 explanation = excluded.explanation,
                 affected_streams_json = excluded.affected_streams_json,
                 recording_interrupted = excluded.recording_interrupted,
                 evidence_source = excluded.evidence_source
             WHERE excluded.revision > operational_events.revision",
            turso::params![
                event.id,
                event.key.camera_id,
                event.key.stream_id,
                event.key.kind.as_str(),
                event.severity.as_str(),
                to_i64(event.revision, "operational event revision")?,
                event.start_time_ms,
                event.end_time_ms,
                event
                    .duration_ms
                    .map(|value| to_i64(value, "operational event duration"))
                    .transpose()?,
                event.evidence.cause,
                event.evidence.explanation,
                affected_streams_json,
                i64::from(event.evidence.recording_interrupted),
                event.evidence.source,
            ],
        )
        .await?;
    Ok(())
}

const OPERATIONAL_EVENT_COLUMNS: &str = "id, camera_id, stream_id, kind, severity, revision,
    start_time_ms, end_time_ms, duration_ms, cause, explanation, affected_streams_json,
    recording_interrupted, evidence_source";

async fn operational_events_in_range(
    connection: &turso::Connection,
    camera_id: &str,
    start_ms: i64,
    end_ms: i64,
) -> anyhow::Result<Vec<OperationalEvent>> {
    let mut rows = connection
        .query(
            format!(
                "SELECT {OPERATIONAL_EVENT_COLUMNS}
                 FROM operational_events
                 WHERE camera_id = ?1
                   AND start_time_ms < ?3
                   AND COALESCE(end_time_ms, ?3) > ?2
                 ORDER BY start_time_ms, id"
            ),
            turso::params![camera_id, start_ms, end_ms],
        )
        .await?;
    let mut events = Vec::new();
    while let Some(row) = rows.next().await? {
        events.push(operational_event_from_row(&row)?);
    }
    Ok(events)
}

async fn open_operational_events(
    connection: &turso::Connection,
) -> anyhow::Result<Vec<OperationalEvent>> {
    let mut rows = connection
        .query(
            format!(
                "SELECT {OPERATIONAL_EVENT_COLUMNS}
                 FROM operational_events
                 WHERE end_time_ms IS NULL
                 ORDER BY start_time_ms, id"
            ),
            (),
        )
        .await?;
    let mut events = Vec::new();
    while let Some(row) = rows.next().await? {
        events.push(operational_event_from_row(&row)?);
    }
    Ok(events)
}

async fn link_event_keyframe(
    connection: &turso::Connection,
    link: CatalogEventKeyframeLink,
) -> anyhow::Result<()> {
    connection
        .execute(
            "INSERT INTO recording_event_keyframes (
                 event_id, stream_id, recording_id, fragment_sequence
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(event_id, stream_id) DO UPDATE SET
                 recording_id = excluded.recording_id,
                 fragment_sequence = excluded.fragment_sequence",
            turso::params![
                link.event_id,
                link.stream_id,
                link.recording_id,
                to_i64(link.fragment_sequence, "event keyframe fragment sequence")?,
            ],
        )
        .await?;
    Ok(())
}

async fn resolve_event_keyframe(
    connection: &turso::Connection,
    event_id: &str,
    stream_id: &str,
) -> anyhow::Result<Option<EventKeyframeLocation>> {
    let mut rows = connection
        .query(
            RESOLVE_EVENT_KEYFRAME_SQL,
            turso::params![event_id, stream_id],
        )
        .await?;
    rows.next()
        .await?
        .map(|row| {
            Ok(EventKeyframeLocation {
                event_id: row.get(0)?,
                stream_id: row.get(1)?,
                event_time_ms: row.get(2)?,
                recording_id: row.get(3)?,
                fragment_sequence: to_u64(row.get(4)?, "keyframe fragment sequence")?,
                fragment_start_ms: row.get(5)?,
                path: row.get(6)?,
                byte_offset: to_u64(row.get(7)?, "keyframe byte offset")?,
                byte_len: to_u64(row.get(8)?, "keyframe byte length")?,
            })
        })
        .transpose()
}

async fn replace_event_search_terms(
    connection: &turso::Connection,
    event_id: &str,
    terms: Vec<EventSearchTerm>,
) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let event_exists = connection
            .query(
                "SELECT 1 FROM recording_events WHERE id = ?1",
                turso::params![event_id],
            )
            .await?
            .next()
            .await?
            .is_some();
        if !event_exists {
            anyhow::bail!("event was not found");
        }
        connection
            .execute(
                "DELETE FROM recording_event_search_terms
                 WHERE event_id = ?1 AND field <> 'event_type'",
                turso::params![event_id],
            )
            .await?;
        for term in terms {
            let normalized = normalize_search_text(&term.value)?;
            connection
                .execute(
                    "INSERT INTO recording_event_search_terms (
                         event_id, field, normalized_value, display_value
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(event_id, field, normalized_value) DO UPDATE SET
                         display_value = excluded.display_value",
                    turso::params![event_id, term.field.as_str(), normalized, term.value,],
                )
                .await?;
        }
        record_event_search_mutation(connection, event_id).await
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn set_event_embedding(
    connection: &turso::Connection,
    event_id: &str,
    embedding: EventEmbedding,
) -> anyhow::Result<()> {
    let dimensions = i64::try_from(embedding.values.len())?;
    let encoded = serde_json::to_string(&embedding.values)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        connection
            .execute(
                "INSERT INTO recording_event_embeddings (
                     event_id, model_id, dimensions, embedding
                 ) VALUES (?1, ?2, ?3, vector32(?4))
                 ON CONFLICT(event_id, model_id) DO UPDATE SET
                     dimensions = excluded.dimensions,
                     embedding = excluded.embedding",
                turso::params![event_id, embedding.model_id, dimensions, encoded],
            )
            .await?;
        record_event_search_mutation(connection, event_id).await
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn record_event_search_mutation(
    connection: &turso::Connection,
    event_id: &str,
) -> anyhow::Result<()> {
    connection
        .execute(
            "UPDATE recording_event_search_state
             SET revision = revision + 1
             WHERE id = 1",
            (),
        )
        .await?;
    let changed = connection
        .execute(
            "UPDATE recording_events
             SET search_revision = (
                 SELECT revision FROM recording_event_search_state WHERE id = 1
             )
             WHERE id = ?1",
            turso::params![event_id],
        )
        .await?;
    if changed == 0 {
        anyhow::bail!("event was not found");
    }
    Ok(())
}

async fn search_event_metadata(
    connection: &turso::Connection,
    query: EventMetadataQuery,
) -> anyhow::Result<EventSearchPage> {
    let fingerprint = metadata_search_fingerprint(&query);
    let cursor = query
        .page_token
        .as_deref()
        .map(decode_search_cursor)
        .transpose()?;
    let (event_snapshot_rowid, search_revision, last_start_time_ms, last_event_id) = match cursor {
        Some(EventSearchCursor::Metadata {
            fingerprint: token_fingerprint,
            event_snapshot_rowid,
            search_revision,
            last_start_time_ms,
            last_event_id,
        }) if token_fingerprint == fingerprint => (
            event_snapshot_rowid,
            search_revision,
            Some(last_start_time_ms),
            Some(last_event_id),
        ),
        Some(_) => anyhow::bail!("event search page token does not match the metadata query"),
        None => (
            maximum_rowid(connection, "recording_events").await?,
            maximum_search_revision(connection).await?,
            None,
            None,
        ),
    };
    ensure_search_snapshot_current(connection, search_revision, event_snapshot_rowid).await?;

    let mut sql = format!(
        "SELECT {EVENT_SEARCH_COLUMNS}
         FROM recording_events AS e
         WHERE (e.stream IS NULL OR e.stream = ?)
           AND e.start_time_ms < ?
           AND COALESCE(e.end_time_ms, e.start_time_ms + 1) > ?
           AND e.rowid <= ?"
    );
    let mut params = vec![
        turso::Value::Text(query.stream_id.clone()),
        turso::Value::Integer(query.end_time_ms),
        turso::Value::Integer(query.start_time_ms),
        turso::Value::Integer(event_snapshot_rowid),
    ];
    append_text_filter(&mut sql, "e.camera_id", &query.source_ids, &mut params);
    append_text_filter(&mut sql, "e.id", &query.event_ids, &mut params);
    append_text_filter(&mut sql, "lower(e.kind)", &query.event_types, &mut params);
    let origins = query
        .origins
        .iter()
        .map(|origin| origin.as_str().to_owned())
        .collect::<Vec<_>>();
    append_text_filter(&mut sql, "e.source", &origins, &mut params);
    append_text_filter(&mut sql, "lower(e.zone)", &query.zones, &mut params);
    if let Some(confidence) = query.minimum_confidence {
        sql.push_str(" AND e.confidence >= ?");
        params.push(turso::Value::Real(confidence));
    }
    match query.image {
        EventImageFilter::Any => {}
        EventImageFilter::WithImage => sql.push_str(" AND e.canonical_attachment_id IS NOT NULL"),
        EventImageFilter::WithoutImage => sql.push_str(" AND e.canonical_attachment_id IS NULL"),
    }
    if let Some(text) = &query.text {
        sql.push_str(
            " AND EXISTS (
                 SELECT 1 FROM recording_event_search_terms AS t
                 WHERE t.event_id = e.id
                   AND t.normalized_value >= ? AND t.normalized_value < ?
             )",
        );
        params.push(turso::Value::Text(text.clone()));
        params.push(turso::Value::Text(format!("{text}\u{10ffff}")));
    }
    if let (Some(last_start_time_ms), Some(last_event_id)) = (last_start_time_ms, last_event_id) {
        sql.push_str(
            " AND (
                 e.start_time_ms < ?
                 OR (e.start_time_ms = ? AND e.id > ?)
             )",
        );
        params.push(turso::Value::Integer(last_start_time_ms));
        params.push(turso::Value::Integer(last_start_time_ms));
        params.push(turso::Value::Text(last_event_id));
    }
    sql.push_str(" ORDER BY e.start_time_ms DESC, e.id LIMIT ?");
    params.push(turso::Value::Integer(i64::from(query.page_size) + 1));

    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    let mut hits = Vec::with_capacity(query.page_size as usize);
    let mut has_more = false;
    while let Some(row) = rows.next().await? {
        if hits.len() == query.page_size as usize {
            has_more = true;
            break;
        }
        hits.push(event_search_hit(
            &row,
            None,
            query.preview_before_ms,
            query.preview_after_ms,
        )?);
    }
    if query.include_preview_keyframes {
        attach_default_previews(connection, &mut hits, &query.stream_id).await?;
    }
    ensure_search_snapshot_current(connection, search_revision, event_snapshot_rowid).await?;
    let next_page_token = has_more
        .then(|| {
            let last = hits
                .last()
                .ok_or_else(|| anyhow::anyhow!("event search page has no cursor row"))?;
            encode_search_cursor(&EventSearchCursor::Metadata {
                fingerprint,
                event_snapshot_rowid,
                search_revision,
                last_start_time_ms: last.start_time_ms,
                last_event_id: last.event_id.clone(),
            })
        })
        .transpose()?;
    Ok(EventSearchPage {
        hits,
        next_page_token,
        candidates_truncated: false,
    })
}

fn append_text_filter(
    sql: &mut String,
    column: &str,
    values: &[String],
    params: &mut Vec<turso::Value>,
) {
    if values.is_empty() {
        return;
    }
    sql.push_str(" AND ");
    sql.push_str(column);
    sql.push_str(" IN (");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push('?');
        params.push(turso::Value::Text(value.clone()));
    }
    sql.push(')');
}

async fn search_event_text(
    connection: &turso::Connection,
    query: EventTextSearchQuery,
) -> anyhow::Result<EventSearchPage> {
    let fingerprint = text_search_fingerprint(&query);
    let cursor = query
        .page_token
        .as_deref()
        .map(decode_search_cursor)
        .transpose()?;
    let (event_snapshot_rowid, search_revision, last_start_time_ms, last_event_id) = match cursor {
        Some(EventSearchCursor::Text {
            fingerprint: token_fingerprint,
            event_snapshot_rowid,
            search_revision,
            last_start_time_ms,
            last_event_id,
        }) if token_fingerprint == fingerprint => (
            event_snapshot_rowid,
            search_revision,
            Some(last_start_time_ms),
            Some(last_event_id),
        ),
        Some(_) => anyhow::bail!("event search page token does not match the text query"),
        None => {
            let search_revision = maximum_search_revision(connection).await?;
            (
                maximum_rowid(connection, "recording_events").await?,
                search_revision,
                None,
                None,
            )
        }
    };
    ensure_search_snapshot_current(connection, search_revision, event_snapshot_rowid).await?;
    let prefix_end = format!("{}\u{10ffff}", query.query);
    let field = query.field.map(|field| field.as_str().to_owned());
    let source_id = query.source_id;
    let sql = format!(
        "SELECT {EVENT_SEARCH_COLUMNS}
             FROM recording_event_search_terms AS t
             JOIN recording_events AS e ON e.id = t.event_id
             WHERE t.normalized_value >= ?1 AND t.normalized_value < ?2
               AND (?3 IS NULL OR t.field = ?4)
               AND (?5 IS NULL OR e.camera_id = ?6)
                   AND (e.stream IS NULL OR e.stream = ?7)
                   AND e.start_time_ms < ?9
                   AND COALESCE(e.end_time_ms, e.start_time_ms + 1) > ?8
                         AND e.rowid <= ?10
                             AND (
                             ?11 IS NULL
                             OR e.start_time_ms < ?12
                             OR (e.start_time_ms = ?13 AND e.id > ?14)
                             )
             GROUP BY e.id
             ORDER BY e.start_time_ms DESC, e.id
                     LIMIT ?15"
    );
    let mut rows = connection
        .query(
            sql,
            turso::params![
                query.query,
                prefix_end,
                field.clone(),
                field,
                source_id.clone(),
                source_id,
                query.stream_id.clone(),
                query.start_time_ms,
                query.end_time_ms,
                event_snapshot_rowid,
                last_start_time_ms,
                last_start_time_ms,
                last_start_time_ms,
                last_event_id,
                i64::from(query.page_size) + 1,
            ],
        )
        .await?;
    let mut hits = Vec::new();
    let mut has_more = false;
    while let Some(row) = rows.next().await? {
        if hits.len() == query.page_size as usize {
            has_more = true;
            break;
        }
        hits.push(event_search_hit(
            &row,
            None,
            query.preview_before_ms,
            query.preview_after_ms,
        )?);
    }
    attach_default_previews(connection, &mut hits, &query.stream_id).await?;
    ensure_search_snapshot_current(connection, search_revision, event_snapshot_rowid).await?;
    let next_page_token = has_more
        .then(|| {
            let last = hits
                .last()
                .ok_or_else(|| anyhow::anyhow!("event search page has no cursor row"))?;
            encode_search_cursor(&EventSearchCursor::Text {
                fingerprint,
                event_snapshot_rowid,
                search_revision,
                last_start_time_ms: last.start_time_ms,
                last_event_id: last.event_id.clone(),
            })
        })
        .transpose()?;
    Ok(EventSearchPage {
        hits,
        next_page_token,
        candidates_truncated: false,
    })
}

async fn search_event_semantic(
    connection: &turso::Connection,
    query: EventSemanticSearchQuery,
) -> anyhow::Result<EventSearchPage> {
    let fingerprint = semantic_search_fingerprint(&query);
    let cursor = query
        .page_token
        .as_deref()
        .map(decode_search_cursor)
        .transpose()?;
    let (
        event_snapshot_rowid,
        embedding_snapshot_rowid,
        search_revision,
        last_distance,
        last_start_time_ms,
        last_event_id,
    ) = match cursor {
        Some(EventSearchCursor::Semantic {
            fingerprint: token_fingerprint,
            event_snapshot_rowid,
            embedding_snapshot_rowid,
            search_revision,
            last_distance_bits,
            last_start_time_ms,
            last_event_id,
        }) if token_fingerprint == fingerprint => (
            event_snapshot_rowid,
            embedding_snapshot_rowid,
            search_revision,
            Some(f64::from_bits(last_distance_bits)),
            Some(last_start_time_ms),
            Some(last_event_id),
        ),
        Some(_) => anyhow::bail!("event search page token does not match the semantic query"),
        None => {
            let search_revision = maximum_search_revision(connection).await?;
            (
                maximum_rowid(connection, "recording_events").await?,
                maximum_rowid(connection, "recording_event_embeddings").await?,
                search_revision,
                None,
                None,
                None,
            )
        }
    };
    ensure_search_snapshot_current(connection, search_revision, event_snapshot_rowid).await?;
    let dimensions = i64::try_from(query.embedding.values.len())?;
    let encoded = serde_json::to_string(&query.embedding.values)?;
    let source_id = query.source_id;
    let candidates_truncated = semantic_candidates_truncated(
        connection,
        &query.embedding.model_id,
        dimensions,
        source_id.as_deref(),
        &query.stream_id,
        query.start_time_ms,
        query.end_time_ms,
        event_snapshot_rowid,
        embedding_snapshot_rowid,
    )
    .await?;
    let sql = format!(
        "WITH candidates AS (
                 SELECT {EVENT_SEARCH_COLUMNS}, s.embedding
                                 FROM recording_event_embeddings AS s
                                 JOIN recording_events AS e ON e.id = s.event_id
                                 WHERE s.model_id = ?2 AND s.dimensions = ?3
                                     AND (?4 IS NULL OR e.camera_id = ?5)
                                     AND (e.stream IS NULL OR e.stream = ?6)
                                     AND e.start_time_ms < ?8
                                     AND COALESCE(e.end_time_ms, e.start_time_ms + 1) > ?7
                                     AND e.rowid <= ?9
                                     AND s.rowid <= ?10
                                 ORDER BY e.start_time_ms DESC, e.id
                                 LIMIT {MAX_SEMANTIC_CANDIDATES}
                         ),
                         scored AS (
                                 SELECT id, camera_id, stream, source, kind, start_time_ms,
                                     end_time_ms, confidence, bbox_json, zone,
                                     thumbnail_filename, revision, bbox_attachment_id,
                                     attachments_json, canonical_attachment_id, icon_key,
                                     rejected_icon_key, text,
                                     vector_distance_cos(embedding, vector32(?1)) AS distance
                                 FROM candidates
                         )
                            SELECT id, camera_id, stream, source, kind, start_time_ms,
                                end_time_ms, confidence, bbox_json, zone,
                                thumbnail_filename, revision, bbox_attachment_id,
                                attachments_json, canonical_attachment_id, icon_key,
                                rejected_icon_key, text, distance
                         FROM scored
                            WHERE ?11 IS NULL
                                OR distance > ?12
                                OR (distance = ?13 AND start_time_ms < ?14)
                                OR (distance = ?15 AND start_time_ms = ?16 AND id > ?17)
                         ORDER BY distance, start_time_ms DESC, id
                            LIMIT ?18"
    );
    let mut rows = connection
        .query(
            sql,
            turso::params![
                encoded,
                query.embedding.model_id,
                dimensions,
                source_id.clone(),
                source_id,
                query.stream_id.clone(),
                query.start_time_ms,
                query.end_time_ms,
                event_snapshot_rowid,
                embedding_snapshot_rowid,
                last_distance,
                last_distance,
                last_distance,
                last_start_time_ms,
                last_distance,
                last_start_time_ms,
                last_event_id,
                i64::from(query.page_size) + 1,
            ],
        )
        .await?;
    let mut hits = Vec::new();
    let mut has_more = false;
    while let Some(row) = rows.next().await? {
        if hits.len() == query.page_size as usize {
            has_more = true;
            break;
        }
        let distance = row.get::<f64>(18)?;
        hits.push(event_search_hit(
            &row,
            Some(distance),
            query.preview_before_ms,
            query.preview_after_ms,
        )?);
    }
    attach_default_previews(connection, &mut hits, &query.stream_id).await?;
    ensure_search_snapshot_current(connection, search_revision, event_snapshot_rowid).await?;
    let next_page_token = has_more
        .then(|| {
            let last = hits
                .last()
                .ok_or_else(|| anyhow::anyhow!("event search page has no cursor row"))?;
            let distance = last
                .semantic_distance
                .ok_or_else(|| anyhow::anyhow!("semantic search result has no distance"))?;
            encode_search_cursor(&EventSearchCursor::Semantic {
                fingerprint,
                event_snapshot_rowid,
                embedding_snapshot_rowid,
                search_revision,
                last_distance_bits: distance.to_bits(),
                last_start_time_ms: last.start_time_ms,
                last_event_id: last.event_id.clone(),
            })
        })
        .transpose()?;
    Ok(EventSearchPage {
        hits,
        next_page_token,
        candidates_truncated,
    })
}

#[allow(clippy::too_many_arguments)]
async fn semantic_candidates_truncated(
    connection: &turso::Connection,
    model_id: &str,
    dimensions: i64,
    source_id: Option<&str>,
    stream_id: &str,
    start_time_ms: i64,
    end_time_ms: i64,
    event_snapshot_rowid: i64,
    embedding_snapshot_rowid: i64,
) -> anyhow::Result<bool> {
    let sql = format!(
        "SELECT COUNT(*)
             FROM (
                 SELECT 1
                 FROM recording_event_embeddings AS s
                 JOIN recording_events AS e ON e.id = s.event_id
                 WHERE s.model_id = ?1 AND s.dimensions = ?2
                   AND (?3 IS NULL OR e.camera_id = ?4)
                                     AND (e.stream IS NULL OR e.stream = ?5)
                                     AND e.start_time_ms < ?7
                                     AND COALESCE(e.end_time_ms, e.start_time_ms + 1) > ?6
                                     AND e.rowid <= ?8
                                     AND s.rowid <= ?9
                 LIMIT {}
             )",
        MAX_SEMANTIC_CANDIDATES + 1,
    );
    let mut rows = connection
        .query(
            sql,
            turso::params![
                model_id,
                dimensions,
                source_id,
                source_id,
                stream_id,
                start_time_ms,
                end_time_ms,
                event_snapshot_rowid,
                embedding_snapshot_rowid,
            ],
        )
        .await?;
    let count = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("semantic candidate count returned no row"))?
        .get::<i64>(0)?;
    Ok(count > MAX_SEMANTIC_CANDIDATES)
}

fn metadata_search_fingerprint(query: &EventMetadataQuery) -> String {
    let mut hasher = Sha256::new();
    update_search_fingerprint(&mut hasher, b"metadata");
    for event_id in &query.event_ids {
        update_search_fingerprint(&mut hasher, event_id.as_bytes());
    }
    update_search_fingerprint(&mut hasher, b"source_ids");
    for source_id in &query.source_ids {
        update_search_fingerprint(&mut hasher, source_id.as_bytes());
    }
    update_search_fingerprint(&mut hasher, b"event_types");
    for event_type in &query.event_types {
        update_search_fingerprint(&mut hasher, event_type.as_bytes());
    }
    update_search_fingerprint(&mut hasher, b"origins");
    for origin in &query.origins {
        update_search_fingerprint(&mut hasher, origin.as_str().as_bytes());
    }
    update_search_fingerprint(&mut hasher, b"zones");
    for zone in &query.zones {
        update_search_fingerprint(&mut hasher, zone.as_bytes());
    }
    update_search_fingerprint(
        &mut hasher,
        &query
            .minimum_confidence
            .map(f64::to_bits)
            .unwrap_or_default()
            .to_le_bytes(),
    );
    let image = match query.image {
        EventImageFilter::Any => b"any".as_slice(),
        EventImageFilter::WithImage => b"with".as_slice(),
        EventImageFilter::WithoutImage => b"without".as_slice(),
    };
    update_search_fingerprint(&mut hasher, image);
    update_search_fingerprint(
        &mut hasher,
        query.text.as_deref().unwrap_or_default().as_bytes(),
    );
    update_search_fingerprint(&mut hasher, query.stream_id.as_bytes());
    update_search_fingerprint(&mut hasher, &query.start_time_ms.to_le_bytes());
    update_search_fingerprint(&mut hasher, &query.end_time_ms.to_le_bytes());
    update_search_fingerprint(&mut hasher, &query.preview_before_ms.to_le_bytes());
    update_search_fingerprint(&mut hasher, &query.preview_after_ms.to_le_bytes());
    update_search_fingerprint(&mut hasher, &query.page_size.to_le_bytes());
    encode_lower_hex(hasher.finalize())
}

fn text_search_fingerprint(query: &EventTextSearchQuery) -> String {
    search_fingerprint(&[
        b"text",
        query.query.as_bytes(),
        query
            .field
            .map_or(b"".as_slice(), |field| field.as_str().as_bytes()),
        query.source_id.as_deref().unwrap_or_default().as_bytes(),
        query.stream_id.as_bytes(),
        &query.start_time_ms.to_le_bytes(),
        &query.end_time_ms.to_le_bytes(),
        &query.preview_before_ms.to_le_bytes(),
        &query.preview_after_ms.to_le_bytes(),
        &query.page_size.to_le_bytes(),
    ])
}

fn semantic_search_fingerprint(query: &EventSemanticSearchQuery) -> String {
    let embedding_bytes = query
        .embedding
        .values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    search_fingerprint(&[
        b"semantic",
        query.embedding.model_id.as_bytes(),
        &embedding_bytes,
        query.source_id.as_deref().unwrap_or_default().as_bytes(),
        query.stream_id.as_bytes(),
        &query.start_time_ms.to_le_bytes(),
        &query.end_time_ms.to_le_bytes(),
        &query.preview_before_ms.to_le_bytes(),
        &query.preview_after_ms.to_le_bytes(),
        &query.page_size.to_le_bytes(),
    ])
}

fn search_fingerprint(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        update_search_fingerprint(&mut hasher, part);
    }
    encode_lower_hex(hasher.finalize())
}

fn update_search_fingerprint(hasher: &mut Sha256, part: &[u8]) {
    hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(part);
}

fn encode_lower_hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn encode_search_cursor(cursor: &EventSearchCursor) -> anyhow::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor)?))
}

fn decode_search_cursor(token: &str) -> anyhow::Result<EventSearchCursor> {
    if token.is_empty() || token.len() > MAX_SEARCH_PAGE_TOKEN_BYTES {
        anyhow::bail!("event search page token has an invalid length");
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| anyhow::anyhow!("event search page token is invalid"))?;
    serde_json::from_slice(&decoded)
        .map_err(|_| anyhow::anyhow!("event search page token is invalid"))
}

async fn maximum_rowid(connection: &turso::Connection, table: &str) -> anyhow::Result<i64> {
    let sql = match table {
        "recording_events" => "SELECT COALESCE(MAX(rowid), 0) FROM recording_events",
        "recording_event_embeddings" => {
            "SELECT COALESCE(MAX(rowid), 0) FROM recording_event_embeddings"
        }
        _ => anyhow::bail!("unsupported event search snapshot table"),
    };
    let mut rows = connection.query(sql, ()).await?;
    rows.next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("event search snapshot query returned no row"))?
        .get(0)
        .map_err(Into::into)
}

async fn maximum_search_revision(connection: &turso::Connection) -> anyhow::Result<i64> {
    let mut rows = connection
        .query(
            "SELECT revision FROM recording_event_search_state WHERE id = 1",
            (),
        )
        .await?;
    rows.next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("event search revision query returned no row"))?
        .get(0)
        .map_err(Into::into)
}

async fn ensure_search_snapshot_current(
    connection: &turso::Connection,
    search_revision: i64,
    event_snapshot_rowid: i64,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT EXISTS (
                 SELECT 1 FROM recording_events
                 WHERE search_revision > ?1 AND rowid <= ?2
             )",
            turso::params![search_revision, event_snapshot_rowid],
        )
        .await?;
    let changed = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("event search revision check returned no row"))?
        .get::<i64>(0)?
        != 0;
    if changed {
        anyhow::bail!("event search snapshot changed; restart the query");
    }
    Ok(())
}

fn event_search_hit(
    row: &turso::Row,
    semantic_distance: Option<f64>,
    preview_before_ms: u64,
    preview_after_ms: u64,
) -> anyhow::Result<EventSearchHit> {
    let event = event_from_row(row)?;
    let text = row.get(17)?;
    let canonical_attachment = event.canonical_attachment().cloned();
    let attachments = event.attachments.clone();
    let image_available = event.canonical_image_available();
    let preview_start_ms = event
        .start_time_ms
        .saturating_sub(i64::try_from(preview_before_ms).unwrap_or(i64::MAX));
    let requested_end_ms = event
        .end_time_ms
        .unwrap_or(event.start_time_ms)
        .saturating_add(i64::try_from(preview_after_ms).unwrap_or(i64::MAX));
    let preview_end_ms = requested_end_ms.min(preview_start_ms.saturating_add(60_000));
    Ok(EventSearchHit {
        event_id: event.id,
        revision: event.revision,
        source_id: event.camera_id,
        event_type: event.kind,
        origin: event.source,
        start_time_ms: event.start_time_ms,
        end_time_ms: event.end_time_ms,
        confidence: event.confidence,
        bbox: event.bbox,
        zone: event.zone,
        text,
        has_image_attachment: canonical_attachment.is_some(),
        canonical_attachment,
        attachments,
        image_available,
        icon_key: event.icon_key,
        rejected_icon_key: event.rejected_icon_key,
        bbox_attachment_id: event.bbox_attachment_id,
        score: semantic_distance.map(|distance| 1.0 - distance),
        semantic_distance,
        preview_start_ms,
        preview_end_ms,
        keyframes: Vec::new(),
        keyframes_truncated: requested_end_ms > preview_end_ms,
    })
}

async fn resolve_event_preview_batch(
    connection: &turso::Connection,
    requests: Vec<EventPreviewRequest>,
) -> anyhow::Result<Vec<EventPreviewResolution>> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let mut values = String::new();
    let mut params = Vec::<turso::Value>::with_capacity(requests.len() * 8);
    for (index, request) in requests.iter().enumerate() {
        if index > 0 {
            values.push(',');
        }
        values.push_str("(?, ?, ?, ?, ?, ?, ?, ?)");
        params.extend([
            turso::Value::Integer(i64::try_from(index)?),
            turso::Value::Text(request.event_id.clone()),
            turso::Value::Text(request.source_id.clone()),
            turso::Value::Text(request.stream_id.clone()),
            turso::Value::Text(request.recording_stream_id.clone()),
            turso::Value::Integer(request.event_time_ms),
            turso::Value::Integer(request.start_time_ms),
            turso::Value::Integer(request.end_time_ms),
        ]);
    }
    let sql = format!(
        "WITH requested(
                         request_index, event_id, source_id, stream_id, recording_stream_id,
                         event_time_ms, start_time_ms, end_time_ms
                 ) AS (VALUES {values}),
                 candidates AS (
                         SELECT q.request_index, q.event_id, q.stream_id, q.event_time_ms,
                                        r.id AS recording_id, f.sequence, f.start_ms, r.path,
                                        k.byte_offset, k.byte_len
                         FROM requested AS q
                         JOIN recording_files AS r
                             ON r.source_id = q.source_id
                            AND r.logical_stream_id = q.stream_id
                         JOIN recording_fragments AS f ON f.recording_id = r.id
                         JOIN recording_keyframes AS k
                             ON k.recording_id = f.recording_id
                            AND k.fragment_sequence = f.sequence
                         WHERE f.start_ms < q.end_time_ms
                             AND f.start_ms + f.duration_ms > q.start_time_ms
                         UNION ALL
                         SELECT q.request_index, q.event_id, q.stream_id, q.event_time_ms,
                                        r.id AS recording_id, f.sequence, f.start_ms, r.path,
                                        k.byte_offset, k.byte_len
                         FROM requested AS q
                         JOIN recording_files AS r
                             ON r.source_id IS NULL
                            AND r.stream_id = q.recording_stream_id
                         JOIN recording_fragments AS f ON f.recording_id = r.id
                         JOIN recording_keyframes AS k
                             ON k.recording_id = f.recording_id
                            AND k.fragment_sequence = f.sequence
                         WHERE f.start_ms < q.end_time_ms
                             AND f.start_ms + f.duration_ms > q.start_time_ms
                 ),
                 ranked AS (
                         SELECT candidates.*,
                                        ROW_NUMBER() OVER (
                                                PARTITION BY request_index
                                                ORDER BY start_ms, recording_id, sequence
                                        ) AS rank
                         FROM candidates
                 )
         SELECT request_index, event_id, stream_id, event_time_ms,
                recording_id, sequence, start_ms, path, byte_offset, byte_len, rank
         FROM ranked
         WHERE rank <= 65
         ORDER BY request_index, rank"
    );
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    let mut resolutions = requests
        .into_iter()
        .map(|request| EventPreviewResolution {
            event_id: request.event_id,
            keyframes: Vec::new(),
            truncated: false,
        })
        .collect::<Vec<_>>();
    while let Some(row) = rows.next().await? {
        let request_index = usize::try_from(row.get::<i64>(0)?)?;
        let resolution = resolutions
            .get_mut(request_index)
            .ok_or_else(|| anyhow::anyhow!("preview query returned an invalid request index"))?;
        let rank = row.get::<i64>(10)?;
        if rank > 64 {
            resolution.truncated = true;
            continue;
        }
        resolution.keyframes.push(EventKeyframeLocation {
            event_id: row.get(1)?,
            stream_id: row.get(2)?,
            event_time_ms: row.get(3)?,
            recording_id: row.get(4)?,
            fragment_sequence: to_u64(row.get(5)?, "preview fragment sequence")?,
            fragment_start_ms: row.get(6)?,
            path: row.get(7)?,
            byte_offset: to_u64(row.get(8)?, "preview keyframe byte offset")?,
            byte_len: to_u64(row.get(9)?, "preview keyframe byte length")?,
        });
    }
    Ok(resolutions)
}

async fn attach_default_previews(
    connection: &turso::Connection,
    hits: &mut [EventSearchHit],
    stream_id: &str,
) -> anyhow::Result<()> {
    let requests = hits
        .iter()
        .map(|hit| EventPreviewRequest {
            event_id: hit.event_id.clone(),
            source_id: hit.source_id.clone(),
            stream_id: stream_id.to_owned(),
            recording_stream_id: format!("{}/{stream_id}", hit.source_id),
            event_time_ms: hit.start_time_ms,
            start_time_ms: hit.preview_start_ms,
            end_time_ms: hit.preview_end_ms,
        })
        .collect();
    for (hit, resolution) in hits
        .iter_mut()
        .zip(resolve_event_preview_batch(connection, requests).await?)
    {
        if hit.event_id != resolution.event_id {
            anyhow::bail!("event preview resolution order changed");
        }
        hit.keyframes = resolution.keyframes;
        hit.keyframes_truncated |= resolution.truncated;
    }
    Ok(())
}

async fn resolve_media_object(
    connection: &turso::Connection,
    source_id: &str,
    logical_stream_id: &str,
    legacy_recording_stream_id: Option<&str>,
    recording_id: &str,
    fragment_sequence: u64,
) -> anyhow::Result<Option<CatalogMediaObjectLocation>> {
    let mut rows = connection
        .query(
            "SELECT r.id, f.sequence, r.path, r.init_offset, r.init_len,
                    f.byte_offset, f.byte_len, k.byte_offset, k.byte_len
             FROM recording_files AS r
             JOIN recording_fragments AS f ON f.recording_id = r.id
             JOIN recording_keyframes AS k
               ON k.recording_id = f.recording_id
              AND k.fragment_sequence = f.sequence
             WHERE r.id = ?1 AND f.sequence = ?2
               AND (
                   (r.source_id = ?3 AND r.logical_stream_id = ?4)
                   OR (
                       r.source_id IS NULL
                       AND (r.logical_stream_id IS NULL OR r.logical_stream_id = ?4)
                       AND ?5 IS NOT NULL
                       AND r.stream_id = ?6
                   )
               )",
            turso::params![
                recording_id,
                to_i64(fragment_sequence, "media object fragment sequence")?,
                source_id,
                logical_stream_id,
                legacy_recording_stream_id,
                legacy_recording_stream_id,
            ],
        )
        .await?;
    rows.next()
        .await?
        .map(|row| {
            Ok(CatalogMediaObjectLocation {
                recording_id: row.get(0)?,
                fragment_sequence: to_u64(row.get(1)?, "media object fragment sequence")?,
                path: row.get(2)?,
                initialization_offset: to_u64(row.get(3)?, "initialization byte offset")?,
                initialization_len: to_u64(row.get(4)?, "initialization byte length")?,
                fragment_offset: to_u64(row.get(5)?, "fragment byte offset")?,
                fragment_len: to_u64(row.get(6)?, "fragment byte length")?,
                keyframe_offset: to_u64(row.get(7)?, "keyframe byte offset")?,
                keyframe_len: to_u64(row.get(8)?, "keyframe byte length")?,
            })
        })
        .transpose()
}

async fn catalog_stats(connection: &turso::Connection) -> anyhow::Result<CatalogStats> {
    let mut rows = connection
        .query(
            "SELECT
                 (SELECT COUNT(*) FROM recording_files),
                 (SELECT COUNT(*) FROM recording_files WHERE finalized = 1),
                 (SELECT COUNT(*) FROM recording_files WHERE finalized = 0),
                 (SELECT COUNT(*) FROM recording_files WHERE protected = 1),
                 (SELECT COALESCE(SUM(file_bytes), 0) FROM recording_files WHERE finalized = 1),
                 (SELECT COUNT(*) FROM recording_fragments),
                 (SELECT COALESCE(SUM(byte_len), 0) FROM recording_fragments),
                 (SELECT COUNT(*) FROM recording_events),
                 (SELECT COUNT(*) FROM recording_events WHERE end_time_ms IS NULL),
                 (SELECT COUNT(*) FROM recording_events WHERE thumbnail_filename IS NOT NULL),
                 (SELECT MIN(start_ms) FROM recording_fragments),
                 (SELECT MAX(start_ms + duration_ms) FROM recording_fragments)",
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
        protected_files: to_u64(row.get(3)?, "protected recording count")?,
        recording_bytes: to_u64(row.get(4)?, "recording bytes")?,
        fragments: to_u64(row.get(5)?, "fragment count")?,
        fragment_bytes: to_u64(row.get(6)?, "fragment bytes")?,
        events: to_u64(row.get(7)?, "event count")?,
        open_events: to_u64(row.get(8)?, "open event count")?,
        event_thumbnails: to_u64(row.get(9)?, "event thumbnail count")?,
        oldest_recording_at_ms: row.get(10)?,
        newest_recording_at_ms: row.get(11)?,
    })
}

fn event_from_row(row: &turso::Row) -> anyhow::Result<TimelineEvent> {
    let source = row.get::<String>(3)?;
    let source = EventSource::parse(&source)
        .ok_or_else(|| anyhow::anyhow!("unknown event source '{source}'"))?;
    let bbox_json = row.get::<Option<String>>(8)?;
    let bbox = bbox_json.as_deref().map(serde_json::from_str).transpose()?;
    let attachments_json = row.get::<String>(13)?;
    let attachments: Vec<EventAttachment> = serde_json::from_str(&attachments_json)?;
    Ok(TimelineEvent {
        id: row.get(0)?,
        revision: to_u64(row.get(11)?, "event revision")?,
        camera_id: row.get(1)?,
        stream: row.get(2)?,
        source,
        kind: row.get(4)?,
        start_time_ms: row.get(5)?,
        end_time_ms: row.get(6)?,
        confidence: row.get(7)?,
        bbox,
        bbox_attachment_id: row.get(12)?,
        zone: row.get(9)?,
        attachments,
        canonical_attachment_id: row.get(14)?,
        icon_key: row.get(15)?,
        rejected_icon_key: row.get(16)?,
        thumbnail_filename: row.get(10)?,
    })
}

fn operational_event_from_row(row: &turso::Row) -> anyhow::Result<OperationalEvent> {
    let kind = row.get::<String>(3)?;
    let kind = OperationalEventKind::parse(&kind)
        .ok_or_else(|| anyhow::anyhow!("unknown operational event kind '{kind}'"))?;
    let severity = row.get::<String>(4)?;
    let severity = OperationalSeverity::parse(&severity)
        .ok_or_else(|| anyhow::anyhow!("unknown operational event severity '{severity}'"))?;
    let duration_ms = row
        .get::<Option<i64>>(8)?
        .map(|value| to_u64(value, "operational event duration"))
        .transpose()?;
    let affected_streams = serde_json::from_str(&row.get::<String>(11)?)?;
    Ok(OperationalEvent {
        id: row.get(0)?,
        key: OperationalEventKey {
            camera_id: row.get(1)?,
            stream_id: row.get(2)?,
            kind,
        },
        severity,
        revision: to_u64(row.get(5)?, "operational event revision")?,
        start_time_ms: row.get(6)?,
        end_time_ms: row.get(7)?,
        duration_ms,
        evidence: OperationalEvidence {
            cause: row.get(9)?,
            explanation: row.get(10)?,
            affected_streams,
            recording_interrupted: row.get::<i64>(12)? != 0,
            source: row.get(13)?,
        },
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
    use bytes::Bytes;
    use std::io::BufWriter;

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
                source_id: Some("front-door".to_owned()),
                logical_stream_id: Some("main".to_owned()),
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
                protected_files: 0,
                recording_bytes: 0,
                fragments: 1,
                fragment_bytes: 8_192,
                events: 0,
                open_events: 0,
                event_thumbnails: 0,
                oldest_recording_at_ms: Some(2_000),
                newest_recording_at_ms: Some(4_000),
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
                revision: 1,
                camera_id: "front-door".to_owned(),
                stream: Some("sub".to_owned()),
                source: EventSource::Camera,
                kind: "motion".to_owned(),
                start_time_ms: 2_000,
                end_time_ms: None,
                confidence: None,
                bbox: None,
                bbox_attachment_id: None,
                zone: None,
                attachments: Vec::new(),
                canonical_attachment_id: None,
                icon_key: "motion".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: None,
            })
            .unwrap();
        handle.close_event("event-1", 4_000).unwrap();
        handle
            .attach_event_thumbnail("event-1", "event-1.jpg", 12)
            .unwrap();

        let events = handle.events_in_range("front-door", 2_500, 3_000).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].end_time_ms, Some(4_000));
        assert_eq!(events[0].revision, 3);
        assert_eq!(
            events[0].canonical_attachment_id.as_deref(),
            Some("thumbnail")
        );
        assert_eq!(events[0].attachments.len(), 1);
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

    #[test]
    fn event_revisions_persist_and_replace_canonical_presentation() {
        let root = test_dir("turso-event-presentation-revisions");
        let database_path = root.join("recordings.db");
        let catalog = RecordingCatalog::open(&database_path).unwrap();
        let handle = catalog.handle();
        let mut event = test_event("event-1", 2_000);
        event.source = EventSource::KeepPeek;
        event.bbox = Some([0.1, 0.2, 0.3, 0.4]);
        event.attachments = vec![
            EventAttachment {
                id: "snapshot-later".to_owned(),
                attachment_type: "snapshot".to_owned(),
                content_type: "image/jpeg".to_owned(),
                byte_len: Some(20),
                ordinal: 4,
                timestamp_ms: Some(2_040),
                text: None,
            },
            EventAttachment {
                id: "story-explicit".to_owned(),
                attachment_type: "story-frame".to_owned(),
                content_type: "image/webp".to_owned(),
                byte_len: Some(10),
                ordinal: 0,
                timestamp_ms: Some(2_010),
                text: Some("first evidence".to_owned()),
            },
        ];
        event.canonical_attachment_id = Some("story-explicit".to_owned());
        event.bbox_attachment_id = Some("story-explicit".to_owned());
        event.icon_key = "<svg onload=alert(1)>".to_owned();
        handle.insert_event(event).unwrap();
        drop(handle);
        catalog.shutdown();

        let catalog = RecordingCatalog::open(&database_path).unwrap();
        let handle = catalog.handle();
        let persisted = handle.event_by_id("event-1").unwrap().unwrap();
        assert_eq!(persisted.revision, 1);
        assert_eq!(
            persisted.canonical_attachment_id.as_deref(),
            Some("story-explicit")
        );
        assert_eq!(persisted.icon_key, "motion");
        assert_eq!(
            persisted.rejected_icon_key.as_deref(),
            Some("<svg?onload=alert(1)>")
        );
        assert!(persisted.canonical_image_owns_bbox());
        assert_eq!(
            persisted
                .attachments
                .iter()
                .map(|attachment| attachment.id.as_str())
                .collect::<Vec<_>>(),
            ["snapshot-later", "story-explicit"]
        );

        let mut revision_two = persisted;
        revision_two.revision = 2;
        revision_two.attachments.reverse();
        revision_two.attachments.push(EventAttachment {
            id: "snapshot-first".to_owned(),
            attachment_type: "snapshot".to_owned(),
            content_type: "image/png".to_owned(),
            byte_len: Some(15),
            ordinal: 0,
            timestamp_ms: Some(2_020),
            text: None,
        });
        revision_two.canonical_attachment_id = None;
        revision_two.icon_key = "vehicle".to_owned();
        revision_two.rejected_icon_key = None;
        handle.insert_event(revision_two).unwrap();
        let replaced = handle.event_by_id("event-1").unwrap().unwrap();
        assert_eq!(replaced.revision, 2);
        assert_eq!(
            replaced.canonical_attachment_id.as_deref(),
            Some("snapshot-first")
        );
        assert_eq!(replaced.icon_key, "vehicle");
        assert!(!replaced.canonical_image_owns_bbox());
        assert_eq!(
            replaced
                .attachments
                .iter()
                .map(|attachment| attachment.id.as_str())
                .collect::<Vec<_>>(),
            ["story-explicit", "snapshot-later", "snapshot-first"]
        );

        let stale_error = handle.insert_event(replaced.clone()).unwrap_err();
        assert!(
            stale_error
                .to_string()
                .contains("does not exceed stored revision 2")
        );
        let mut invalid = replaced.clone();
        invalid.revision = 3;
        invalid.canonical_attachment_id = Some("missing".to_owned());
        let invalid_error = handle.insert_event(invalid).unwrap_err();
        assert_eq!(
            invalid_error.to_string(),
            "canonical attachment was not found in the event revision"
        );

        let mut revision_three = replaced;
        revision_three.revision = 3;
        revision_three.attachments = vec![EventAttachment {
            id: "replacement-story".to_owned(),
            attachment_type: "story-frame".to_owned(),
            content_type: "image/jpeg".to_owned(),
            byte_len: None,
            ordinal: 2,
            timestamp_ms: None,
            text: None,
        }];
        revision_three.canonical_attachment_id = None;
        revision_three.bbox_attachment_id = Some("replacement-story".to_owned());
        handle.insert_event(revision_three).unwrap();
        let final_event = handle.event_by_id("event-1").unwrap().unwrap();
        assert_eq!(final_event.revision, 3);
        assert_eq!(
            final_event.canonical_attachment_id.as_deref(),
            Some("replacement-story")
        );
        assert!(final_event.canonical_image_owns_bbox());

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_restores_and_revises_open_operational_events() {
        let root = test_dir("turso-operational-events");
        let catalog_path = root.join("recordings.db");
        let mut event = OperationalEvent {
            id: "operational-1".to_owned(),
            key: OperationalEventKey {
                camera_id: "front-door".to_owned(),
                stream_id: Some("sub".to_owned()),
                kind: OperationalEventKind::StreamStale,
            },
            evidence: OperationalEvidence {
                cause: "frames_stale".to_owned(),
                explanation: "No recent frames".to_owned(),
                affected_streams: vec!["sub".to_owned()],
                recording_interrupted: true,
                source: "canonical_health".to_owned(),
            },
            severity: OperationalSeverity::Warning,
            revision: 1,
            start_time_ms: 2_000,
            end_time_ms: None,
            duration_ms: None,
        };
        {
            let catalog = RecordingCatalog::open(&catalog_path).unwrap();
            catalog
                .handle()
                .upsert_operational_event(event.clone())
                .unwrap();
            catalog.shutdown();
        }

        let catalog = RecordingCatalog::open(&catalog_path).unwrap();
        let handle = catalog.handle();
        assert_eq!(
            handle.open_operational_events().unwrap(),
            vec![event.clone()]
        );
        event.revision = 2;
        event.severity = OperationalSeverity::Critical;
        event.evidence.cause = "keyframes_missing".to_owned();
        handle.upsert_operational_event(event.clone()).unwrap();
        let mut stale_revision = event.clone();
        stale_revision.revision = 1;
        stale_revision.evidence.cause = "stale_revision".to_owned();
        handle.upsert_operational_event(stale_revision).unwrap();
        let queried = handle
            .operational_events_in_range("front-door", 2_500, 3_000)
            .unwrap();
        assert_eq!(queried, vec![event.clone()]);

        event.revision = 3;
        event.end_time_ms = Some(4_000);
        event.duration_ms = Some(2_000);
        handle.upsert_operational_event(event.clone()).unwrap();
        assert!(handle.open_operational_events().unwrap().is_empty());
        assert_eq!(
            handle
                .operational_events_in_range("front-door", 3_000, 5_000)
                .unwrap(),
            vec![event]
        );
        assert!(
            handle
                .operational_events_in_range("front-door", 4_000, 5_000)
                .unwrap()
                .is_empty()
        );

        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn event_keyframe_lookup_uses_the_composite_link_key() {
        let root = test_dir("turso-event-keyframe-plan");
        let path = root.join("recordings.db");
        let details = pollster::block_on(async {
            let database = turso::Builder::new_local(path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let connection = database.connect().unwrap();
            initialize_schema(&connection).await.unwrap();
            let mut rows = connection
                .query(
                    format!("EXPLAIN QUERY PLAN {RESOLVE_EVENT_KEYFRAME_SQL}"),
                    turso::params!["event-1", "main"],
                )
                .await
                .unwrap();
            let mut details = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                details.push(row.get::<String>(3).unwrap());
            }
            details
        });
        assert!(
            details.iter().any(|detail| {
                detail.contains("sqlite_autoindex_recording_event_keyframes_1")
                    && detail.contains("event_id")
            }),
            "query plan did not use the event-link primary key: {details:?}"
        );
        assert!(
            details.iter().all(|detail| !detail.starts_with("SCAN l")),
            "query plan scanned the event-keyframe link table: {details:?}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn schema_migration_backfills_existing_event_types_once() {
        let root = test_dir("turso-event-search-migration");
        let path = root.join("recordings.db");
        let (
            term_count,
            migration_count,
            identity_column_count,
            revision,
            bbox_attachment_id,
            attachments,
            canonical_attachment_id,
            icon_key,
        ) = pollster::block_on(async {
            let database = turso::Builder::new_local(path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let connection = database.connect().unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE recording_files (
                         id TEXT PRIMARY KEY,
                         stream_id TEXT NOT NULL,
                         started_at_ms INTEGER NOT NULL,
                         ended_at_ms INTEGER,
                         path TEXT NOT NULL UNIQUE,
                         init_offset INTEGER NOT NULL,
                         init_len INTEGER NOT NULL,
                         finalized INTEGER NOT NULL
                     );
                     CREATE TABLE recording_events (
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
                     INSERT INTO recording_events (
                         id, camera_id, source, kind, start_time_ms,
                         bbox_json, thumbnail_filename
                     ) VALUES (
                         'event-1', 'front-door', 'camera', 'Vehicle', 1000,
                         '[0.1,0.2,0.3,0.4]', 'event-1.jpg'
                     );",
                )
                .await
                .unwrap();
            initialize_schema(&connection).await.unwrap();
            initialize_schema(&connection).await.unwrap();
            let term_count = query_count(
                &connection,
                "SELECT COUNT(*) FROM recording_event_search_terms
                 WHERE event_id = 'event-1'
                   AND field = 'event_type'
                   AND normalized_value = 'vehicle'",
            )
            .await;
            let migration_count = query_count(
                &connection,
                "SELECT COUNT(*) FROM catalog_schema_migrations WHERE version = 2",
            )
            .await;
            let mut columns = connection
                .query("PRAGMA table_info(recording_files)", ())
                .await
                .unwrap();
            let mut identity_column_count = 0;
            while let Some(row) = columns.next().await.unwrap() {
                if matches!(
                    row.get::<String>(1).unwrap().as_str(),
                    "source_id" | "logical_stream_id"
                ) {
                    identity_column_count += 1;
                }
            }
            let mut rows = connection
                .query(
                    "SELECT revision, bbox_attachment_id, attachments_json,
                            canonical_attachment_id, icon_key
                     FROM recording_events WHERE id = 'event-1'",
                    (),
                )
                .await
                .unwrap();
            let row = rows.next().await.unwrap().unwrap();
            let attachments =
                serde_json::from_str::<Vec<EventAttachment>>(&row.get::<String>(2).unwrap())
                    .unwrap();
            (
                term_count,
                migration_count,
                identity_column_count,
                row.get::<i64>(0).unwrap(),
                row.get::<Option<String>>(1).unwrap(),
                attachments,
                row.get::<Option<String>>(3).unwrap(),
                row.get::<String>(4).unwrap(),
            )
        });
        assert_eq!(term_count, 1);
        assert_eq!(migration_count, 1);
        assert_eq!(identity_column_count, 2);
        assert_eq!(revision, 1);
        assert_eq!(bbox_attachment_id.as_deref(), Some("thumbnail"));
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].id, "thumbnail");
        assert_eq!(attachments[0].timestamp_ms, Some(1_000));
        assert_eq!(canonical_attachment_id.as_deref(), Some("thumbnail"));
        assert_eq!(icon_key, "vehicle");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn event_keyframes_reconcile_in_both_arrival_orders() {
        let root = test_dir("turso-event-keyframe-reconciliation");
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front-door/main".to_owned(),
                source_id: Some("192.0.2.10".to_owned()),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(3_000),
                path: root.join("recording.mp4").to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 7,
                finalized: true,
            })
            .unwrap();
        handle
            .insert_event(test_event("event-before", 1_500))
            .unwrap();
        handle
            .insert_fragment_with_keyframe(test_fragment(), test_keyframe())
            .unwrap();
        assert!(
            handle
                .resolve_event_keyframe("event-before", "main")
                .unwrap()
                .is_some()
        );

        handle
            .insert_event(test_event("event-after", 1_600))
            .unwrap();
        assert!(
            handle
                .resolve_event_keyframe("event-after", "main")
                .unwrap()
                .is_some()
        );
        assert!(
            handle
                .resolve_event_keyframe("event-after", "sub")
                .unwrap()
                .is_none()
        );

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_backfills_legacy_mp4_keyframes_and_removes_missing_files() {
        let root = test_dir("turso-keyframe-backfill");
        let recording_path = root.join("legacy.mp4");
        let (initialization, fragments) = write_fragmented_recording(&recording_path);
        let missing_path = root.join("missing.mp4");
        let catalog_path = root.join("recordings.db");
        let catalog = RecordingCatalog::open(&catalog_path).unwrap();
        let handle = catalog.handle();
        for (id, path) in [
            ("legacy-recording", recording_path.as_path()),
            ("missing-recording", missing_path.as_path()),
        ] {
            handle
                .upsert_recording(CatalogRecording {
                    id: id.to_owned(),
                    stream_id: "front-door/main".to_owned(),
                    source_id: None,
                    logical_stream_id: None,
                    started_at_ms: 1_000,
                    ended_at_ms: Some(3_000),
                    path: path.to_string_lossy().into_owned(),
                    init_offset: initialization.offset,
                    init_len: initialization.size,
                    finalized: true,
                })
                .unwrap();
            for (index, fragment) in fragments.iter().enumerate() {
                handle
                    .insert_fragment(CatalogFragment {
                        recording_id: id.to_owned(),
                        sequence: u64::from(fragment.sequence_number),
                        start_ms: 1_000 + index as i64 * 1_000,
                        duration_ms: 1_000,
                        byte_offset: fragment.range.offset,
                        byte_len: fragment.range.size,
                        random_access: true,
                    })
                    .unwrap();
            }
        }
        handle
            .insert_event(test_event("legacy-event", 1_500))
            .unwrap();
        drop(handle);
        catalog.shutdown();

        let mut catalog = RecordingCatalog::open(&catalog_path).unwrap();
        catalog.wait_for_maintenance();
        let handle = catalog.handle();
        handle
            .backfill_recording_identity("front-door/main", "192.0.2.10", "main")
            .unwrap();
        let location = handle
            .resolve_event_keyframe("legacy-event", "main")
            .unwrap()
            .unwrap();
        assert_eq!(location.recording_id, "legacy-recording");
        assert_eq!(handle.stats().unwrap().recording_files, 1);
        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_recovers_an_interrupted_recording_finalization() {
        let root = test_dir("turso-interrupted-finalization");
        let final_path = root.join("recording.mp4");
        let active_path = root.join("recording.mp4.active");
        let (initialization, fragments) = write_fragmented_recording(&final_path);
        let catalog_path = root.join("recordings.db");
        let catalog = RecordingCatalog::open(&catalog_path).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front-door/main".to_owned(),
                source_id: Some("192.0.2.10".to_owned()),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(3_000),
                path: active_path.to_string_lossy().into_owned(),
                init_offset: initialization.offset,
                init_len: initialization.size,
                finalized: false,
            })
            .unwrap();
        for (index, fragment) in fragments.iter().enumerate() {
            handle
                .insert_fragment(CatalogFragment {
                    recording_id: "recording-1".to_owned(),
                    sequence: u64::from(fragment.sequence_number),
                    start_ms: 1_000 + index as i64 * 1_000,
                    duration_ms: 1_000,
                    byte_offset: fragment.range.offset,
                    byte_len: fragment.range.size,
                    random_access: true,
                })
                .unwrap();
        }
        handle.insert_event(test_event("event-1", 1_500)).unwrap();
        drop(handle);
        catalog.shutdown();

        let mut catalog = RecordingCatalog::open(&catalog_path).unwrap();
        catalog.wait_for_maintenance();
        let handle = catalog.handle();
        let location = handle
            .resolve_event_keyframe("event-1", "main")
            .unwrap()
            .unwrap();
        assert_eq!(location.path, final_path.to_string_lossy());
        assert_eq!(handle.stats().unwrap().finalized_files, 1);

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retention_deletes_media_links_but_preserves_events() {
        let root = test_dir("turso-retention-cascade");
        let recording_path = root.join("recording.mp4");
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front-door/main".to_owned(),
                source_id: Some("192.0.2.10".to_owned()),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(3_000),
                path: recording_path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 7,
                finalized: true,
            })
            .unwrap();
        handle
            .insert_fragment_with_keyframe(test_fragment(), test_keyframe())
            .unwrap();
        handle.insert_event(test_event("event-1", 1_500)).unwrap();
        assert!(
            handle
                .resolve_event_keyframe("event-1", "main")
                .unwrap()
                .is_some()
        );

        handle
            .delete_recordings_by_path(std::slice::from_ref(&recording_path))
            .unwrap();
        assert!(handle.event_by_id("event-1").unwrap().is_some());
        assert!(
            handle
                .resolve_event_keyframe("event-1", "main")
                .unwrap()
                .is_none()
        );
        assert_eq!(handle.stats().unwrap().recording_files, 0);

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleanup_intent_recovers_after_file_deletion_and_completion_is_idempotent() {
        let root = test_dir("turso-cleanup-reconciliation");
        let catalog_path = root.join("recordings.db");
        let recording_path = root.join("old.mp4");
        std::fs::write(&recording_path, vec![0; 64]).unwrap();
        let catalog = RecordingCatalog::open(&catalog_path).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "old".to_owned(),
                stream_id: "front/sub".to_owned(),
                source_id: Some("front".to_owned()),
                logical_stream_id: Some("sub".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(2_000),
                path: recording_path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 8,
                finalized: true,
            })
            .unwrap();
        handle
            .update_recording_path("old", &recording_path, true)
            .unwrap();

        let claimed = handle.claim_cleanup_candidate().unwrap().unwrap();
        assert_eq!(claimed.recording_id, "old");
        assert_eq!(claimed.file_bytes, 64);
        assert!(!claimed.pending);
        drop(handle);
        catalog.shutdown();

        let catalog = RecordingCatalog::open(&catalog_path).unwrap();
        let handle = catalog.handle();
        let before_deletion = handle.pending_cleanup_candidate().unwrap().unwrap();
        assert_eq!(before_deletion.recording_id, "old");
        assert!(before_deletion.path.is_file());
        std::fs::remove_file(&recording_path).unwrap();
        drop(handle);
        catalog.shutdown();

        let catalog = RecordingCatalog::open(&catalog_path).unwrap();
        let handle = catalog.handle();
        let recovered = handle.claim_cleanup_candidate().unwrap().unwrap();
        assert_eq!(recovered.recording_id, "old");
        assert!(recovered.pending);
        handle.complete_cleanup("old").unwrap();
        handle.complete_cleanup("old").unwrap();
        assert!(handle.claim_cleanup_candidate().unwrap().is_none());

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleanup_candidates_exclude_active_and_protected_recordings() {
        let root = test_dir("turso-cleanup-protection");
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        for (id, finalized, started_at_ms) in [("protected", true, 1_000), ("active", false, 2_000)]
        {
            let path = root.join(format!("{id}.mp4"));
            std::fs::write(&path, vec![0; 32]).unwrap();
            handle
                .upsert_recording(CatalogRecording {
                    id: id.to_owned(),
                    stream_id: "front/sub".to_owned(),
                    source_id: Some("front".to_owned()),
                    logical_stream_id: Some("sub".to_owned()),
                    started_at_ms,
                    ended_at_ms: finalized.then_some(started_at_ms + 1_000),
                    path: path.to_string_lossy().into_owned(),
                    init_offset: 0,
                    init_len: 8,
                    finalized,
                })
                .unwrap();
            if finalized {
                handle.update_recording_path(id, &path, true).unwrap();
            }
        }
        handle.set_recording_protected("protected", true).unwrap();

        assert!(handle.claim_cleanup_candidate().unwrap().is_none());
        handle.set_recording_protected("protected", false).unwrap();
        assert_eq!(
            handle
                .claim_cleanup_candidate()
                .unwrap()
                .map(|candidate| candidate.recording_id),
            Some("protected".to_owned())
        );

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_search_backfill_does_not_commit_its_migration_marker() {
        let root = test_dir("turso-event-search-migration-rollback");
        let path = root.join("recordings.db");
        let marker_count = pollster::block_on(async {
            let database = turso::Builder::new_local(path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let connection = database.connect().unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE recording_events (
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
                     CREATE TABLE recording_event_search_terms (
                         event_id TEXT NOT NULL,
                         field TEXT NOT NULL,
                         normalized_value TEXT NOT NULL,
                         display_value TEXT NOT NULL,
                         PRIMARY KEY(event_id, field, normalized_value)
                     );
                     CREATE TRIGGER fail_event_search_backfill
                     BEFORE INSERT ON recording_event_search_terms
                     BEGIN
                         SELECT RAISE(ABORT, 'simulated migration failure');
                     END;
                     INSERT INTO recording_events (
                         id, camera_id, source, kind, start_time_ms
                     ) VALUES ('event-1', 'front-door', 'camera', 'Vehicle', 1000);",
                )
                .await
                .unwrap();
            assert!(initialize_schema(&connection).await.is_err());
            query_count(
                &connection,
                "SELECT COUNT(*) FROM catalog_schema_migrations WHERE version = 2",
            )
            .await
        });
        assert_eq!(marker_count, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn test_event(id: &str, start_time_ms: i64) -> TimelineEvent {
        TimelineEvent {
            id: id.to_owned(),
            revision: 1,
            camera_id: "192.0.2.10".to_owned(),
            stream: Some("main".to_owned()),
            source: EventSource::Camera,
            kind: "motion".to_owned(),
            start_time_ms,
            end_time_ms: Some(start_time_ms + 100),
            confidence: None,
            bbox: None,
            bbox_attachment_id: None,
            zone: None,
            attachments: Vec::new(),
            canonical_attachment_id: None,
            icon_key: "motion".to_owned(),
            rejected_icon_key: None,
            thumbnail_filename: None,
        }
    }

    fn test_fragment() -> CatalogFragment {
        CatalogFragment {
            recording_id: "recording-1".to_owned(),
            sequence: 1,
            start_ms: 1_000,
            duration_ms: 2_000,
            byte_offset: 7,
            byte_len: 24,
            random_access: true,
        }
    }

    fn test_keyframe() -> CatalogKeyframe {
        CatalogKeyframe {
            recording_id: "recording-1".to_owned(),
            fragment_sequence: 1,
            byte_offset: 7,
            byte_len: 16,
        }
    }

    fn write_fragmented_recording(path: &Path) -> (mp4::Mp4ByteRange, Vec<mp4::Mp4FragmentInfo>) {
        let config = mp4::Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
            timescale: 1_000,
        };
        let track = mp4::TrackConfig {
            track_type: mp4::TrackType::Video,
            timescale: 90_000,
            language: "und".to_owned(),
            media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                width: 320,
                height: 240,
                seq_param_set: Vec::new(),
                pic_param_set: Vec::new(),
            }),
        };
        let mut writer = mp4::FragmentedMp4Writer::write_start(
            BufWriter::new(File::create(path).unwrap()),
            &config,
            &[track],
        )
        .unwrap();
        let initialization = writer.initialization();
        let mut fragments = Vec::new();
        for index in 0..2 {
            writer
                .write_sample(
                    1,
                    mp4::Mp4Sample {
                        start_time: index * 90_000,
                        duration: 90_000,
                        rendering_offset: 0,
                        is_sync: true,
                        bytes: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
                    },
                )
                .unwrap();
            fragments.push(writer.flush_fragment().unwrap().unwrap());
        }
        drop(writer.into_writer());
        (initialization, fragments)
    }

    async fn query_count(connection: &turso::Connection, sql: &str) -> i64 {
        connection
            .query(sql, ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap()
    }
}
