//! End-to-end verification of bounded boolean AST, hybrid semantic ranking,
//! provenance, authorization isolation, cost limits, and snapshot pagination (#142).

use keeppeek::storage::{
    EventAdvancedSearchQuery, EventEmbedding, EventEmbeddingInput, EventEmbeddingProvenance,
    EventMetadataQuery, EventSearch, EventSearchField, EventSearchTerm, EventSemanticRanking,
    RecordingCatalog,
    metadata::{EventAttachment, EventSource, TimelineEvent},
    search::{
        DEFAULT_PREVIEW_AFTER_MS, DEFAULT_PREVIEW_BEFORE_MS,
        expression::{EXPRESSION_VERSION, Expression, InvalidExpression},
        predicate::{
            ConfidenceComparison, LabelField, PredicateInput, canonical_hash, summarize_expression,
            validate_expression,
        },
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct EventSeed<'a> {
    id: &'a str,
    camera_id: &'a str,
    kind: &'a str,
    source: EventSource,
    start_time_ms: i64,
    confidence: Option<f64>,
    zone: Option<&'a str>,
    has_image: bool,
}

fn seed_event(seed: EventSeed<'_>) -> TimelineEvent {
    TimelineEvent {
        id: seed.id.to_owned(),
        revision: 1,
        camera_id: seed.camera_id.to_owned(),
        stream: Some("main".to_owned()),
        source: seed.source,
        kind: seed.kind.to_owned(),
        start_time_ms: seed.start_time_ms,
        end_time_ms: Some(seed.start_time_ms + 1_000),
        confidence: seed.confidence,
        bbox: None,
        bbox_attachment_id: None,
        zone: seed.zone.map(ToOwned::to_owned),
        text: None,
        payload: None,
        attachments: seed
            .has_image
            .then(|| EventAttachment {
                id: "thumb".to_owned(),
                attachment_type: "thumbnail".to_owned(),
                content_type: "image/jpeg".to_owned(),
                byte_len: Some(64),
                ordinal: 0,
                timestamp_ms: Some(seed.start_time_ms),
                text: None,
            })
            .into_iter()
            .collect(),
        canonical_attachment_id: seed.has_image.then(|| "thumb".to_owned()),
        icon_key: "event".to_owned(),
        rejected_icon_key: None,
        thumbnail_filename: seed.has_image.then(|| format!("{}.jpg", seed.id)),
    }
}

fn build_query(
    raw: Expression<PredicateInput>,
    authorized_cameras: Vec<String>,
    camera_groups: BTreeMap<String, BTreeSet<String>>,
    semantic: Option<EventSemanticRanking>,
    is_admin: bool,
) -> EventAdvancedSearchQuery {
    let expression = validate_expression(EXPRESSION_VERSION, &raw).expect("valid expression");
    let hash = canonical_hash(EXPRESSION_VERSION, &expression);
    let summary = summarize_expression(&expression);
    EventAdvancedSearchQuery {
        expression_version: EXPRESSION_VERSION,
        expression,
        canonical_hash: hash,
        normalized_summary: summary,
        authorized_cameras,
        camera_groups,
        stream_id: "main".to_owned(),
        start_time_ms: 0,
        end_time_ms: 100_000,
        preview_before_ms: DEFAULT_PREVIEW_BEFORE_MS,
        preview_after_ms: DEFAULT_PREVIEW_AFTER_MS,
        page_size: 50,
        page_token: None,
        include_preview_keyframes: false,
        workflow: None,
        workflow_actor: None,
        semantic,
        policy_revision: 1,
        is_admin,
        max_candidates: 10_000,
        cancelled: None,
    }
}

#[test]
fn boolean_ast_matches_truth_table_including_nulls_negation_groups_and_authorization() {
    let dir = TempDir::new("keeppeek-adv-bool");
    let catalog = RecordingCatalog::open(&dir.0.join("recordings.db")).unwrap();
    let handle = catalog.handle();
    let search = EventSearch::new(handle.clone());

    handle
        .insert_event(seed_event(EventSeed {
            id: "e1",
            camera_id: "cam-a",
            kind: "person",
            source: EventSource::KeepPeek,
            start_time_ms: 10_000,
            confidence: Some(0.92),
            zone: Some("Porch"),
            has_image: true,
        }))
        .unwrap();
    handle
        .insert_event(seed_event(EventSeed {
            id: "e2",
            camera_id: "cam-a",
            kind: "vehicle",
            source: EventSource::Camera,
            start_time_ms: 20_000,
            confidence: None,
            zone: None,
            has_image: false,
        }))
        .unwrap();
    handle
        .insert_event(seed_event(EventSeed {
            id: "e3",
            camera_id: "cam-b",
            kind: "person",
            source: EventSource::KeepPeek,
            start_time_ms: 30_000,
            confidence: Some(0.45),
            zone: Some("Driveway"),
            has_image: true,
        }))
        .unwrap();
    search
        .replace_terms(
            "e1",
            &[EventSearchTerm {
                field: EventSearchField::Text,
                value: "Delivery Courier".to_owned(),
            }],
        )
        .unwrap();

    // NOT (zone = porch) must include e2 (NULL zone) under two-valued boolean semantics,
    // and must NEVER include unauthorized cam-b (e3).
    let expr = Expression::Not(Box::new(Expression::Predicate(PredicateInput::Label(
        LabelField::Zone,
        vec!["porch".to_owned()],
    ))));
    let page = search
        .search_advanced(build_query(
            expr,
            vec!["cam-a".to_owned()],
            BTreeMap::new(),
            None,
            false,
        ))
        .unwrap();
    let ids: Vec<_> = page.hits.iter().map(|hit| hit.event_id.as_str()).collect();
    assert_eq!(ids, vec!["e2"]);
    assert!(
        page.diagnostics.is_none(),
        "non-admin must never receive planner diagnostics"
    );

    // Nested (NOT confidence >= 0.8 AND missing_zone) OR (group perimeter AND text "delivery")
    // evaluated across authorized cam-a and cam-b as admin.
    let mut groups = BTreeMap::new();
    groups.insert(
        "perimeter".to_owned(),
        BTreeSet::from(["cam-a".to_owned(), "cam-b".to_owned()]),
    );
    let nested = Expression::Or(vec![
        Expression::And(vec![
            Expression::Not(Box::new(Expression::Predicate(PredicateInput::Confidence(
                ConfidenceComparison::AtLeast,
                0.8,
            )))),
            Expression::Predicate(PredicateInput::MissingZone),
        ]),
        Expression::And(vec![
            Expression::Predicate(PredicateInput::Label(
                LabelField::Group,
                vec!["perimeter".to_owned()],
            )),
            Expression::Predicate(PredicateInput::Text {
                field: Some(EventSearchField::Text),
                prefix: "delivery".to_owned(),
            }),
        ]),
    ]);
    let admin_page = search
        .search_advanced(build_query(
            nested,
            vec!["cam-a".to_owned(), "cam-b".to_owned()],
            groups,
            None,
            true,
        ))
        .unwrap();
    let admin_ids: Vec<_> = admin_page
        .hits
        .iter()
        .map(|hit| hit.event_id.as_str())
        .collect();
    assert_eq!(admin_ids, vec!["e2", "e1"]);
    let diag = admin_page.diagnostics.expect("admin receives diagnostics");
    assert_eq!(diag.plan_category, "indexed_camera_time_boolean");
    assert_eq!(diag.scanned_candidates, 2);

    drop(search);
    drop(handle);
    catalog.shutdown();
}

#[test]
fn hybrid_semantic_enforces_exact_model_space_provenance_threshold_and_outage_isolation() {
    let dir = TempDir::new("keeppeek-adv-sem");
    let catalog = RecordingCatalog::open(&dir.0.join("recordings.db")).unwrap();
    let handle = catalog.handle();
    let search = EventSearch::new(handle.clone());

    for (id, start_ms) in [("a", 10_000), ("b", 20_000), ("c", 30_000)] {
        handle
            .insert_event(seed_event(EventSeed {
                id,
                camera_id: "cam-a",
                kind: "person",
                source: EventSource::KeepPeek,
                start_time_ms: start_ms,
                confidence: Some(0.9),
                zone: Some("Porch"),
                has_image: true,
            }))
            .unwrap();
    }
    // Same model_id, different model_version: must never mix vector spaces.
    search
        .set_embedding_with_provenance(
            "a",
            EventEmbeddingInput {
                embedding: EventEmbedding {
                    model_id: "clip".to_owned(),
                    values: vec![1.0, 0.0, 0.0],
                },
                provenance: EventEmbeddingProvenance {
                    model_version: "v2".to_owned(),
                    producer_id: "cv-service-1".to_owned(),
                    event_revision: Some(1),
                    created_at_ms: Some(11_000),
                },
            },
        )
        .unwrap();
    search
        .set_embedding_with_provenance(
            "b",
            EventEmbeddingInput {
                embedding: EventEmbedding {
                    model_id: "clip".to_owned(),
                    values: vec![0.8, 0.6, 0.0],
                },
                provenance: EventEmbeddingProvenance {
                    model_version: "v2".to_owned(),
                    producer_id: "cv-service-1".to_owned(),
                    event_revision: Some(1),
                    created_at_ms: Some(21_000),
                },
            },
        )
        .unwrap();
    search
        .set_embedding_with_provenance(
            "c",
            EventEmbeddingInput {
                embedding: EventEmbedding {
                    model_id: "clip".to_owned(),
                    values: vec![1.0, 0.0, 0.0],
                },
                provenance: EventEmbeddingProvenance {
                    model_version: "v1".to_owned(),
                    producer_id: "cv-service-1".to_owned(),
                    event_revision: Some(1),
                    created_at_ms: Some(31_000),
                },
            },
        )
        .unwrap();

    // Wrong event revision must be rejected.
    let stale_rev = search.set_embedding_with_provenance(
        "a",
        EventEmbeddingInput {
            embedding: EventEmbedding {
                model_id: "clip".to_owned(),
                values: vec![0.0, 1.0, 0.0],
            },
            provenance: EventEmbeddingProvenance {
                model_version: "v2".to_owned(),
                producer_id: "cv-service-1".to_owned(),
                event_revision: Some(9),
                created_at_ms: None,
            },
        },
    );
    assert!(stale_rev.is_err());

    // Hybrid query with producer filter + minimum_score = 0.90 (excludes b at 0.80 and c in v1).
    let expr = Expression::Predicate(PredicateInput::Label(
        LabelField::Producer,
        vec!["cv-service-1".to_owned()],
    ));
    let ranking = EventSemanticRanking {
        embedding: EventEmbedding {
            model_id: "clip".to_owned(),
            values: vec![1.0, 0.0, 0.0],
        },
        model_version: "v2".to_owned(),
        minimum_score: Some(0.90),
    };
    let page = search
        .search_advanced(build_query(
            expr.clone(),
            vec!["cam-a".to_owned()],
            BTreeMap::new(),
            Some(ranking.clone()),
            true,
        ))
        .unwrap();
    let ids: Vec<_> = page.hits.iter().map(|hit| hit.event_id.as_str()).collect();
    assert_eq!(ids, vec!["a"]);
    let sem = page.semantic_summary.expect("semantic summary returned");
    assert_eq!(sem.model_id, "clip");
    assert_eq!(sem.model_version, "v2");
    assert_eq!(sem.producer_id, "cv-service-1");
    assert_eq!(sem.dimensions, 3);
    assert_eq!(sem.candidate_count, 2);
    assert!(!sem.candidates_truncated);

    // Truncation: with 3 compatible v2 embeddings and max_candidates = 2, keep newest 2 deterministically.
    search
        .set_embedding_with_provenance(
            "c",
            EventEmbeddingInput {
                embedding: EventEmbedding {
                    model_id: "clip".to_owned(),
                    values: vec![0.6, 0.8, 0.0],
                },
                provenance: EventEmbeddingProvenance {
                    model_version: "v2".to_owned(),
                    producer_id: "cv-service-1".to_owned(),
                    event_revision: Some(1),
                    created_at_ms: Some(32_000),
                },
            },
        )
        .unwrap();
    let mut truncated_query = build_query(
        expr.clone(),
        vec!["cam-a".to_owned()],
        BTreeMap::new(),
        Some(EventSemanticRanking {
            embedding: EventEmbedding {
                model_id: "clip".to_owned(),
                values: vec![1.0, 0.0, 0.0],
            },
            model_version: "v2".to_owned(),
            minimum_score: Some(0.5),
        }),
        true,
    );
    truncated_query.max_candidates = 2;
    let truncated_page = search.search_advanced(truncated_query).unwrap();
    assert!(truncated_page.candidates_truncated);
    let truncated_sem = truncated_page.semantic_summary.expect("summary returned");
    assert!(truncated_sem.candidates_truncated);
    assert_eq!(truncated_sem.candidate_count, 2);
    let truncated_ids: Vec<_> = truncated_page
        .hits
        .iter()
        .map(|hit| hit.event_id.as_str())
        .collect();
    assert_eq!(truncated_ids, vec!["b", "c"]);

    // Delete embedding removes it from semantic search immediately.
    let deleted = search.delete_embedding("a", "clip", Some("v2")).unwrap();
    assert_eq!(deleted, 1);
    let after_delete = search
        .search_advanced(build_query(
            expr,
            vec!["cam-a".to_owned()],
            BTreeMap::new(),
            Some(ranking.clone()),
            false,
        ))
        .unwrap();
    assert!(after_delete.hits.is_empty());

    // Semantic outage disables semantic ranking while ordinary search and event writes keep working.
    handle.set_semantic_search_available(false);
    let outage = search.search_advanced(build_query(
        Expression::Predicate(PredicateInput::HasImage),
        vec!["cam-a".to_owned()],
        BTreeMap::new(),
        Some(ranking),
        false,
    ));
    assert!(outage.is_err());
    handle
        .insert_event(seed_event(EventSeed {
            id: "d",
            camera_id: "cam-a",
            kind: "motion",
            source: EventSource::Camera,
            start_time_ms: 40_000,
            confidence: None,
            zone: None,
            has_image: false,
        }))
        .unwrap();
    let ordinary = search
        .search_metadata(EventMetadataQuery::new("main", 0, 100_000))
        .unwrap();
    assert_eq!(ordinary.hits.len(), 4);

    drop(search);
    drop(handle);
    catalog.shutdown();
}

#[test]
fn snapshot_pagination_cost_bounds_and_cancellation_fail_closed() {
    let dir = TempDir::new("keeppeek-adv-page");
    let catalog = RecordingCatalog::open(&dir.0.join("recordings.db")).unwrap();
    let handle = catalog.handle();
    let search = EventSearch::new(handle.clone());

    for index in 0..5 {
        handle
            .insert_event(seed_event(EventSeed {
                id: &format!("ev-{index}"),
                camera_id: "cam-a",
                kind: "person",
                source: EventSource::KeepPeek,
                start_time_ms: 10_000 + index * 1_000,
                confidence: Some(0.8),
                zone: None,
                has_image: true,
            }))
            .unwrap();
    }

    // Candidate budget rejection before scanning unbounded rows.
    let mut over_budget = build_query(
        Expression::Predicate(PredicateInput::HasImage),
        vec!["cam-a".to_owned()],
        BTreeMap::new(),
        None,
        false,
    );
    over_budget.max_candidates = 3;
    assert!(search.search_advanced(over_budget).is_err());

    // Cancellation token aborts query cleanly.
    let mut cancelled_query = build_query(
        Expression::Predicate(PredicateInput::HasImage),
        vec!["cam-a".to_owned()],
        BTreeMap::new(),
        None,
        false,
    );
    cancelled_query.cancelled = Some(Arc::new(AtomicBool::new(true)));
    assert!(search.search_advanced(cancelled_query).is_err());

    // Snapshot pagination: page 1 -> page 2 succeeds, then policy_revision swap fails closed.
    let mut page1_query = build_query(
        Expression::Predicate(PredicateInput::HasImage),
        vec!["cam-a".to_owned()],
        BTreeMap::new(),
        None,
        false,
    );
    page1_query.page_size = 2;
    let page1 = search.search_advanced(page1_query.clone()).unwrap();
    assert_eq!(page1.hits.len(), 2);
    let token = page1.next_page_token.expect("next token exists");

    let mut page2_query = page1_query.clone();
    page2_query.page_token = Some(token.clone());
    let page2 = search.search_advanced(page2_query).unwrap();
    assert_eq!(page2.hits.len(), 2);

    let mut swapped_policy = page1_query;
    swapped_policy.policy_revision = 2;
    swapped_policy.page_token = Some(token);
    assert!(search.search_advanced(swapped_policy).is_err());

    // Contradictory range validation fails before catalog access.
    let contradiction = Expression::And(vec![
        Expression::Predicate(PredicateInput::Confidence(
            ConfidenceComparison::AtLeast,
            0.9,
        )),
        Expression::Predicate(PredicateInput::Confidence(
            ConfidenceComparison::AtMost,
            0.2,
        )),
    ]);
    assert_eq!(
        validate_expression(EXPRESSION_VERSION, &contradiction),
        Err(InvalidExpression::ContradictoryRange)
    );

    drop(search);
    drop(handle);
    catalog.shutdown();
}
