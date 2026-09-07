//! Durable principal review state and shared bookmarks, independent of recording retention.

use super::{RecordingCatalogHandle, current_unix_time_ms, mpsc, to_i64, to_u64};
use std::collections::BTreeSet;

mod bookmarks;
mod library;
mod query;
pub(super) use library::list as list_bookmarks;
pub(super) use query::{append_filter, counts, hydrate, snapshot};

pub(crate) const MAX_BATCH: usize = 128;
pub(crate) const MAX_NOTE_BYTES: usize = 1_024;
const MAX_ID_BYTES: usize = 256;
const MAX_IDENTITY_BYTES: usize = 16 * 1_024;
const RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_REVIEW_ROWS: i64 = 250_000;
const MAX_EVENT_PRINCIPALS: i64 = 64;
const TOMBSTONE_RETENTION_MS: i64 = 90 * 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventKey {
    pub source_id: String,
    pub event_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewChange {
    pub key: EventKey,
    pub expected_revision: u64,
    pub reviewed: bool,
    pub dismissed: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct BookmarkChange {
    pub key: EventKey,
    pub expected_revision: u64,
    pub active: bool,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub active: bool,
    pub note: String,
    pub revision: u64,
    pub created_by: String,
    pub created_at_ms: i64,
    pub updated_by: String,
    pub updated_at_ms: i64,
    pub event_start_ms: i64,
    pub event_kind: String,
    pub audit: Vec<BookmarkAudit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkAudit {
    pub revision: u64,
    pub actor_id: String,
    pub occurred_at_ms: i64,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub key: EventKey,
    pub reviewed: bool,
    pub dismissed: bool,
    pub review_revision: u64,
    pub reviewed_at_ms: Option<i64>,
    pub dismissed_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub bookmark: Option<Bookmark>,
    pub event_present: bool,
    pub media_available: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub enum ReviewFilter {
    #[default]
    Any,
    Unreviewed,
    Reviewed,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Query {
    pub actor_id: String,
    pub review: ReviewFilter,
    pub bookmarked: Option<bool>,
    pub bookmarked_by_me: bool,
}

impl Query {
    pub fn new(actor_id: impl Into<String>) -> Self {
        Self {
            actor_id: actor_id.into(),
            review: ReviewFilter::Any,
            bookmarked: None,
            bookmarked_by_me: false,
        }
    }

    pub(super) fn fingerprint(&self) -> String {
        serde_json::to_string(self).expect("workflow query has only serializable fields")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: u64,
    pub unreviewed: u64,
    pub reviewed: u64,
    pub dismissed: u64,
    pub bookmarked: u64,
    pub bookmarked_by_me: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct BookmarkQuery {
    pub actor_id: String,
    pub source_ids: Vec<String>,
    pub all_sources: bool,
    pub start_ms: i64,
    pub end_ms: i64,
    pub by_me: bool,
    pub page_size: u32,
    #[serde(skip)]
    pub page_token: String,
}

#[derive(Debug)]
pub(crate) struct BookmarkPage {
    pub states: Vec<State>,
    pub next_page_token: String,
    pub total: u64,
}

#[derive(Debug)]
pub(crate) struct Conflict {
    pub current: State,
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("event workflow revision conflict")
    }
}

impl std::error::Error for Conflict {}

#[derive(Debug)]
pub(crate) enum Failure {
    Invalid(&'static str),
    Limit(&'static str),
    Forbidden,
    NotFound,
    Unavailable,
}

impl Failure {
    pub(crate) const fn message(&self) -> &'static str {
        match self {
            Self::Invalid(message) | Self::Limit(message) => message,
            Self::Forbidden => {
                "only the bookmark creator or an administrator can change this bookmark"
            }
            Self::NotFound => "event workflow source or event does not exist",
            Self::Unavailable => "event workflow is unavailable; reload state before retrying",
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for Failure {}

pub(super) enum Command {
    Read {
        actor: String,
        keys: Vec<EventKey>,
    },
    Review {
        actor: String,
        changes: Vec<ReviewChange>,
    },
    Bookmark {
        actor: String,
        administrator: bool,
        change: BookmarkChange,
    },
}

impl RecordingCatalogHandle {
    pub(crate) fn list_event_bookmarks(
        &self,
        query: BookmarkQuery,
    ) -> anyhow::Result<BookmarkPage> {
        library::validate(&query)?;
        let (reply, response) = mpsc::sync_channel(1);
        self.search_tx
            .try_send(super::SearchCommand::Bookmarks { query, reply })
            .map_err(|_| Failure::Unavailable)?;
        response
            .recv_timeout(RESPONSE_TIMEOUT)
            .map_err(|_| Failure::Unavailable)?
    }

    pub(crate) fn event_workflow(
        &self,
        actor: &str,
        keys: Vec<EventKey>,
    ) -> anyhow::Result<Vec<State>> {
        validate_keys(actor, keys.iter())?;
        self.workflow_command(Command::Read {
            actor: actor.to_owned(),
            keys,
        })
    }

    pub(crate) fn mutate_event_reviews(
        &self,
        actor: &str,
        changes: Vec<ReviewChange>,
    ) -> anyhow::Result<Vec<State>> {
        validate_keys(actor, changes.iter().map(|change| &change.key))?;
        self.workflow_command(Command::Review {
            actor: actor.to_owned(),
            changes,
        })
    }

    pub(crate) fn mutate_event_bookmark(
        &self,
        actor: &str,
        administrator: bool,
        change: BookmarkChange,
    ) -> anyhow::Result<Vec<State>> {
        validate_keys(actor, std::iter::once(&change.key))?;
        anyhow::ensure!(
            change.note.len() <= MAX_NOTE_BYTES,
            Failure::Invalid("bookmark note exceeds 1,024 UTF-8 bytes")
        );
        anyhow::ensure!(
            !change
                .note
                .chars()
                .any(|character| character.is_control() && character != '\n' && character != '\t'),
            Failure::Invalid("bookmark note contains control characters")
        );
        self.workflow_command(Command::Bookmark {
            actor: actor.to_owned(),
            administrator,
            change,
        })
    }

    fn workflow_command(&self, command: Command) -> anyhow::Result<Vec<State>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(super::Command::Workflow { command, reply })
            .map_err(|_| Failure::Unavailable)?;
        response
            .recv_timeout(RESPONSE_TIMEOUT)
            .map_err(|_| Failure::Unavailable)?
    }
}

fn validate_keys<'key>(
    actor: &str,
    keys: impl Iterator<Item = &'key EventKey>,
) -> anyhow::Result<()> {
    validate_id(actor)?;
    let mut unique = BTreeSet::new();
    let mut identity_bytes = 0usize;
    for key in keys {
        validate_id(&key.source_id)?;
        validate_id(&key.event_id)?;
        identity_bytes += key.source_id.len() + key.event_id.len();
        anyhow::ensure!(
            identity_bytes <= MAX_IDENTITY_BYTES,
            Failure::Invalid("event workflow identity bytes exceed 16 KiB")
        );
        anyhow::ensure!(
            unique.insert(key),
            Failure::Invalid("duplicate event workflow target")
        );
        anyhow::ensure!(
            unique.len() <= MAX_BATCH,
            Failure::Invalid("event workflow batch exceeds 128 events")
        );
    }
    anyhow::ensure!(
        !unique.is_empty(),
        Failure::Invalid("event workflow batch is empty")
    );
    Ok(())
}

fn validate_id(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= MAX_ID_BYTES && !value.chars().any(char::is_control),
        Failure::Invalid("event workflow identity is invalid")
    );
    Ok(())
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS event_reviews (
             source_id TEXT NOT NULL,
             event_id TEXT NOT NULL,
             principal_id TEXT NOT NULL,
             reviewed INTEGER NOT NULL CHECK (reviewed IN (0, 1)),
             dismissed INTEGER NOT NULL CHECK (dismissed IN (0, 1)),
             revision INTEGER NOT NULL CHECK (revision > 0),
             reviewed_at_ms INTEGER,
             dismissed_at_ms INTEGER,
             updated_at_ms INTEGER NOT NULL,
             PRIMARY KEY(source_id, event_id, principal_id)
         );
         CREATE INDEX IF NOT EXISTS event_reviews_principal
             ON event_reviews(principal_id, source_id, event_id, reviewed, dismissed);
         CREATE TABLE IF NOT EXISTS event_workflow_budget (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             review_rows INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO event_workflow_budget VALUES (1, 0);
         CREATE TRIGGER IF NOT EXISTS event_reviews_insert
         AFTER INSERT ON event_reviews BEGIN
             UPDATE event_workflow_budget SET review_rows = review_rows + 1 WHERE id = 1;
         END;
         CREATE TRIGGER IF NOT EXISTS event_reviews_delete
         AFTER DELETE ON event_reviews BEGIN
             UPDATE event_workflow_budget SET review_rows = review_rows - 1 WHERE id = 1;
         END;",
        )
        .await?;
    super::ensure_column(
        connection,
        "event_reviews",
        "event_deleted_at_ms",
        "INTEGER",
    )
    .await?;
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS event_reviews_deleted ON event_reviews(event_deleted_at_ms);
         CREATE TRIGGER IF NOT EXISTS event_reviews_event_deleted
         AFTER DELETE ON recording_events BEGIN
             UPDATE event_reviews SET event_deleted_at_ms = CAST(unixepoch('subsec') * 1000 AS INTEGER)
             WHERE event_id = OLD.id AND source_id = OLD.camera_id;
         END;"
    ).await?;
    bookmarks::initialize(connection).await?;
    cleanup(connection, current_unix_time_ms()).await?;
    Ok(())
}

async fn cleanup(connection: &turso::Connection, now_ms: i64) -> anyhow::Result<()> {
    let cutoff = now_ms.saturating_sub(TOMBSTONE_RETENTION_MS);
    for table in ["event_reviews", "event_bookmarks"] {
        connection.execute(format!(
            "DELETE FROM {table} WHERE rowid IN (
                 SELECT retained.rowid FROM {table} AS retained
                 WHERE retained.event_deleted_at_ms < ?1
                     AND NOT EXISTS(SELECT 1 FROM recording_events AS event
                         WHERE event.id = retained.event_id AND event.camera_id = retained.source_id)
                 ORDER BY retained.event_deleted_at_ms LIMIT 128)"
        ), turso::params![cutoff]).await?;
    }
    Ok(())
}

pub(super) async fn execute(
    connection: &turso::Connection,
    command: Command,
) -> anyhow::Result<Vec<State>> {
    if let Command::Read { actor, keys } = command {
        let mut states = Vec::with_capacity(keys.len());
        for key in keys {
            states.push(read(connection, &actor, key).await?);
        }
        return Ok(states);
    }
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let now_ms = current_unix_time_ms();
    let result = async {
        cleanup(connection, now_ms).await?;
        match command {
            Command::Review { actor, changes } => {
                mutate_reviews(connection, &actor, changes, now_ms).await
            }
            Command::Bookmark {
                actor,
                administrator,
                change,
            } => bookmarks::mutate(connection, &actor, administrator, change, now_ms).await,
            Command::Read { .. } => unreachable!("workflow read returned before transaction"),
        }
    }
    .await;
    match result {
        Ok(states) => {
            connection.execute_batch("COMMIT").await?;
            Ok(states)
        }
        Err(error) => {
            connection.execute_batch("ROLLBACK").await?;
            Err(error)
        }
    }
}

async fn read(connection: &turso::Connection, actor: &str, key: EventKey) -> anyhow::Result<State> {
    let bookmark = bookmarks::read(connection, &key).await?;
    let (event_present, media_available) = presence(connection, &key).await?;
    let mut rows = connection
        .query(
            "SELECT reviewed, dismissed, revision, reviewed_at_ms, dismissed_at_ms, updated_at_ms
         FROM event_reviews WHERE source_id = ?1 AND event_id = ?2 AND principal_id = ?3",
            turso::params![key.source_id.clone(), key.event_id.clone(), actor],
        )
        .await?;
    let mut state = State {
        key,
        reviewed: false,
        dismissed: false,
        review_revision: 0,
        reviewed_at_ms: None,
        dismissed_at_ms: None,
        updated_at_ms: None,
        bookmark,
        event_present,
        media_available,
    };
    if let Some(row) = rows.next().await? {
        state.reviewed = row.get::<i64>(0)? != 0;
        state.dismissed = row.get::<i64>(1)? != 0;
        state.review_revision = to_u64(row.get(2)?, "review revision")?;
        state.reviewed_at_ms = row.get(3)?;
        state.dismissed_at_ms = row.get(4)?;
        state.updated_at_ms = row.get(5)?;
    }
    Ok(state)
}

async fn presence(connection: &turso::Connection, key: &EventKey) -> anyhow::Result<(bool, bool)> {
    let mut rows = connection
        .query(
            "SELECT EXISTS(SELECT 1 FROM recording_events WHERE id = ?1 AND camera_id = ?2)",
            turso::params![key.event_id.clone(), key.source_id.clone()],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("event presence result is missing"))?;
    let event_present = row.get::<i64>(0)? != 0;
    drop(rows);
    if !event_present {
        return Ok((false, false));
    }
    let mut media = connection
        .query(
            "SELECT recording.path FROM recording_event_keyframes AS link
         JOIN recording_files AS recording ON recording.id = link.recording_id
         WHERE link.event_id = ?1 AND recording.cleanup_pending = 0 LIMIT 4",
            turso::params![key.event_id.clone()],
        )
        .await?;
    while let Some(row) = media.next().await? {
        if std::path::Path::new(&row.get::<String>(0)?).is_file() {
            return Ok((true, true));
        }
    }
    Ok((true, false))
}

async fn mutate_reviews(
    connection: &turso::Connection,
    actor: &str,
    changes: Vec<ReviewChange>,
    now_ms: i64,
) -> anyhow::Result<Vec<State>> {
    let keys = changes
        .iter()
        .map(|change| change.key.clone())
        .collect::<Vec<_>>();
    let mut current_states = query::read_states(connection, actor, &keys).await?;
    bookmarks::hydrate_audits(connection, &mut current_states).await?;
    let mut write = connection
        .prepare(
            "INSERT INTO event_reviews (source_id, event_id, principal_id, reviewed, dismissed,
             revision, reviewed_at_ms, dismissed_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(source_id, event_id, principal_id) DO UPDATE SET
             reviewed = excluded.reviewed, dismissed = excluded.dismissed,
             revision = excluded.revision, reviewed_at_ms = excluded.reviewed_at_ms,
             dismissed_at_ms = excluded.dismissed_at_ms, updated_at_ms = excluded.updated_at_ms",
        )
        .await?;
    let mut states = Vec::with_capacity(changes.len());
    for (change, mut current) in changes.into_iter().zip(current_states) {
        anyhow::ensure!(current.event_present, Failure::NotFound);
        if current.review_revision != change.expected_revision {
            return Err(Conflict { current }.into());
        }
        if current.review_revision == 0 {
            check_review_capacity(connection, &change.key).await?;
        }
        let revision = to_i64(change.expected_revision, "review revision")?
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("review revision exhausted"))?;
        current.reviewed = change.reviewed;
        current.dismissed = change.dismissed;
        current.review_revision = to_u64(revision, "review revision")?;
        current.reviewed_at_ms = change
            .reviewed
            .then_some(current.reviewed_at_ms.unwrap_or(now_ms));
        current.dismissed_at_ms = change
            .dismissed
            .then_some(current.dismissed_at_ms.unwrap_or(now_ms));
        current.updated_at_ms = Some(now_ms);
        write
            .execute(turso::params![
                change.key.source_id.clone(),
                change.key.event_id.clone(),
                actor,
                i64::from(change.reviewed),
                i64::from(change.dismissed),
                revision,
                current.reviewed_at_ms,
                current.dismissed_at_ms,
                now_ms
            ])
            .await?;
        write.reset()?;
        states.push(current);
    }
    invalidate_review_search(connection, &states).await?;
    Ok(states)
}

async fn invalidate_review_search(
    connection: &turso::Connection,
    states: &[State],
) -> anyhow::Result<()> {
    assert!(
        !states.is_empty() && states.len() <= MAX_BATCH,
        "review search invalidation requires a bounded nonempty batch"
    );
    connection
        .execute(
            "UPDATE recording_event_search_state SET revision = revision + ?1 WHERE id = 1",
            turso::params![i64::try_from(states.len())?],
        )
        .await?;
    let placeholders = (1..=states.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let changed = connection
        .execute(
            format!(
                "UPDATE recording_events SET search_revision = (
                SELECT revision FROM recording_event_search_state WHERE id = 1
            ) WHERE id IN ({placeholders})"
            ),
            turso::params_from_iter(states.iter().map(|state| state.key.event_id.clone())),
        )
        .await?;
    anyhow::ensure!(
        changed == u64::try_from(states.len())?,
        "review search invalidation did not update every event"
    );
    Ok(())
}

async fn require_event(connection: &turso::Connection, key: &EventKey) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM recording_events WHERE id = ?1 AND camera_id = ?2",
            turso::params![key.event_id.clone(), key.source_id.clone()],
        )
        .await?;
    anyhow::ensure!(rows.next().await?.is_some(), Failure::NotFound);
    Ok(())
}

async fn check_review_capacity(
    connection: &turso::Connection,
    key: &EventKey,
) -> anyhow::Result<()> {
    let mut rows = connection.query(
        "SELECT review_rows, (SELECT COUNT(*) FROM event_reviews WHERE source_id = ?1 AND event_id = ?2)
         FROM event_workflow_budget WHERE id = 1",
        turso::params![key.source_id.clone(), key.event_id.clone()],
    ).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("workflow budget is missing"))?;
    anyhow::ensure!(
        row.get::<i64>(0)? < MAX_REVIEW_ROWS,
        Failure::Limit("event workflow storage limit reached")
    );
    anyhow::ensure!(
        row.get::<i64>(1)? < MAX_EVENT_PRINCIPALS,
        Failure::Limit("event reviewer limit reached")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::catalog::tests::{test_dir, test_event};

    #[test]
    fn event_workflow_batch_reads_preserve_audit_order_and_source_validation() {
        let root = test_dir("turso-workflow-batch-reads");
        let catalog = crate::storage::RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let keys = ["last", "first", "middle"].map(|event_id| {
            handle.insert_event(test_event(event_id, 1_000)).unwrap();
            EventKey {
                source_id: "192.0.2.10".to_owned(),
                event_id: event_id.to_owned(),
            }
        });
        seed_batch_bookmarks(&handle, &keys);
        let changes = keys
            .iter()
            .enumerate()
            .map(|(index, key)| ReviewChange {
                key: key.clone(),
                expected_revision: 0,
                reviewed: index != 1,
                dismissed: index == 1,
            })
            .collect::<Vec<_>>();
        handle.mutate_event_reviews("bob", changes.clone()).unwrap();
        let acknowledged = handle
            .mutate_event_reviews("alice", changes.clone())
            .unwrap();
        let expected = handle.event_workflow("alice", keys.to_vec()).unwrap();
        assert_eq!(acknowledged, expected);
        assert_eq!(expected[0].bookmark.as_ref().unwrap().audit.len(), 16);
        assert!(!expected[1].bookmark.as_ref().unwrap().active);
        let mut rejected = changes.clone();
        for change in &mut rejected {
            change.expected_revision = 1;
            change.reviewed = !change.reviewed;
        }
        rejected[1].key.source_id = "another-source".to_owned();
        let error = handle.mutate_event_reviews("alice", rejected).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<Failure>(),
            Some(Failure::NotFound)
        ));
        assert_eq!(
            handle.event_workflow("alice", keys.to_vec()).unwrap(),
            expected
        );
        let bob = handle.event_workflow("bob", keys.to_vec()).unwrap();
        let error = handle.mutate_event_reviews("bob", changes).unwrap_err();
        assert_eq!(error.downcast_ref::<Conflict>().unwrap().current, bob[0]);
        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    fn seed_batch_bookmarks(handle: &RecordingCatalogHandle, keys: &[EventKey]) {
        for (index, key) in keys.iter().enumerate() {
            for revision in 0..18 {
                handle
                    .mutate_event_bookmark(
                        "alice",
                        false,
                        BookmarkChange {
                            key: key.clone(),
                            expected_revision: revision,
                            active: index != 1,
                            note: format!("{} note revision {revision}", key.event_id),
                        },
                    )
                    .unwrap();
            }
        }
    }

    #[test]
    fn event_workflow_rejects_oversized_identity_batches_before_database_work() {
        let root = test_dir("turso-workflow-bounds");
        let catalog = crate::storage::RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let changes = (0..128)
            .map(|index| ReviewChange {
                key: EventKey {
                    source_id: "a".repeat(256),
                    event_id: format!("{index:03}{}", "b".repeat(253)),
                },
                expected_revision: 0,
                reviewed: true,
                dismissed: false,
            })
            .collect();
        let error = handle.mutate_event_reviews("alice", changes).unwrap_err();
        assert!(error.to_string().contains("identity bytes"), "{error}");
        let key = EventKey {
            source_id: "source".to_owned(),
            event_id: "event".to_owned(),
        };
        assert!(
            handle
                .event_workflow("alice", vec![key.clone(), key.clone()])
                .is_err()
        );
        let error = handle
            .mutate_event_bookmark(
                "alice",
                false,
                BookmarkChange {
                    key,
                    expected_revision: 0,
                    active: true,
                    note: "é".repeat(513),
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("note"));
        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn event_workflow_bookmark_never_pins_recording_or_claims_a_missing_file() {
        use crate::storage::{
            CatalogEventKeyframeLink, CatalogFragment, CatalogKeyframe, CatalogRecording,
            RecordingCatalog,
        };
        let root = test_dir("turso-workflow-media-presence");
        let media_path = root.join("recording.mp4");
        std::fs::write(&media_path, [0u8; 64]).unwrap();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "media-1".to_owned(),
                stream_id: "front/main".to_owned(),
                source_id: Some("192.0.2.10".to_owned()),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: 1000,
                ended_at_ms: Some(2000),
                path: media_path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 8,
                finalized: true,
            })
            .unwrap();
        handle
            .insert_fragment_with_keyframe(
                CatalogFragment {
                    recording_id: "media-1".to_owned(),
                    sequence: 1,
                    start_ms: 1000,
                    duration_ms: 1000,
                    byte_offset: 8,
                    byte_len: 56,
                    random_access: true,
                },
                CatalogKeyframe {
                    recording_id: "media-1".to_owned(),
                    fragment_sequence: 1,
                    byte_offset: 8,
                    byte_len: 16,
                },
            )
            .unwrap();
        handle
            .update_recording_path("media-1", &media_path, true)
            .unwrap();
        handle
            .insert_event(test_event("media-event", 1100))
            .unwrap();
        handle
            .link_event_keyframe(CatalogEventKeyframeLink {
                event_id: "media-event".to_owned(),
                stream_id: "front/main".to_owned(),
                recording_id: "media-1".to_owned(),
                fragment_sequence: 1,
            })
            .unwrap();
        let key = EventKey {
            source_id: "192.0.2.10".to_owned(),
            event_id: "media-event".to_owned(),
        };
        handle
            .mutate_event_bookmark(
                "alice",
                false,
                BookmarkChange {
                    key: key.clone(),
                    expected_revision: 0,
                    active: true,
                    note: String::new(),
                },
            )
            .unwrap();
        assert!(handle.event_workflow("alice", vec![key.clone()]).unwrap()[0].media_available);
        assert_search_matches_point_read(&handle, &key);
        std::fs::remove_file(&media_path).unwrap();
        assert_search_matches_point_read(&handle, &key);
        let missing = handle.event_workflow("alice", vec![key]).unwrap();
        assert!(!missing[0].media_available);
        assert!(missing[0].bookmark.as_ref().unwrap().active);
        assert_eq!(handle.stats().unwrap().protected_files, 0);
        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    fn assert_search_matches_point_read(handle: &RecordingCatalogHandle, key: &EventKey) {
        let mut expected = handle
            .event_workflow("alice", vec![key.clone()])
            .unwrap()
            .remove(0);
        if let Some(bookmark) = &mut expected.bookmark {
            bookmark.audit.clear();
        }
        let mut query = crate::storage::EventMetadataQuery::new("main", 0, 10_000);
        query.workflow = Some(Query::new("alice"));
        let result = handle.search_event_metadata(query).unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].workflow.as_ref(), Some(&expected));
    }

    #[test]
    fn event_workflow_deleted_event_references_expire_without_losing_live_tombstone_revisions() {
        let root = test_dir("turso-workflow-retention");
        let path = root.join("recordings.db");
        pollster::block_on(async {
            let database = turso::Builder::new_local(path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let connection = database.connect().unwrap();
            super::super::initialize_schema(&connection).await.unwrap();
            for event_id in ["deleted", "live"] {
                super::super::insert_event(&connection, test_event(event_id, 1000), None)
                    .await
                    .unwrap();
                let key = EventKey {
                    source_id: "192.0.2.10".to_owned(),
                    event_id: event_id.to_owned(),
                };
                mutate_reviews(
                    &connection,
                    "alice",
                    vec![ReviewChange {
                        key: key.clone(),
                        expected_revision: 0,
                        reviewed: true,
                        dismissed: false,
                    }],
                    1_000,
                )
                .await
                .unwrap();
                bookmarks::mutate(
                    &connection,
                    "alice",
                    false,
                    BookmarkChange {
                        key,
                        expected_revision: 0,
                        active: false,
                        note: "Reference".to_owned(),
                    },
                    1_000,
                )
                .await
                .unwrap();
            }
            connection
                .execute("DELETE FROM recording_events WHERE id = 'deleted'", ())
                .await
                .unwrap();
            let deleted_key = EventKey {
                source_id: "192.0.2.10".to_owned(),
                event_id: "deleted".to_owned(),
            };
            let before = read(&connection, "alice", deleted_key.clone())
                .await
                .unwrap();
            assert!(!before.event_present);
            assert!(!before.media_available);
            assert_eq!(before.bookmark.as_ref().unwrap().event_kind, "motion");
            cleanup(
                &connection,
                current_unix_time_ms() + TOMBSTONE_RETENTION_MS + 1,
            )
            .await
            .unwrap();
            let expired = read(&connection, "alice", deleted_key).await.unwrap();
            assert!(expired.bookmark.is_none());
            assert_eq!(expired.review_revision, 0);
            let live = read(
                &connection,
                "alice",
                EventKey {
                    source_id: "192.0.2.10".to_owned(),
                    event_id: "live".to_owned(),
                },
            )
            .await
            .unwrap();
            assert_eq!(live.bookmark.as_ref().unwrap().revision, 1);
            assert_eq!(live.review_revision, 1);
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn event_workflow_bookmark_library_keeps_deleted_references_and_authorized_counts() {
        let root = test_dir("turso-workflow-bookmark-library");
        let path = root.join("recordings.db");
        pollster::block_on(async {
            let database = turso::Builder::new_local(path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let connection = database.connect().unwrap();
            super::super::initialize_schema(&connection).await.unwrap();
            for (event_id, source_id) in [
                ("first", "allowed"),
                ("second", "allowed"),
                ("hidden", "denied"),
            ] {
                let mut event = test_event(event_id, 1000);
                event.camera_id = source_id.to_owned();
                super::super::insert_event(&connection, event, None)
                    .await
                    .unwrap();
                bookmarks::mutate(
                    &connection,
                    "alice",
                    false,
                    BookmarkChange {
                        key: EventKey {
                            source_id: source_id.to_owned(),
                            event_id: event_id.to_owned(),
                        },
                        expected_revision: 0,
                        active: true,
                        note: "Keep the reference".to_owned(),
                    },
                    1000,
                )
                .await
                .unwrap();
            }
            connection
                .execute("DELETE FROM recording_events WHERE id = 'first'", ())
                .await
                .unwrap();
            let mut query = BookmarkQuery {
                actor_id: "bob".to_owned(),
                source_ids: vec!["allowed".to_owned()],
                all_sources: false,
                start_ms: 0,
                end_ms: 2000,
                by_me: false,
                page_size: 1,
                page_token: String::new(),
            };
            let first = library::list(&connection, &query).await.unwrap();
            assert_eq!(first.total, 2);
            assert_eq!(first.states.len(), 1);
            assert_eq!(first.states[0].key.event_id, "first");
            assert!(!first.states[0].event_present);
            query.page_token = first.next_page_token;
            let next = library::list(&connection, &query).await.unwrap();
            assert_eq!(next.states[0].key.event_id, "second");
            assert!(next.next_page_token.is_empty());
            query.page_token.clear();
            query.by_me = true;
            assert_eq!(library::list(&connection, &query).await.unwrap().total, 0);
            query.actor_id = "alice".to_owned();
            assert_eq!(library::list(&connection, &query).await.unwrap().total, 2);
        });
        std::fs::remove_dir_all(root).unwrap();
    }
}
