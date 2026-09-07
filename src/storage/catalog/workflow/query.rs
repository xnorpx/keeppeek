use super::{Bookmark, Counts, EventKey, Query, ReviewFilter, State, to_u64};

pub(in crate::storage::catalog) async fn snapshot<Output>(
    connection: &turso::Connection,
    operation: impl std::future::Future<Output = anyhow::Result<Output>>,
) -> anyhow::Result<Output> {
    connection.execute_batch("BEGIN").await?;
    match operation.await {
        Ok(result) => {
            connection.execute_batch("COMMIT").await?;
            Ok(result)
        }
        Err(error) => {
            connection.execute_batch("ROLLBACK").await?;
            Err(error)
        }
    }
}

fn review_expression(field: &str, actor_parameter: usize) -> String {
    assert!(
        matches!(field, "reviewed" | "dismissed"),
        "unsupported workflow review field"
    );
    format!(
        "COALESCE((SELECT {field} FROM event_reviews AS review
        WHERE review.source_id = e.camera_id AND review.event_id = e.id
            AND review.principal_id = ?{actor_parameter}), 0)"
    )
}

fn bookmark_expression(actor_parameter: Option<usize>) -> String {
    let creator = actor_parameter.map_or_else(String::new, |parameter| {
        format!(" AND bookmark.created_by = ?{parameter}")
    });
    format!("EXISTS(SELECT 1 FROM event_bookmarks AS bookmark
        WHERE bookmark.source_id = e.camera_id AND bookmark.event_id = e.id AND bookmark.active = 1{creator})")
}

pub(in crate::storage::catalog) fn append_filter(
    query: Option<&Query>,
    sql: &mut String,
    params: &mut Vec<turso::Value>,
) -> anyhow::Result<()> {
    let Some(query) = query else {
        return Ok(());
    };
    super::validate_id(&query.actor_id)?;
    if query.review != ReviewFilter::Any || query.bookmarked_by_me {
        params.push(turso::Value::Text(query.actor_id.clone()));
    }
    let reviewed = review_expression("reviewed", params.len());
    let dismissed = review_expression("dismissed", params.len());
    match query.review {
        ReviewFilter::Any => {}
        ReviewFilter::Unreviewed => {
            sql.push_str(&format!(" AND {reviewed} = 0 AND {dismissed} = 0"));
        }
        ReviewFilter::Reviewed => sql.push_str(&format!(" AND {reviewed} = 1")),
        ReviewFilter::Dismissed => sql.push_str(&format!(" AND {dismissed} = 1")),
    }
    if let Some(bookmarked) = query.bookmarked {
        sql.push_str(&format!(
            " AND {} = {}",
            bookmark_expression(None),
            u8::from(bookmarked)
        ));
    }
    if query.bookmarked_by_me {
        sql.push_str(&format!(" AND {}", bookmark_expression(Some(params.len()))));
    }
    Ok(())
}

pub(in crate::storage::catalog) async fn counts(
    connection: &turso::Connection,
    query: Option<&Query>,
    from_sql: &str,
    parameters: &[turso::Value],
) -> anyhow::Result<Option<Counts>> {
    let Some(query) = query else {
        return Ok(None);
    };
    let mut params = parameters.to_vec();
    params.push(turso::Value::Text(query.actor_id.clone()));
    let actor_parameter = params.len();
    let sql = format!("WITH matches AS (SELECT e.camera_id, e.id {from_sql})
        SELECT COUNT(*),
            COALESCE(SUM(COALESCE(review.reviewed, 0) = 0 AND COALESCE(review.dismissed, 0) = 0), 0),
            COALESCE(SUM(review.reviewed), 0), COALESCE(SUM(review.dismissed), 0),
            COALESCE(SUM(bookmark.active), 0),
            COALESCE(SUM(bookmark.active = 1 AND bookmark.created_by = ?{actor_parameter}), 0)
        FROM matches AS matched
        LEFT JOIN event_reviews AS review ON review.source_id = matched.camera_id
            AND review.event_id = matched.id AND review.principal_id = ?{actor_parameter}
        LEFT JOIN event_bookmarks AS bookmark ON bookmark.source_id = matched.camera_id
            AND bookmark.event_id = matched.id");
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("workflow counts are missing"))?;
    Ok(Some(Counts {
        total: to_u64(row.get(0)?, "workflow total count")?,
        unreviewed: to_u64(row.get(1)?, "unreviewed count")?,
        reviewed: to_u64(row.get(2)?, "reviewed count")?,
        dismissed: to_u64(row.get(3)?, "dismissed count")?,
        bookmarked: to_u64(row.get(4)?, "bookmarked count")?,
        bookmarked_by_me: to_u64(row.get(5)?, "owned bookmark count")?,
    }))
}

pub(in crate::storage::catalog) async fn hydrate(
    connection: &turso::Connection,
    query: Option<&Query>,
    hits: &mut [crate::storage::search::EventSearchHit],
) -> anyhow::Result<()> {
    let Some(query) = query else {
        return Ok(());
    };
    if hits.is_empty() {
        return Ok(());
    }
    assert!(
        hits.len() <= super::MAX_BATCH,
        "workflow page exceeds its query bound"
    );
    let mut params = Vec::with_capacity(1 + hits.len() * 2);
    params.push(turso::Value::Text(query.actor_id.clone()));
    let targets = hits
        .iter()
        .enumerate()
        .map(|(index, hit)| {
            let target = format!("({index}, ?{}, ?{})", params.len() + 1, params.len() + 2);
            params.push(turso::Value::Text(hit.source_id.clone()));
            params.push(turso::Value::Text(hit.event_id.clone()));
            target
        })
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "WITH requested(position, source_id, event_id) AS (VALUES {targets}) {}",
        hydration_select()
    );
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        let index = usize::try_from(row.get::<i64>(0)?)?;
        let hit = hits
            .get_mut(index)
            .ok_or_else(|| anyhow::anyhow!("workflow row is outside its page"))?;
        let key = EventKey {
            source_id: hit.source_id.clone(),
            event_id: hit.event_id.clone(),
        };
        hit.workflow = Some(hydrated_state(&row, key)?);
        count += 1;
    }
    assert_eq!(
        count,
        hits.len(),
        "workflow page must return one state per requested event"
    );
    Ok(())
}

fn hydration_select() -> String {
    let paths = (0..4)
        .map(|offset| {
            format!(
                "(SELECT recording.path FROM recording_event_keyframes AS link
          JOIN recording_files AS recording ON recording.id = link.recording_id
          WHERE link.event_id = requested.event_id AND recording.cleanup_pending = 0
          LIMIT 1 OFFSET {offset})"
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("SELECT requested.position, COALESCE(review.reviewed, 0), COALESCE(review.dismissed, 0),
        COALESCE(review.revision, 0), review.reviewed_at_ms, review.dismissed_at_ms, review.updated_at_ms,
        bookmark.active, bookmark.note, bookmark.revision, bookmark.created_by, bookmark.created_at_ms,
        bookmark.updated_by, bookmark.updated_at_ms, bookmark.event_start_ms, bookmark.event_kind, {paths}
        FROM requested
        LEFT JOIN event_reviews AS review ON review.source_id = requested.source_id
            AND review.event_id = requested.event_id AND review.principal_id = ?1
        LEFT JOIN event_bookmarks AS bookmark ON bookmark.source_id = requested.source_id
            AND bookmark.event_id = requested.event_id")
}

fn hydrated_state(row: &turso::Row, key: EventKey) -> anyhow::Result<State> {
    let bookmark = row
        .get::<Option<i64>>(9)?
        .map(|revision| {
            anyhow::Ok(Bookmark {
                active: row.get::<i64>(7)? != 0,
                note: row.get(8)?,
                revision: to_u64(revision, "bookmark revision")?,
                created_by: row.get(10)?,
                created_at_ms: row.get(11)?,
                updated_by: row.get(12)?,
                updated_at_ms: row.get(13)?,
                event_start_ms: row.get(14)?,
                event_kind: row.get(15)?,
                audit: Vec::new(),
            })
        })
        .transpose()?;
    let mut media_available = false;
    for column in 16..20 {
        if let Some(path) = row.get::<Option<String>>(column)?
            && std::path::Path::new(&path).is_file()
        {
            media_available = true;
            break;
        }
    }
    Ok(State {
        key,
        reviewed: row.get::<i64>(1)? != 0,
        dismissed: row.get::<i64>(2)? != 0,
        review_revision: to_u64(row.get(3)?, "review revision")?,
        reviewed_at_ms: row.get(4)?,
        dismissed_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
        bookmark,
        event_present: true,
        media_available,
    })
}
