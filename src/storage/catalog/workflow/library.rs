use super::{BookmarkPage, BookmarkQuery, EventKey};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
struct Cursor {
    fingerprint: String,
    revision: i64,
    time_ms: i64,
    source_id: String,
    event_id: String,
}

pub(super) fn validate(query: &BookmarkQuery) -> anyhow::Result<()> {
    super::validate_id(&query.actor_id)?;
    anyhow::ensure!(query.source_ids.len() <= 128, "too many bookmark sources");
    for source in &query.source_ids {
        super::validate_id(source)?;
    }
    anyhow::ensure!(
        query.start_ms < query.end_ms
            && query.end_ms.saturating_sub(query.start_ms) <= 31 * 86_400_000,
        "bookmark date range must fit within 31 days"
    );
    anyhow::ensure!(
        (1..=16).contains(&query.page_size),
        "bookmark page requires 1 to 16 records"
    );
    anyhow::ensure!(
        query.page_token.len() <= 4096,
        "bookmark page token exceeds its bound"
    );
    Ok(())
}

pub(in crate::storage::catalog) async fn list(
    connection: &turso::Connection,
    query: &BookmarkQuery,
) -> anyhow::Result<BookmarkPage> {
    validate(query)?;
    super::snapshot(connection, list_snapshot(connection, query)).await
}

async fn list_snapshot(
    connection: &turso::Connection,
    query: &BookmarkQuery,
) -> anyhow::Result<BookmarkPage> {
    let fingerprint = super::super::encode_lower_hex(Sha256::digest(serde_json::to_vec(query)?));
    let mut revision_rows = connection
        .query("SELECT revision FROM event_bookmark_state WHERE id = 1", ())
        .await?;
    let revision = revision_rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("bookmark revision is missing"))?
        .get::<i64>(0)?;
    drop(revision_rows);
    let cursor = if query.page_token.is_empty() {
        None
    } else {
        let decoded = URL_SAFE_NO_PAD.decode(&query.page_token)?;
        let cursor: Cursor = serde_json::from_slice(&decoded)?;
        anyhow::ensure!(
            cursor.fingerprint == fingerprint && cursor.revision == revision,
            "bookmark page token changed; reload bookmarks"
        );
        Some(cursor)
    };
    let (mut from_sql, mut params) = conditions(query);
    let mut rows = connection
        .query(
            format!("SELECT COUNT(*) {from_sql}"),
            turso::params_from_iter(params.iter().cloned()),
        )
        .await?;
    let total = super::to_u64(
        rows.next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("bookmark count is missing"))?
            .get(0)?,
        "bookmark count",
    )?;
    drop(rows);
    if let Some(cursor) = cursor {
        let time = params.len() + 1;
        let source = time + 1;
        let event = time + 2;
        from_sql.push_str(&format!(" AND (bookmark.event_start_ms < ?{time} OR (bookmark.event_start_ms = ?{time}
            AND (bookmark.source_id > ?{source} OR (bookmark.source_id = ?{source} AND bookmark.event_id > ?{event}))))"));
        params.extend(
            turso::params![cursor.time_ms, cursor.source_id, cursor.event_id]
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    from_sql.push_str(&format!(
        " ORDER BY bookmark.event_start_ms DESC, bookmark.source_id, bookmark.event_id LIMIT ?{}",
        params.len() + 1
    ));
    params.push(turso::Value::Integer(i64::from(query.page_size) + 1));
    let mut rows = connection
        .query(
            format!(
                "SELECT bookmark.source_id, bookmark.event_id, bookmark.event_start_ms {from_sql}"
            ),
            turso::params_from_iter(params),
        )
        .await?;
    let mut keys = Vec::with_capacity(query.page_size as usize);
    let mut last = None;
    let mut more = false;
    while let Some(row) = rows.next().await? {
        if keys.len() == query.page_size as usize {
            more = true;
            break;
        }
        let key = EventKey {
            source_id: row.get(0)?,
            event_id: row.get(1)?,
        };
        last = Some(Cursor {
            fingerprint: fingerprint.clone(),
            revision,
            time_ms: row.get(2)?,
            source_id: key.source_id.clone(),
            event_id: key.event_id.clone(),
        });
        keys.push(key);
    }
    drop(rows);
    let next_page_token = if more {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &last.expect("a continued bookmark page contains a row"),
        )?)
    } else {
        String::new()
    };
    let mut states = Vec::with_capacity(keys.len());
    for key in keys {
        states.push(super::read(connection, &query.actor_id, key).await?);
    }
    Ok(BookmarkPage {
        states,
        total,
        next_page_token,
    })
}

fn conditions(query: &BookmarkQuery) -> (String, Vec<turso::Value>) {
    let mut sql = String::from(
        "FROM event_bookmarks AS bookmark WHERE bookmark.active = 1
        AND bookmark.event_start_ms >= ?1 AND bookmark.event_start_ms < ?2",
    );
    let mut params = vec![
        turso::Value::Integer(query.start_ms),
        turso::Value::Integer(query.end_ms),
    ];
    if !query.all_sources && query.source_ids.is_empty() {
        sql.push_str(" AND 0 = 1");
    }
    if !query.source_ids.is_empty() {
        super::super::append_text_filter(
            &mut sql,
            "bookmark.source_id",
            &query.source_ids,
            &mut params,
        );
    }
    if query.by_me {
        sql.push_str(&format!(" AND bookmark.created_by = ?{}", params.len() + 1));
        params.push(turso::Value::Text(query.actor_id.clone()));
    }
    (sql, params)
}
