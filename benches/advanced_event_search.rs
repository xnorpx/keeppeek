//! Reproducible benchmark comparing flat metadata search vs advanced boolean AST
//! and 10k-candidate 4096-dim hybrid semantic ranking on 127 cameras and 1M seeded events.

use keeppeek::storage::{
    EventAdvancedSearchQuery, EventEmbedding, EventEmbeddingInput, EventEmbeddingProvenance,
    EventMetadataQuery, EventSearch, EventSemanticRanking, RecordingCatalog,
    metadata::{EventSource, TimelineEvent},
    search::{
        DEFAULT_PREVIEW_AFTER_MS, DEFAULT_PREVIEW_BEFORE_MS,
        expression::{EXPRESSION_VERSION, Expression},
        predicate::{
            ConfidenceComparison, LabelField, PredicateInput, canonical_hash, summarize_expression,
            validate_expression,
        },
    },
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const CAMERAS: usize = 127;
const TOTAL_EVENTS: usize = 1_000_000;
const SEMANTIC_CANDIDATES: usize = 10_000;
const SEMANTIC_DIMS: usize = 4_096;
const RUNS: usize = 30;

fn main() -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("advanced-event-search-bench".to_owned())
        .stack_size(8 * 1024 * 1024)
        .spawn(run_bench)?
        .join()
        .map_err(|_| anyhow::anyhow!("benchmark thread panicked"))?
}

fn run_bench() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-bench-advanced-search-{}",
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("bench-events.db");
    let total_events = std::env::var("KEEPPEEK_BENCH_EVENTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(TOTAL_EVENTS);
    let semantic_count = std::env::var("KEEPPEEK_BENCH_SEMANTIC")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(SEMANTIC_CANDIDATES);

    // Seed directly via Turso transaction so 1M events populate quickly, then open via RecordingCatalog.
    let seed_start = Instant::now();
    {
        let catalog = RecordingCatalog::open(&db_path)?;
        catalog.shutdown();
    }
    seed_database(&db_path, total_events, semantic_count)?;
    let seed_ms = seed_start.elapsed().as_secs_f64() * 1_000.0;

    let catalog = RecordingCatalog::open(&db_path)?;
    let handle = catalog.handle();
    let search = EventSearch::new(handle.clone());

    let visible_cameras: Vec<String> = (0..64).map(|idx| format!("cam-{idx:03}")).collect();
    let target_window_start = 1_780_000_000_000_i64;
    let target_window_end = target_window_start + 10_000;

    // 1. Baseline flat metadata query on one authorized window (mixed visibility + missing metadata).
    let mut flat_query = EventMetadataQuery::new("main", target_window_start, target_window_end);
    flat_query.source_ids = visible_cameras.clone();
    flat_query.event_types = vec!["person".to_owned()];
    flat_query.minimum_confidence = Some(0.8);
    flat_query.page_size = 50;

    let flat_times = measure_runs(RUNS, || {
        let page = search.search_metadata(flat_query.clone())?;
        Ok(page.hits.len())
    })?;

    // 2. Equivalent advanced boolean AST query.
    let ast_raw = Expression::And(vec![
        Expression::Predicate(PredicateInput::Label(
            LabelField::Kind,
            vec!["person".to_owned()],
        )),
        Expression::Predicate(PredicateInput::Confidence(
            ConfidenceComparison::AtLeast,
            0.8,
        )),
    ]);
    let ast_expr = validate_expression(EXPRESSION_VERSION, &ast_raw)?;
    let ast_query = EventAdvancedSearchQuery {
        expression_version: EXPRESSION_VERSION,
        canonical_hash: canonical_hash(EXPRESSION_VERSION, &ast_expr),
        normalized_summary: summarize_expression(&ast_expr),
        expression: ast_expr,
        authorized_cameras: visible_cameras,
        camera_groups: BTreeMap::new(),
        stream_id: "main".to_owned(),
        start_time_ms: target_window_start,
        end_time_ms: target_window_end,
        preview_before_ms: DEFAULT_PREVIEW_BEFORE_MS,
        preview_after_ms: DEFAULT_PREVIEW_AFTER_MS,
        page_size: 50,
        page_token: None,
        include_preview_keyframes: false,
        workflow: None,
        semantic: None,
        policy_revision: 1,
        is_admin: true,
        max_candidates: 10_000,
        cancelled: None,
    };

    let ast_times = measure_runs(RUNS, || {
        let page = search.search_advanced(ast_query.clone())?;
        Ok(page.hits.len())
    })?;

    // 3. Over-budget scan rejection across all 127 cameras / full window.
    let mut over_budget = ast_query;
    over_budget.authorized_cameras = (0..CAMERAS).map(|idx| format!("cam-{idx:03}")).collect();
    over_budget.start_time_ms = target_window_start;
    over_budget.end_time_ms = target_window_start + 86_400_000;
    over_budget.max_candidates =
        u32::try_from((total_events / 2).clamp(1, 10_000)).unwrap_or(10_000);
    let reject_times = measure_runs(RUNS, || {
        let err = search.search_advanced(over_budget.clone()).unwrap_err();
        anyhow::ensure!(
            err.to_string().contains("candidate budget"),
            "unexpected over-budget error: {err}"
        );
        Ok(0)
    })?;

    // 4. Hybrid semantic ranking over 10k compatible 4096-dim candidates.
    let mut query_vec = vec![0.0_f32; SEMANTIC_DIMS];
    query_vec[0] = 1.0;
    query_vec[1] = 0.5;
    let semantic_window_start = 1_780_100_000_000_i64;
    let semantic_window_end = semantic_window_start + 3_600_000;
    let sem_raw = Expression::Predicate(PredicateInput::Label(
        LabelField::Producer,
        vec!["cv-bench".to_owned()],
    ));
    let sem_expr = validate_expression(EXPRESSION_VERSION, &sem_raw)?;
    let sem_query = EventAdvancedSearchQuery {
        expression_version: EXPRESSION_VERSION,
        canonical_hash: canonical_hash(EXPRESSION_VERSION, &sem_expr),
        normalized_summary: summarize_expression(&sem_expr),
        expression: sem_expr,
        authorized_cameras: vec!["cam-000".to_owned()],
        camera_groups: BTreeMap::new(),
        stream_id: "main".to_owned(),
        start_time_ms: semantic_window_start,
        end_time_ms: semantic_window_end,
        preview_before_ms: DEFAULT_PREVIEW_BEFORE_MS,
        preview_after_ms: DEFAULT_PREVIEW_AFTER_MS,
        page_size: 50,
        page_token: None,
        include_preview_keyframes: false,
        workflow: None,
        semantic: Some(EventSemanticRanking {
            embedding: EventEmbedding {
                model_id: "clip-bench".to_owned(),
                values: query_vec,
            },
            model_version: "v2".to_owned(),
            minimum_score: Some(0.5),
        }),
        policy_revision: 1,
        is_admin: true,
        max_candidates: 10_000,
        cancelled: None,
    };

    let semantic_times = measure_runs(RUNS, || {
        let page = search.search_advanced(sem_query.clone())?;
        Ok(page.hits.len())
    })?;

    // 5. Catalog write latency (event insert + 4096-dim embedding write).
    let mut write_vec = vec![0.0_f32; SEMANTIC_DIMS];
    write_vec[0] = 0.9;
    let write_times = measure_runs(RUNS, || {
        let id = format!("bench-write-{}", rand::random::<u64>());
        handle.insert_event(TimelineEvent {
            id: id.clone(),
            revision: 1,
            camera_id: "cam-000".to_owned(),
            stream: Some("main".to_owned()),
            source: EventSource::KeepPeek,
            kind: "person".to_owned(),
            start_time_ms: target_window_start + 100,
            end_time_ms: Some(target_window_start + 1_100),
            confidence: Some(0.95),
            bbox: None,
            bbox_attachment_id: None,
            zone: Some("Porch".to_owned()),
            text: None,
            payload: None,
            attachments: Vec::new(),
            canonical_attachment_id: None,
            icon_key: "person".to_owned(),
            rejected_icon_key: None,
            thumbnail_filename: None,
        })?;
        search.set_embedding_with_provenance(
            &id,
            EventEmbeddingInput {
                embedding: EventEmbedding {
                    model_id: "clip-bench".to_owned(),
                    values: write_vec.clone(),
                },
                provenance: EventEmbeddingProvenance {
                    model_version: "v2".to_owned(),
                    producer_id: "cv-bench".to_owned(),
                    event_revision: Some(1),
                    created_at_ms: Some(target_window_start + 200),
                },
            },
        )?;
        Ok(1)
    })?;

    let mut sys = sysinfo::System::new().map_err(|err| anyhow::anyhow!("{err:?}"))?;
    let pid = sysinfo::get_current_pid().map_err(|err| anyhow::anyhow!(err))?;
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    let rss_mib = sys
        .process(pid)
        .map_or(0.0, |proc| proc.memory() as f64 / (1024.0 * 1024.0));

    println!(
        "dataset: cameras={CAMERAS} events={total_events} semantic_candidates={semantic_count} dims={SEMANTIC_DIMS} runs={RUNS} seed_ms={seed_ms:.1}"
    );
    print_stats("flat_metadata", &flat_times);
    print_stats("advanced_ast", &ast_times);
    print_stats("over_budget_reject", &reject_times);
    print_stats("hybrid_semantic_10k_4096d", &semantic_times);
    print_stats("catalog_write_event_plus_4096d", &write_times);
    println!("process_rss_mib={rss_mib:.1}");

    catalog.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn seed_database(
    db_path: &std::path::Path,
    total_events: usize,
    semantic_count: usize,
) -> anyhow::Result<()> {
    let db_path = db_path.to_path_buf();
    std::thread::Builder::new()
        .name("bench-seed".to_owned())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || seed_database_on_worker(&db_path, total_events, semantic_count))?
        .join()
        .map_err(|_| anyhow::anyhow!("bench-seed thread panicked"))?
}

fn seed_database_on_worker(
    db_path: &std::path::Path,
    total_events: usize,
    semantic_count: usize,
) -> anyhow::Result<()> {
    pollster::block_on(async {
        let db = turso::Builder::new_local(db_path.to_str().unwrap())
            .build()
            .await?;
        let conn = db.connect()?;
        let _ = conn.query("PRAGMA synchronous = OFF", ()).await?;
        let base_start = 1_780_000_000_000_i64;
        let semantic_start = 1_780_100_000_000_i64;
        let batch_size = 5_000;
        let mut inserted = 0usize;
        while inserted < total_events {
            let end = (inserted + batch_size).min(total_events);
            conn.execute_batch("BEGIN IMMEDIATE").await?;
            let mut stmt = conn
                .prepare(
                    "INSERT INTO recording_events (
                         id, revision, camera_id, stream, source, kind, start_time_ms,
                         end_time_ms, confidence, zone, icon_key, attachments_json
                     ) VALUES (?1, 1, ?2, 'main', ?3, ?4, ?5, ?6, ?7, ?8, 'event', '[]')",
                )
                .await?;
            for idx in inserted..end {
                let cam = format!("cam-{:03}", idx % CAMERAS);
                let start = base_start + i64::try_from((idx / CAMERAS) * 100)?;
                let kind = if idx % 3 == 0 { "person" } else { "motion" };
                let source = if idx % 2 == 0 { "keeppeek" } else { "camera" };
                let confidence: Option<f64> = if idx % 5 == 0 {
                    None
                } else {
                    Some(((idx % 50) as f64).mul_add(0.01, 0.5))
                };
                let zone: Option<&str> = if idx % 4 == 0 { None } else { Some("Porch") };
                stmt.execute(turso::params![
                    format!("ev-{idx}"),
                    cam,
                    source,
                    kind,
                    start,
                    start + 500,
                    confidence,
                    zone
                ])
                .await?;
                stmt.reset()?;
            }
            drop(stmt);
            conn.execute_batch("COMMIT").await?;
            inserted = end;
        }

        // Seed semantic candidates with 4096-dim vectors on cam-000.
        let mut vec_a = vec![0.0_f32; SEMANTIC_DIMS];
        vec_a[0] = 1.0;
        vec_a[1] = 0.25;
        let encoded_a = serde_json::to_string(&vec_a)?;
        let mut vec_b = vec![0.0_f32; SEMANTIC_DIMS];
        vec_b[1] = 1.0;
        let encoded_b = serde_json::to_string(&vec_b)?;
        let mut sem_done = 0usize;
        while sem_done < semantic_count {
            let end = (sem_done + 500).min(semantic_count);
            conn.execute_batch("BEGIN IMMEDIATE").await?;
            let mut ev_stmt = conn
                .prepare(
                    "INSERT INTO recording_events (
                         id, revision, camera_id, stream, source, kind, start_time_ms,
                         end_time_ms, confidence, zone, icon_key, attachments_json
                     ) VALUES (?1, 1, 'cam-000', 'main', 'keeppeek', 'person', ?2, ?3, 0.9, 'Porch', 'person', '[]')",
                )
                .await?;
            let mut emb_stmt = conn
                .prepare(
                    "INSERT INTO recording_event_embeddings (
                         event_id, model_id, model_version, producer_id, created_at_ms,
                         dimensions, embedding, event_revision
                     ) VALUES (?1, 'clip-bench', 'v2', 'cv-bench', ?2, ?3, vector32(?4), 1)",
                )
                .await?;
            for idx in sem_done..end {
                let id = format!("sem-{idx}");
                let start = semantic_start + i64::try_from(idx * 100)?;
                ev_stmt
                    .execute(turso::params![id.clone(), start, start + 500])
                    .await?;
                ev_stmt.reset()?;
                let payload = if idx % 2 == 0 {
                    encoded_a.as_str()
                } else {
                    encoded_b.as_str()
                };
                emb_stmt
                    .execute(turso::params![
                        id,
                        start,
                        i64::try_from(SEMANTIC_DIMS)?,
                        payload
                    ])
                    .await?;
                emb_stmt.reset()?;
            }
            drop(ev_stmt);
            drop(emb_stmt);
            conn.execute_batch("COMMIT").await?;
            sem_done = end;
        }
        Ok::<(), anyhow::Error>(())
    })
}

fn measure_runs(
    runs: usize,
    mut op: impl FnMut() -> anyhow::Result<usize>,
) -> anyhow::Result<Vec<Duration>> {
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let start = Instant::now();
        let _ = op()?;
        times.push(start.elapsed());
    }
    Ok(times)
}

fn print_stats(name: &str, samples: &[Duration]) {
    let mut ms: Vec<f64> = samples
        .iter()
        .map(|duration| duration.as_secs_f64() * 1_000.0)
        .collect();
    ms.sort_by(f64::total_cmp);
    let p50 = ms[ms.len() / 2];
    let p95 = ms[(ms.len() * 95) / 100];
    let max = *ms.last().unwrap_or(&0.0);
    println!("{name}: p50_ms={p50:.3} p95_ms={p95:.3} max_ms={max:.3}");
}
