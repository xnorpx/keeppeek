//! Execute bounded authorized boolean AST and hybrid semantic searches inside one snapshot.

use super::{EVENT_SEARCH_COLUMNS, attach_default_previews, event_search_hit, workflow};
use crate::storage::search::{
    EventAdvancedSearchQuery, EventSearchHit, EventSearchPage, EventSearchPlannerDiagnostics,
    EventSearchSemanticSummary, EventSemanticRanking, MAX_ADVANCED_CANDIDATES,
    MAX_SEARCH_WINDOW_MS,
};
use anyhow::ensure;
use std::{
    collections::BTreeSet,
    sync::atomic::Ordering as AtomicOrdering,
    time::{Duration, Instant},
};

mod cursor;
mod filter;

pub(super) const MAX_CAMERAS: usize = 127;
const MAX_CAMERA_ID_BYTES: usize = 256;
const DEFAULT_QUERY_DEADLINE: Duration = Duration::from_secs(5);
const SCORE_INTERPRETATION: &str = "cosine_similarity_higher_is_closer_threshold_before_topk";

const SCOPED_EVENTS: &str = "FROM recording_events AS e
    INDEXED BY recording_events_camera_time
    WHERE e.camera_id = ?1 AND e.start_time_ms >= ?2 AND e.start_time_ms < ?3
      AND (e.stream IS NULL OR e.stream = ?4)";

pub(super) struct Scope<'a> {
    pub cameras: BTreeSet<&'a str>,
    pub start_ms: i64,
    pub end_ms: i64,
}

pub(super) struct MatchedEvent {
    pub id: String,
    pub start_ms: i64,
    pub revision: i64,
    pub distance: Option<f64>,
    pub producer_id: Option<String>,
}

/// Evaluates an authorized bounded advanced boolean or hybrid semantic query.
pub(super) async fn execute(
    connection: &turso::Connection,
    query: EventAdvancedSearchQuery,
    semantic_available: bool,
) -> anyhow::Result<EventSearchPage> {
    if query.semantic.is_some() {
        ensure!(
            semantic_available,
            "semantic event search is currently unavailable"
        );
    }
    let started = Instant::now();
    let deadline = started + DEFAULT_QUERY_DEADLINE;
    let previous = query
        .page_token
        .as_deref()
        .map(cursor::decode)
        .transpose()?;
    workflow::snapshot(connection, async {
        execute_in_snapshot(connection, query, previous, started, deadline).await
    })
    .await
}

async fn execute_in_snapshot(
    connection: &turso::Connection,
    query: EventAdvancedSearchQuery,
    previous: Option<cursor::Cursor>,
    started: Instant,
    deadline: Instant,
) -> anyhow::Result<EventSearchPage> {
    let scope = Scope::new(
        &query.authorized_cameras,
        query.start_time_ms,
        query.end_time_ms,
    )?;
    let actor = query.workflow.as_ref().map(|item| item.actor_id.as_str());
    let compiled = filter::compile(
        &query.expression,
        &scope.cameras,
        actor,
        &query.camera_groups,
    )?;
    if scope.cameras.is_empty() {
        return Ok(empty_page(&query, started));
    }
    let estimated_candidates = check_candidate_budget(
        connection,
        &scope,
        &query.stream_id,
        query.max_candidates,
        deadline,
        &query,
    )
    .await?;
    let (matches, candidates_truncated, semantic_summary) =
        evaluate_matches(connection, &scope, &compiled, &query, deadline).await?;
    let scanned_candidates = u64::try_from(matches.len()).unwrap_or(u64::MAX);
    let workflow_counts =
        compute_workflow_counts(connection, query.workflow.as_ref(), &matches).await?;
    let (page_matches, next_page_token) = cursor::slice_page(&scope, &query, matches, previous)?;
    let mut hits = hydrate_hits(connection, &page_matches, &query).await?;
    if query.include_preview_keyframes {
        attach_default_previews(connection, &mut hits, &query.stream_id).await?;
    }
    workflow::hydrate(connection, query.workflow.as_ref(), &mut hits).await?;
    let diagnostics = build_diagnostics(
        query.is_admin,
        query.semantic.is_some(),
        estimated_candidates,
        scanned_candidates,
        started,
    );
    Ok(EventSearchPage {
        hits,
        next_page_token,
        candidates_truncated,
        workflow_counts,
        normalized_summary: Some(query.normalized_summary),
        semantic_summary,
        diagnostics,
    })
}

async fn evaluate_matches(
    connection: &turso::Connection,
    scope: &Scope<'_>,
    compiled: &filter::CompiledFilter,
    query: &EventAdvancedSearchQuery,
    deadline: Instant,
) -> anyhow::Result<(Vec<MatchedEvent>, bool, Option<EventSearchSemanticSummary>)> {
    match &query.semantic {
        Some(ranking) => {
            collect_hybrid_matches(connection, scope, compiled, query, ranking, deadline).await
        }
        None => {
            let mut matched =
                collect_boolean_matches(connection, scope, compiled, query, deadline).await?;
            matched.sort_by(|left, right| {
                right
                    .start_ms
                    .cmp(&left.start_ms)
                    .then_with(|| left.id.cmp(&right.id))
            });
            Ok((matched, false, None))
        }
    }
}

fn build_diagnostics(
    is_admin: bool,
    has_semantic: bool,
    estimated_candidates: u32,
    scanned_candidates: u64,
    started: Instant,
) -> Option<EventSearchPlannerDiagnostics> {
    is_admin.then(|| EventSearchPlannerDiagnostics {
        plan_category: if has_semantic {
            "indexed_camera_time_hybrid_semantic".to_owned()
        } else {
            "indexed_camera_time_boolean".to_owned()
        },
        estimated_candidates: u64::from(estimated_candidates),
        scanned_candidates,
        wall_time_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

impl<'a> Scope<'a> {
    fn new(authorized_cameras: &'a [String], start_ms: i64, end_ms: i64) -> anyhow::Result<Self> {
        ensure!(
            authorized_cameras.len() <= MAX_CAMERAS,
            "advanced search camera limit exceeded"
        );
        ensure!(
            end_ms
                .checked_sub(start_ms)
                .is_some_and(|span| (1..=MAX_SEARCH_WINDOW_MS).contains(&span)),
            "advanced search requires a bounded UTC window"
        );
        let mut cameras = BTreeSet::new();
        for camera in authorized_cameras {
            ensure!(
                (1..=MAX_CAMERA_ID_BYTES).contains(&camera.len()),
                "invalid authorized camera ID"
            );
            cameras.insert(camera.as_str());
        }
        Ok(Self {
            cameras,
            start_ms,
            end_ms,
        })
    }
}

fn check_request(deadline: Instant, query: &EventAdvancedSearchQuery) -> anyhow::Result<()> {
    if query
        .cancelled
        .as_ref()
        .is_some_and(|flag| flag.load(AtomicOrdering::Acquire))
    {
        anyhow::bail!("advanced search cancelled");
    }
    ensure!(
        Instant::now() < deadline,
        "advanced search deadline exceeded"
    );
    Ok(())
}

async fn check_candidate_budget(
    connection: &turso::Connection,
    scope: &Scope<'_>,
    stream_id: &str,
    limit: u32,
    deadline: Instant,
    query: &EventAdvancedSearchQuery,
) -> anyhow::Result<u32> {
    ensure!(
        limit > 0 && limit <= MAX_ADVANCED_CANDIDATES,
        "advanced search candidate budget is invalid"
    );
    check_request(deadline, query)?;
    let mut statement = connection
        .prepare(format!(
            "SELECT COUNT(*) FROM (SELECT e.id {SCOPED_EVENTS} LIMIT ?5)"
        ))
        .await?;
    let mut total = 0u32;
    for &camera in &scope.cameras {
        check_request(deadline, query)?;
        let remaining = limit.saturating_sub(total);
        let probe = i64::from(remaining) + 1;
        let mut rows = statement
            .query(turso::params![
                camera,
                scope.start_ms,
                scope.end_ms,
                stream_id,
                probe
            ])
            .await?;
        let count = rows
            .next()
            .await?
            .map(|row| row.get::<i64>(0))
            .transpose()?
            .unwrap_or(0);
        drop(rows);
        statement.reset()?;
        let found = u32::try_from(count.max(0)).unwrap_or(u32::MAX);
        total = total.saturating_add(found);
        ensure!(
            total <= limit,
            "advanced search candidate budget exceeded; narrow the camera or time scope"
        );
    }
    Ok(total)
}

async fn collect_boolean_matches(
    connection: &turso::Connection,
    scope: &Scope<'_>,
    compiled: &filter::CompiledFilter,
    query: &EventAdvancedSearchQuery,
    deadline: Instant,
) -> anyhow::Result<Vec<MatchedEvent>> {
    let sql = format!(
        "SELECT e.id, e.start_time_ms, e.revision {SCOPED_EVENTS} AND ({})",
        compiled.sql
    );
    let mut statement = connection.prepare(sql).await?;
    let mut matches = Vec::new();
    for &camera in &scope.cameras {
        check_request(deadline, query)?;
        let mut params = Vec::with_capacity(4 + compiled.values.len());
        params.push(turso::Value::Text(camera.to_owned()));
        params.push(turso::Value::Integer(scope.start_ms));
        params.push(turso::Value::Integer(scope.end_ms));
        params.push(turso::Value::Text(query.stream_id.clone()));
        params.extend(compiled.values.iter().cloned());
        let mut rows = statement.query(turso::params_from_iter(params)).await?;
        while let Some(row) = rows.next().await? {
            matches.push(MatchedEvent {
                id: row.get(0)?,
                start_ms: row.get(1)?,
                revision: row.get(2)?,
                distance: None,
                producer_id: None,
            });
            ensure!(
                matches.len() <= usize::try_from(query.max_candidates).unwrap_or(0),
                "advanced search candidate budget exceeded"
            );
        }
        drop(rows);
        statement.reset()?;
    }
    Ok(matches)
}

fn hybrid_query_sql(filter_sql: &str, base_len: usize) -> String {
    let model_param = base_len + 1;
    let version_param = base_len + 2;
    let dims_param = base_len + 3;
    let vector_param = base_len + 4;
    format!(
        "SELECT e.id, e.start_time_ms, e.revision, \
         vector_distance_cos(s.embedding, ?{vector_param}), s.producer_id \
         FROM recording_events AS e \
         INDEXED BY recording_events_camera_time \
         JOIN recording_event_embeddings AS s \
         INDEXED BY recording_event_embeddings_event_space \
         ON s.event_id = e.id \
         WHERE e.camera_id = ?1 AND e.start_time_ms >= ?2 AND e.start_time_ms < ?3 \
           AND (e.stream IS NULL OR e.stream = ?4) \
           AND COALESCE(s.event_revision, 1) = e.revision \
           AND s.model_id = ?{model_param} \
           AND s.model_version = ?{version_param} \
           AND s.dimensions = ?{dims_param} \
           AND ({filter_sql})"
    )
}

async fn encode_query_vector(
    connection: &turso::Connection,
    values: &[f32],
) -> anyhow::Result<Vec<u8>> {
    // Convert the query vector once into Turso's binary vector32 blob so SQL never re-parses JSON per row.
    let encoded_json = serde_json::to_string(values)?;
    let mut rows = connection
        .query("SELECT vector32(?1)", [encoded_json])
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("failed to encode query embedding vector"))?;
    row.get::<Vec<u8>>(0).map_err(Into::into)
}

async fn collect_hybrid_matches(
    connection: &turso::Connection,
    scope: &Scope<'_>,
    compiled: &filter::CompiledFilter,
    query: &EventAdvancedSearchQuery,
    ranking: &EventSemanticRanking,
    deadline: Instant,
) -> anyhow::Result<(Vec<MatchedEvent>, bool, Option<EventSearchSemanticSummary>)> {
    let base_len = 4 + compiled.values.len();
    let query_blob = encode_query_vector(connection, &ranking.embedding.values).await?;
    let dimensions = i64::try_from(ranking.embedding.values.len())?;
    let mut statement = connection
        .prepare(hybrid_query_sql(&compiled.sql, base_len))
        .await?;
    let mut candidates = Vec::new();
    for &camera in &scope.cameras {
        check_request(deadline, query)?;
        let mut params = Vec::with_capacity(base_len + 4);
        params.push(turso::Value::Text(camera.to_owned()));
        params.push(turso::Value::Integer(scope.start_ms));
        params.push(turso::Value::Integer(scope.end_ms));
        params.push(turso::Value::Text(query.stream_id.clone()));
        params.extend(compiled.values.iter().cloned());
        params.push(turso::Value::Text(ranking.embedding.model_id.clone()));
        params.push(turso::Value::Text(ranking.model_version.clone()));
        params.push(turso::Value::Integer(dimensions));
        params.push(turso::Value::Blob(query_blob.clone()));
        let mut rows = statement.query(turso::params_from_iter(params)).await?;
        while let Some(row) = rows.next().await? {
            if candidates.len().is_multiple_of(512) {
                check_request(deadline, query)?;
            }
            let distance: f64 = row.get(3)?;
            if !distance.is_finite() {
                continue;
            }
            candidates.push(MatchedEvent {
                id: row.get(0)?,
                start_ms: row.get(1)?,
                revision: row.get(2)?,
                distance: Some(distance),
                producer_id: row.get::<Option<String>>(4)?,
            });
        }
        drop(rows);
        statement.reset()?;
    }
    Ok(rank_hybrid_candidates(candidates, scope, query, ranking))
}

fn rank_hybrid_candidates(
    mut candidates: Vec<MatchedEvent>,
    scope: &Scope<'_>,
    query: &EventAdvancedSearchQuery,
    ranking: &EventSemanticRanking,
) -> (Vec<MatchedEvent>, bool, Option<EventSearchSemanticSummary>) {
    // Keep the newest compatible candidates deterministically before ranking.
    candidates.sort_by(|left, right| {
        right
            .start_ms
            .cmp(&left.start_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    let candidate_cap = usize::try_from(query.max_candidates).unwrap_or(0);
    let candidates_truncated = candidates.len() > candidate_cap;
    candidates.truncate(candidate_cap);
    let candidate_count = u64::try_from(candidates.len()).unwrap_or(u64::MAX);
    let producer_id = summarize_producers(&candidates);
    if let Some(min_score) = ranking.minimum_score {
        candidates.retain(|item| {
            item.distance
                .is_some_and(|distance| (1.0 - distance) >= min_score)
        });
    }
    candidates.sort_by(|left, right| {
        let left_d = left.distance.unwrap_or(f64::INFINITY);
        let right_d = right.distance.unwrap_or(f64::INFINITY);
        left_d
            .total_cmp(&right_d)
            .then_with(|| right.start_ms.cmp(&left.start_ms))
            .then_with(|| left.id.cmp(&right.id))
    });
    let summary = EventSearchSemanticSummary {
        model_id: ranking.embedding.model_id.clone(),
        model_version: ranking.model_version.clone(),
        producer_id,
        dimensions: u32::try_from(ranking.embedding.values.len()).unwrap_or(u32::MAX),
        score_interpretation: SCORE_INTERPRETATION.to_owned(),
        minimum_score: ranking.minimum_score,
        searched_start_time_ms: scope.start_ms,
        searched_end_time_ms: scope.end_ms,
        candidate_count,
        candidates_truncated,
    };
    (candidates, candidates_truncated, Some(summary))
}

fn summarize_producers(candidates: &[MatchedEvent]) -> String {
    let mut producers = BTreeSet::new();
    for item in candidates {
        let value = item
            .producer_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        producers.insert(value);
    }
    match producers.len() {
        0 => "unknown".to_owned(),
        1 => producers.into_iter().next().unwrap_or("unknown").to_owned(),
        _ => "multiple".to_owned(),
    }
}

async fn compute_workflow_counts(
    connection: &turso::Connection,
    query: Option<&workflow::Query>,
    matches: &[MatchedEvent],
) -> anyhow::Result<Option<workflow::Counts>> {
    let Some(workflow_query) = query else {
        return Ok(None);
    };
    if matches.is_empty() {
        return Ok(Some(workflow::Counts::default()));
    }
    let mut counts = workflow::Counts::default();
    let mut statement = connection
        .prepare(
            "SELECT COALESCE(review.reviewed, 0), COALESCE(review.dismissed, 0), \
             COALESCE(bookmark.active, 0), COALESCE(bookmark.created_by, '') \
             FROM recording_events AS e \
             LEFT JOIN event_reviews AS review ON review.source_id = e.camera_id \
               AND review.event_id = e.id AND review.principal_id = ?2 \
             LEFT JOIN event_bookmarks AS bookmark ON bookmark.source_id = e.camera_id \
               AND bookmark.event_id = e.id \
             WHERE e.id = ?1",
        )
        .await?;
    for item in matches {
        let mut rows = statement
            .query(turso::params![
                item.id.as_str(),
                workflow_query.actor_id.as_str()
            ])
            .await?;
        if let Some(row) = rows.next().await? {
            let reviewed = row.get::<i64>(0)? != 0;
            let dismissed = row.get::<i64>(1)? != 0;
            let bookmarked = row.get::<i64>(2)? != 0;
            let created_by = row.get::<String>(3)?;
            counts.total += 1;
            if !reviewed && !dismissed {
                counts.unreviewed += 1;
            }
            if reviewed {
                counts.reviewed += 1;
            }
            if dismissed {
                counts.dismissed += 1;
            }
            if bookmarked {
                counts.bookmarked += 1;
                if created_by == workflow_query.actor_id {
                    counts.bookmarked_by_me += 1;
                }
            }
        }
        drop(rows);
        statement.reset()?;
    }
    Ok(Some(counts))
}

async fn hydrate_hits(
    connection: &turso::Connection,
    page_matches: &[MatchedEvent],
    query: &EventAdvancedSearchQuery,
) -> anyhow::Result<Vec<EventSearchHit>> {
    let mut statement = connection
        .prepare(format!(
            "SELECT {EVENT_SEARCH_COLUMNS} FROM recording_events AS e WHERE e.id = ?1"
        ))
        .await?;
    let mut hits = Vec::with_capacity(page_matches.len());
    for item in page_matches {
        let mut rows = statement.query(turso::params![item.id.as_str()]).await?;
        let row = rows.next().await?.ok_or_else(|| {
            anyhow::anyhow!("advanced search snapshot changed; restart the query")
        })?;
        let hit = event_search_hit(
            &row,
            item.distance,
            query.preview_before_ms,
            query.preview_after_ms,
        )?;
        hits.push(hit);
        drop(rows);
        statement.reset()?;
    }
    Ok(hits)
}

fn empty_page(query: &EventAdvancedSearchQuery, started: Instant) -> EventSearchPage {
    let semantic_summary = query
        .semantic
        .as_ref()
        .map(|ranking| EventSearchSemanticSummary {
            model_id: ranking.embedding.model_id.clone(),
            model_version: ranking.model_version.clone(),
            producer_id: "unknown".to_owned(),
            dimensions: u32::try_from(ranking.embedding.values.len()).unwrap_or(u32::MAX),
            score_interpretation: SCORE_INTERPRETATION.to_owned(),
            minimum_score: ranking.minimum_score,
            searched_start_time_ms: query.start_time_ms,
            searched_end_time_ms: query.end_time_ms,
            candidate_count: 0,
            candidates_truncated: false,
        });
    let diagnostics = query.is_admin.then(|| EventSearchPlannerDiagnostics {
        plan_category: if query.semantic.is_some() {
            "indexed_camera_time_hybrid_semantic".to_owned()
        } else {
            "indexed_camera_time_boolean".to_owned()
        },
        estimated_candidates: 0,
        scanned_candidates: 0,
        wall_time_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    });
    EventSearchPage {
        hits: Vec::new(),
        next_page_token: None,
        candidates_truncated: false,
        workflow_counts: query.workflow.as_ref().map(|_| workflow::Counts::default()),
        normalized_summary: Some(query.normalized_summary.clone()),
        semantic_summary,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        EventSearch, RecordingCatalog,
        catalog::tests::{test_dir, test_event},
        catalog::workflow::{BookmarkChange, EventKey, Query as WorkflowQuery, ReviewChange},
        search::{
            DEFAULT_PREVIEW_AFTER_MS, DEFAULT_PREVIEW_BEFORE_MS,
            expression::{EXPRESSION_VERSION, Expression},
            predicate::{
                PredicateInput, WorkflowState, canonical_hash, summarize_expression,
                validate_expression,
            },
        },
    };
    use std::collections::BTreeMap;

    fn workflow_search_query(
        expr: crate::storage::search::expression::Expression<
            crate::storage::search::predicate::Predicate,
        >,
        actor: &str,
    ) -> EventAdvancedSearchQuery {
        EventAdvancedSearchQuery {
            expression_version: EXPRESSION_VERSION,
            canonical_hash: canonical_hash(EXPRESSION_VERSION, &expr),
            normalized_summary: summarize_expression(&expr),
            expression: expr,
            authorized_cameras: vec!["192.0.2.10".to_owned()],
            camera_groups: BTreeMap::new(),
            stream_id: "main".to_owned(),
            start_time_ms: 0,
            end_time_ms: 50_000,
            preview_before_ms: DEFAULT_PREVIEW_BEFORE_MS,
            preview_after_ms: DEFAULT_PREVIEW_AFTER_MS,
            page_size: 20,
            page_token: None,
            include_preview_keyframes: false,
            workflow: Some(WorkflowQuery::new(actor)),
            semantic: None,
            policy_revision: 1,
            is_admin: false,
            max_candidates: 10_000,
            cancelled: None,
        }
    }

    #[test]
    fn workflow_predicates_respect_principal_reviews_and_shared_bookmarks() {
        let dir = test_dir("advanced-workflow-predicates");
        let catalog = RecordingCatalog::open(&dir.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let search = EventSearch::new(handle.clone());
        handle.insert_event(test_event("w1", 10_000)).unwrap();
        handle.insert_event(test_event("w2", 20_000)).unwrap();

        handle
            .mutate_event_reviews(
                "alice",
                vec![ReviewChange {
                    key: EventKey {
                        source_id: "192.0.2.10".to_owned(),
                        event_id: "w1".to_owned(),
                    },
                    expected_revision: 0,
                    reviewed: true,
                    dismissed: false,
                }],
            )
            .unwrap();
        handle
            .mutate_event_bookmark(
                "alice",
                false,
                BookmarkChange {
                    key: EventKey {
                        source_id: "192.0.2.10".to_owned(),
                        event_id: "w2".to_owned(),
                    },
                    expected_revision: 0,
                    active: true,
                    note: "Saved".to_owned(),
                },
            )
            .unwrap();

        let raw = Expression::Or(vec![
            Expression::Predicate(PredicateInput::Workflow(WorkflowState::Reviewed)),
            Expression::Predicate(PredicateInput::Workflow(WorkflowState::BookmarkedByMe)),
        ]);
        let expr = validate_expression(EXPRESSION_VERSION, &raw).unwrap();
        let alice_page = search
            .search_advanced(workflow_search_query(expr.clone(), "alice"))
            .unwrap();
        let alice_ids: Vec<_> = alice_page
            .hits
            .iter()
            .map(|h| h.event_id.as_str())
            .collect();
        assert_eq!(alice_ids, vec!["w2", "w1"]);
        assert_eq!(alice_page.workflow_counts.as_ref().unwrap().total, 2);

        // Bob did not review w1 or create w2's bookmark -> zero hits.
        let bob_page = search
            .search_advanced(workflow_search_query(expr, "bob"))
            .unwrap();
        assert!(bob_page.hits.is_empty());

        drop(search);
        drop(handle);
        catalog.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
