//! Measures metadata-only retention lookup latency over a closed historical event archive.

use anyhow::{Result, ensure};
use hdrhistogram::Histogram;
use keeppeek::storage::catalog::{CatalogRecording, RecordingCatalog};
use keeppeek::storage::retention::{Policy, Predicate, Rule};
use std::{path::Path, time::Instant};

fn main() -> Result<()> {
    let count: u32 = std::env::args()
        .nth(1)
        .map_or(Ok(50_000), |value| value.parse())?;
    ensure!(
        (1..=1_000_000).contains(&count),
        "archive size must be 1 to 1000000"
    );
    // ponytail: Use the Rust test thread's stack size for this debug-only catalog harness.
    std::thread::Builder::new()
        .name("retention-query-benchmark".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || run(count))?
        .join()
        .map_err(|_| anyhow::anyhow!("retention query benchmark panicked"))?
}

fn run(count: u32) -> Result<()> {
    let root =
        std::env::temp_dir().join(format!("keeppeek-retention-query-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let result = measure(&root.join("catalog.db"), count);
    std::fs::remove_dir_all(&root)?;
    result
}

fn measure(path: &Path, count: u32) -> Result<()> {
    let catalog = RecordingCatalog::open(path)?;
    catalog.shutdown();
    seed(path, count)?;
    let catalog = RecordingCatalog::open(path)?;
    let handle = catalog.handle();
    let preparation = Instant::now();
    let mut complete = false;
    for _ in 0..(count.div_ceil(256) + 2) {
        if handle.reconcile_retention_events(256)? {
            complete = true;
            break;
        }
    }
    ensure!(complete, "retention event index migration did not complete");
    println!("index_preparation_ms={}", preparation.elapsed().as_millis());
    let start = i64::from(count) * 1_000 + 1_000;
    handle.upsert_recording(CatalogRecording {
        id: "recording".into(),
        stream_id: "front/main".into(),
        source_id: Some("front".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: start,
        ended_at_ms: Some(start + 10_000),
        path: "metadata-only.mp4".into(),
        init_offset: 0,
        init_len: 0,
        finalized: true,
    })?;
    let policy = Policy::new(vec![Rule::new("motion", 50_000, Predicate::Motion)?])?;
    let mut samples = Histogram::<u64>::new(3)?;
    for round in 0..35 {
        let began = Instant::now();
        let decision = handle.commit_retention("recording", 1, &policy)?;
        let micros = u64::try_from(began.elapsed().as_micros())?;
        ensure!(
            decision.deadline_ms == Some(start + 60_000),
            "incorrect retained deadline"
        );
        if round >= 5 {
            samples.record(micros)?;
        }
    }
    println!(
        "historical_events={count} samples={} warmup=5 median_us={} p95_us={} max_us={}",
        samples.len(),
        samples.value_at_quantile(0.5),
        samples.value_at_quantile(0.95),
        samples.max()
    );
    catalog.shutdown();
    Ok(())
}

fn seed(path: &Path, count: u32) -> Result<()> {
    let database = pollster::block_on(
        turso::Builder::new_local(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("benchmark path must be UTF-8"))?,
        )
        .build(),
    )?;
    let connection = database.connect()?;
    let started = Instant::now();
    seed_events(&connection, count)?;
    println!("seed_ms={}", started.elapsed().as_millis());
    let mut plan = pollster::block_on(connection.query(
        "EXPLAIN QUERY PLAN SELECT id FROM recording_events
         WHERE camera_id = 'front' AND (stream IS NULL OR stream = 'main')
           AND start_time_ms < ?1 AND (end_time_ms IS NULL OR end_time_ms > ?2
                OR (end_time_ms = start_time_ms AND start_time_ms >= ?2))
         ORDER BY start_time_ms, id LIMIT 257",
        turso::params![
            i64::from(count) * 1_000 + 11_000,
            i64::from(count) * 1_000 + 1_000
        ],
    ))?;
    for _ in 0..16 {
        let Some(row) = pollster::block_on(plan.next())? else {
            return Ok(());
        };
        println!("legacy_plan={}", row.get::<String>(3)?);
    }
    anyhow::bail!("query plan exceeds 16 rows")
}

fn seed_events(connection: &turso::Connection, count: u32) -> Result<()> {
    pollster::block_on(connection.execute_batch("BEGIN IMMEDIATE"))?;
    let result = (|| {
        // ponytail: Bound each SQL insert to 256 rows instead of recursive query execution.
        for first in (1..=count).step_by(256) {
            let last = count.min(first + 255);
            let values = (first..=last)
                .map(|n| {
                    format!(
                        "('old-{n}','front',NULL,'keeppeek','motion',{}, {})",
                        i64::from(n) * 1000,
                        i64::from(n) * 1000 + 500,
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            pollster::block_on(connection.execute_batch(&format!(
                "INSERT INTO recording_events(id,camera_id,stream,source,kind,start_time_ms,end_time_ms)
                 VALUES {values}")))?;
        }
        let start = i64::from(count) * 1000 + 1000;
        pollster::block_on(connection.execute(
            "INSERT INTO recording_events(id,camera_id,stream,source,kind,start_time_ms,end_time_ms)
             VALUES ('current','front',NULL,'keeppeek','motion',?1,?2)",
            turso::params![start + 100, start + 1000],
        ))?;
        pollster::block_on(connection.execute_batch("COMMIT"))?;
        Ok(())
    })();
    if result.is_err() {
        pollster::block_on(connection.execute_batch("ROLLBACK"))?;
    }
    result
}
