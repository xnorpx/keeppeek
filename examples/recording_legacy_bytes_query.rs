//! Profiles legacy accounting on a closed, synthetic benchmark catalog.

use anyhow::{Context, Result, ensure};
use hdrhistogram::Histogram;
use keeppeek::storage::catalog::{
    RecordingCatalog,
    locations::{Reply, Request},
};
use std::time::Instant;
#[path = "recording_legacy_bytes_query/stages.rs"]
mod stages;

const ORIGINAL: &str = "SELECT COALESCE(SUM(file_bytes),0) FROM recording_files r
    WHERE NOT EXISTS(SELECT 1 FROM storage_volume_allocations a WHERE a.kind='recording' AND a.state!='cancelled'
        AND (a.object_id=r.id OR a.destination_path=replace(r.path,char(92),'/') COLLATE NOCASE))";
const ALL: &str = "SELECT COALESCE(SUM(file_bytes),0) FROM recording_files";
const PENDING: &str = "SELECT id,path,file_bytes,cleanup_retention_expiry FROM recording_files
    WHERE finalized=1 AND protected=0 AND cleanup_pending=1
    AND NOT EXISTS(SELECT 1 FROM storage_volume_allocations a WHERE a.kind='recording' AND a.state!='cancelled'
    AND (a.object_id=recording_files.id OR a.destination_path=replace(recording_files.path,char(92),'/') COLLATE NOCASE))
    ORDER BY started_at_ms,id LIMIT 1";

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("closed synthetic catalog path required")?;
    let canonical = std::path::Path::new(&path).canonicalize()?;
    ensure!(
        canonical.starts_with(std::env::temp_dir().canonicalize()?)
            && canonical
                .parent()
                .and_then(std::path::Path::file_name)
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with("retention-scale-")),
        "query probe requires a closed generated retention-scale fixture"
    );
    if std::env::args().nth(2).as_deref() == Some("--cleanup") {
        return cleanup_probe_index(&path);
    }
    if std::env::args().nth(2).as_deref() == Some("--pending") {
        return measure_pending(&path);
    }
    if std::env::args().nth(2).as_deref() == Some("--filesystem") {
        return measure_filesystem(canonical.parent().context("fixture parent missing")?);
    }
    if std::env::args().nth(2).as_deref() == Some("--stages") {
        return stages::measure(&canonical);
    }
    pollster::block_on(async {
        let database = turso::Builder::new_local(&path).build().await?;
        let connection = database.connect()?;
        let mut rows = connection
            .query("SELECT COUNT(*) FROM storage_volume_allocations", ())
            .await?;
        ensure!(
            rows.next().await?.context("count missing")?.get::<i64>(0)? == 0,
            "probe requires the no-allocation synthetic fixture"
        );
        drop(rows);
        let original = measure(&connection, "original", ORIGINAL).await?;
        ensure!(
            measure(&connection, "sum_without_index", ALL).await? == original,
            "sum differs"
        );
        let began = Instant::now();
        connection.execute_batch("CREATE INDEX IF NOT EXISTS recording_legacy_bytes_probe ON recording_files(file_bytes)").await?;
        eprintln!("probe_index_build_ms={}", began.elapsed().as_millis());
        ensure!(
            measure(&connection, "sum_with_covering_index", ALL).await? == original,
            "indexed sum differs"
        );
        ensure!(
            measure(&connection, "original_with_index", ORIGINAL).await? == original,
            "original differs"
        );
        anyhow::Ok(())
    })?;
    measure_catalog_api(std::path::Path::new(&path))?;
    cleanup_probe_index(&path)
}

fn measure_filesystem(path: &std::path::Path) -> Result<()> {
    let mut samples = Histogram::<u64>::new(3)?;
    let mut raw = Vec::with_capacity(30);
    for round in 0..35 {
        let began = Instant::now();
        ensure!(
            std::fs::metadata(path)?.is_dir(),
            "filesystem probe root missing"
        );
        let capacity = fs4::statvfs(path)?;
        ensure!(
            capacity.available_space() <= capacity.total_space(),
            "filesystem capacity invalid"
        );
        let elapsed = u64::try_from(began.elapsed().as_micros())?.max(1);
        if round >= 5 {
            samples.record(elapsed)?;
            raw.push(elapsed);
        }
    }
    println!(
        "{}",
        serde_json::json!({"name":"filesystem_capacity","warmups":5,"runs":30,
        "raw_us":raw,"median_us":samples.value_at_quantile(0.5),
        "p95_us":samples.value_at_quantile(0.95),"max_us":samples.max()})
    );
    Ok(())
}

fn measure_pending(path: &str) -> Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        let indexed = PENDING.replace(
            "FROM recording_files\n",
            "FROM recording_files INDEXED BY recording_retention_legacy_claims\n",
        );
        for (name, sql) in [
            ("pending_original", PENDING),
            ("pending_indexed", indexed.as_str()),
        ] {
            let mut plan = connection
                .query(format!("EXPLAIN QUERY PLAN {sql}"), ())
                .await?;
            let mut details = Vec::new();
            while let Some(row) = plan.next().await? {
                details.push(row.get::<String>(3)?);
            }
            drop(plan);
            let mut raw = Vec::with_capacity(5);
            for _ in 0..5 {
                let began = Instant::now();
                let mut rows = connection.query(sql, ()).await?;
                ensure!(
                    rows.next().await?.is_none(),
                    "probe requires no pending cleanup claims"
                );
                raw.push(began.elapsed().as_micros());
            }
            println!(
                "{}",
                serde_json::json!({"name":name,"plan":details,"raw_us":raw})
            );
        }
        anyhow::Ok(())
    })
}

fn cleanup_probe_index(path: &str) -> Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        // ponytail: Remove the probe-owned index so later ingest uses production schema indexes.
        connection
            .execute_batch("DROP INDEX IF EXISTS recording_legacy_bytes_probe")
            .await?;
        anyhow::Ok(())
    })
}

fn measure_catalog_api(path: &std::path::Path) -> Result<()> {
    let catalog = RecordingCatalog::open(path)?;
    let handle = catalog.handle();
    let mut samples = Histogram::<u64>::new(3)?;
    let mut raw = Vec::with_capacity(30);
    let mut expected = None;
    for round in 0..35 {
        let began = Instant::now();
        let Reply::Bytes(bytes) = handle.volume_location(Request::LegacyRecordingBytes)? else {
            anyhow::bail!("legacy accounting reply missing");
        };
        let elapsed = u64::try_from(began.elapsed().as_micros())?.max(1);
        ensure!(
            expected.is_none_or(|expected| expected == bytes),
            "accounting changed during probe"
        );
        expected = Some(bytes);
        if round >= 5 {
            samples.record(elapsed)?;
            raw.push(elapsed);
        }
    }
    catalog.shutdown();
    println!(
        "{}",
        serde_json::json!({"name":"catalog_legacy_bytes_api","warmups":5,"runs":30,
        "bytes":expected,"raw_us":raw,"median_us":samples.value_at_quantile(0.5),
        "p95_us":samples.value_at_quantile(0.95),"max_us":samples.max()})
    );
    Ok(())
}

async fn measure(connection: &turso::Connection, name: &str, sql: &str) -> Result<i64> {
    let mut plan = connection
        .query(format!("EXPLAIN QUERY PLAN {sql}"), ())
        .await?;
    let mut details = Vec::new();
    while let Some(row) = plan.next().await? {
        details.push(row.get::<String>(3)?);
    }
    drop(plan);
    let mut raw = Vec::with_capacity(5);
    let mut result = None;
    for _ in 0..5 {
        let began = Instant::now();
        let mut rows = connection.query(sql, ()).await?;
        let value = rows.next().await?.context("sum missing")?.get::<i64>(0)?;
        ensure!(
            result.is_none_or(|expected| expected == value),
            "sum changed during probe"
        );
        result = Some(value);
        raw.push(began.elapsed().as_micros());
    }
    println!(
        "{}",
        serde_json::json!({"name":name,"plan":details,"raw_us":raw,"bytes":result})
    );
    result.context("no measurements")
}
