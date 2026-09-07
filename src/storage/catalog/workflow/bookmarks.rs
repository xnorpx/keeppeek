use super::{
    Bookmark, BookmarkAudit, BookmarkChange, Conflict, EventKey, Failure, State, to_i64, to_u64,
};

const MAX_BOOKMARKS: i64 = 10_000;
const MAX_AUDIT_PER_EVENT: i64 = 16;

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS event_bookmarks (
             source_id TEXT NOT NULL,
             event_id TEXT NOT NULL,
             active INTEGER NOT NULL CHECK (active IN (0, 1)),
             note TEXT NOT NULL,
             revision INTEGER NOT NULL CHECK (revision > 0),
             created_by TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL,
             updated_by TEXT NOT NULL,
             updated_at_ms INTEGER NOT NULL,
             event_start_ms INTEGER NOT NULL,
             event_kind TEXT NOT NULL,
             event_deleted_at_ms INTEGER,
             PRIMARY KEY(source_id, event_id)
         );
         CREATE INDEX IF NOT EXISTS event_bookmarks_creator
             ON event_bookmarks(created_by, active, event_start_ms, event_id);
         CREATE INDEX IF NOT EXISTS event_bookmarks_time
             ON event_bookmarks(active, event_start_ms, event_id);
         CREATE INDEX IF NOT EXISTS event_bookmarks_deleted
             ON event_bookmarks(event_deleted_at_ms);
         CREATE TABLE IF NOT EXISTS event_bookmark_state (
             id INTEGER PRIMARY KEY CHECK(id = 1), revision INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO event_bookmark_state VALUES(1, 0);
         CREATE TRIGGER IF NOT EXISTS event_bookmarks_revision_insert AFTER INSERT ON event_bookmarks BEGIN
             UPDATE event_bookmark_state SET revision = revision + 1 WHERE id = 1;
         END;
         CREATE TRIGGER IF NOT EXISTS event_bookmarks_revision_update AFTER UPDATE ON event_bookmarks BEGIN
             UPDATE event_bookmark_state SET revision = revision + 1 WHERE id = 1;
         END;
         CREATE TRIGGER IF NOT EXISTS event_bookmarks_revision_delete AFTER DELETE ON event_bookmarks BEGIN
             UPDATE event_bookmark_state SET revision = revision + 1 WHERE id = 1;
         END;
         CREATE TABLE IF NOT EXISTS event_bookmark_audit (
             source_id TEXT NOT NULL,
             event_id TEXT NOT NULL,
             revision INTEGER NOT NULL,
             actor_id TEXT NOT NULL,
             occurred_at_ms INTEGER NOT NULL,
             action TEXT NOT NULL,
             PRIMARY KEY(source_id, event_id, revision),
             FOREIGN KEY(source_id, event_id) REFERENCES event_bookmarks(source_id, event_id)
                 ON DELETE CASCADE
         );
         CREATE TRIGGER IF NOT EXISTS event_bookmark_event_deleted
         AFTER DELETE ON recording_events BEGIN
             UPDATE event_bookmarks SET event_deleted_at_ms = CAST(unixepoch('subsec') * 1000 AS INTEGER)
             WHERE event_id = OLD.id AND source_id = OLD.camera_id;
         END;",
    ).await?;
    Ok(())
}

pub(super) async fn read(
    connection: &turso::Connection,
    key: &EventKey,
) -> anyhow::Result<Option<Bookmark>> {
    let mut rows = connection.query(
        "SELECT active, note, revision, created_by, created_at_ms, updated_by, updated_at_ms,
             event_start_ms, event_kind FROM event_bookmarks WHERE source_id = ?1 AND event_id = ?2",
        turso::params![key.source_id.clone(), key.event_id.clone()],
    ).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let mut bookmark = Bookmark {
        active: row.get::<i64>(0)? != 0,
        note: row.get(1)?,
        revision: to_u64(row.get(2)?, "bookmark revision")?,
        created_by: row.get(3)?,
        created_at_ms: row.get(4)?,
        updated_by: row.get(5)?,
        updated_at_ms: row.get(6)?,
        event_start_ms: row.get(7)?,
        event_kind: row.get(8)?,
        audit: Vec::with_capacity(MAX_AUDIT_PER_EVENT as usize),
    };
    drop(rows);
    let mut audit = connection
        .query(
            "SELECT revision, actor_id, occurred_at_ms, action FROM event_bookmark_audit
         WHERE source_id = ?1 AND event_id = ?2 ORDER BY revision LIMIT ?3",
            turso::params![
                key.source_id.clone(),
                key.event_id.clone(),
                MAX_AUDIT_PER_EVENT
            ],
        )
        .await?;
    while let Some(row) = audit.next().await? {
        bookmark.audit.push(BookmarkAudit {
            revision: to_u64(row.get(0)?, "bookmark audit revision")?,
            actor_id: row.get(1)?,
            occurred_at_ms: row.get(2)?,
            action: row.get(3)?,
        });
    }
    Ok(Some(bookmark))
}

pub(super) async fn hydrate_audits(
    connection: &turso::Connection,
    states: &mut [State],
) -> anyhow::Result<()> {
    assert!(
        states.len() <= super::MAX_BATCH,
        "bookmark audit batch exceeds its bound"
    );
    let mut params = Vec::with_capacity(states.len() * 2);
    let targets = states
        .iter()
        .enumerate()
        .filter(|(_, state)| state.bookmark.is_some())
        .map(|(index, state)| {
            let target = format!("({index}, ?{}, ?{})", params.len() + 1, params.len() + 2);
            params.push(turso::Value::Text(state.key.source_id.clone()));
            params.push(turso::Value::Text(state.key.event_id.clone()));
            target
        })
        .collect::<Vec<_>>()
        .join(",");
    if targets.is_empty() {
        return Ok(());
    }
    let limit = i64::try_from(states.len())? * MAX_AUDIT_PER_EVENT + 1;
    let mut rows = connection
        .query(
            format!("WITH requested(position, source_id, event_id) AS (VALUES {targets})
                SELECT requested.position, audit.revision, audit.actor_id, audit.occurred_at_ms, audit.action
                FROM requested JOIN event_bookmark_audit AS audit
                    ON audit.source_id = requested.source_id AND audit.event_id = requested.event_id
                ORDER BY requested.position, audit.revision LIMIT {limit}"),
            turso::params_from_iter(params),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let index = usize::try_from(row.get::<i64>(0)?)?;
        let bookmark = states
            .get_mut(index)
            .and_then(|state| state.bookmark.as_mut())
            .ok_or_else(|| anyhow::anyhow!("bookmark audit row is outside its batch"))?;
        anyhow::ensure!(
            bookmark.audit.len() < usize::try_from(MAX_AUDIT_PER_EVENT)?,
            "bookmark audit exceeds its retained bound"
        );
        bookmark.audit.push(BookmarkAudit {
            revision: to_u64(row.get(1)?, "bookmark audit revision")?,
            actor_id: row.get(2)?,
            occurred_at_ms: row.get(3)?,
            action: row.get(4)?,
        });
    }
    Ok(())
}

pub(super) async fn mutate(
    connection: &turso::Connection,
    actor: &str,
    administrator: bool,
    change: BookmarkChange,
    now_ms: i64,
) -> anyhow::Result<Vec<State>> {
    let current = super::read(connection, actor, change.key.clone()).await?;
    if let Some(bookmark) = &current.bookmark {
        anyhow::ensure!(
            administrator || bookmark.created_by == actor,
            Failure::Forbidden
        );
    } else {
        super::require_event(connection, &change.key).await?;
        validate_event_metadata(connection, &change.key).await?;
        check_capacity(connection).await?;
    }
    if current
        .bookmark
        .as_ref()
        .map_or(0, |bookmark| bookmark.revision)
        != change.expected_revision
    {
        return Err(Conflict { current }.into());
    }
    let revision = to_i64(change.expected_revision, "bookmark revision")?
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("bookmark revision exhausted"))?;
    write(connection, actor, &change, revision, now_ms).await?;
    if current.event_present {
        super::super::record_event_search_mutation(connection, &change.key.event_id).await?;
    }
    let action = if !change.active {
        "removed"
    } else if current.bookmark.as_ref().is_none_or(|item| !item.active) {
        "bookmarked"
    } else {
        "edited"
    };
    connection.execute(
        "INSERT INTO event_bookmark_audit (source_id, event_id, revision, actor_id, occurred_at_ms, action)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        turso::params![change.key.source_id.clone(), change.key.event_id.clone(), revision, actor, now_ms, action],
    ).await?;
    connection.execute(
        "DELETE FROM event_bookmark_audit WHERE source_id = ?1 AND event_id = ?2 AND revision <= ?3",
        turso::params![change.key.source_id.clone(), change.key.event_id.clone(), revision - MAX_AUDIT_PER_EVENT],
    ).await?;
    Ok(vec![super::read(connection, actor, change.key).await?])
}

async fn write(
    connection: &turso::Connection,
    actor: &str,
    change: &BookmarkChange,
    revision: i64,
    now_ms: i64,
) -> anyhow::Result<()> {
    connection.execute(
        "INSERT INTO event_bookmarks (source_id, event_id, active, note, revision, created_by,
             created_at_ms, updated_by, updated_at_ms, event_start_ms, event_kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?6, ?7,
             COALESCE((SELECT start_time_ms FROM recording_events WHERE id = ?2 AND camera_id = ?1), 0),
             COALESCE((SELECT kind FROM recording_events WHERE id = ?2 AND camera_id = ?1), 'event'))
         ON CONFLICT(source_id, event_id) DO UPDATE SET active = excluded.active, note = excluded.note,
             revision = excluded.revision, updated_by = excluded.updated_by, updated_at_ms = excluded.updated_at_ms",
        turso::params![change.key.source_id.clone(), change.key.event_id.clone(), i64::from(change.active),
            change.note.clone(), revision, actor, now_ms],
    ).await?;
    Ok(())
}

async fn check_capacity(connection: &turso::Connection) -> anyhow::Result<()> {
    let mut rows = connection
        .query("SELECT COUNT(*) FROM event_bookmarks", ())
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("bookmark count is missing"))?;
    anyhow::ensure!(
        row.get::<i64>(0)? < MAX_BOOKMARKS,
        Failure::Limit("bookmark storage limit reached")
    );
    Ok(())
}

async fn validate_event_metadata(
    connection: &turso::Connection,
    key: &EventKey,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT kind FROM recording_events WHERE id = ?1 AND camera_id = ?2",
            turso::params![key.event_id.clone(), key.source_id.clone()],
        )
        .await?;
    let row = rows.next().await?.ok_or(Failure::NotFound)?;
    anyhow::ensure!(
        row.get::<String>(0)?.len() <= 256,
        Failure::Invalid("bookmark event type exceeds 256 UTF-8 bytes")
    );
    Ok(())
}
