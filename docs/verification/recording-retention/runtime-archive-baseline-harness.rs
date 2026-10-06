//! Pre-feature archive control using the same synthetic archive and real-media ingest workload.
use anyhow::{Context, Result, ensure};
use keeppeek::storage::RecordingCatalog;
use std::path::Path;
#[path = "recording_retention_archive_support.rs"]
mod ingest;
const START: i64 = 3_000_000_000_000;
const SEGMENT_MS: i64 = 1_800_000;
fn main() -> Result<()> {
    let root = std::env::temp_dir().join(format!("retention-scale-baseline-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    std::thread::Builder::new().name("archive-baseline".into()).stack_size(2 * 1024 * 1024)
        .spawn(move || run(&root))?.join().map_err(|_| anyhow::anyhow!("baseline panicked"))?
}
fn run(root: &Path) -> Result<()> {
    let path = root.join("catalog.db");
    RecordingCatalog::open(&path)?.shutdown();
    // ponytail: reuse the exact bounded archive seeding and ingest verification helpers.
    seed(&path, 127, 30)?;
    seed_late_events(&path)?;
    let report = ingest::measure(&root.join("ingest"), false, Some(&path), 35)?;
    std::fs::write(root.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    eprintln!("artifact_directory={}", root.display());
    Ok(())
}
fn seed(path: &Path, sources: u32, days: u32) -> Result<()> {
    let database = pollster::block_on(
        turso::Builder::new_local(path.to_str().context("non-UTF8 fixture path")?).build(),
    )?;
    let connection = database.connect()?;
    for source in 0..sources {
        let mut values = Vec::with_capacity(256);
        for segment in 0..days * 48 {
            for stream in ["main", "sub"] {
                let start = START + i64::from(segment) * SEGMENT_MS;
                values.push(format!(
                    "('cam-{source:03}-{segment:05}-{stream}','cam-{source:03}/{stream}',
                    'cam-{source:03}','{stream}',{start},{},'metadata-only-{source:03}-{segment:05}-{stream}.mp4',0,0,1,{})",
                    start + SEGMENT_MS,
                    u8::from(segment % 48 == 0)
                ));
                if values.len() == 256 {
                    insert(
                        &connection,
                        "recording_files(id,stream_id,source_id,logical_stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,finalized,protected)",
                        &mut values,
                    )?;
                }
            }
        }
        insert(
            &connection,
            "recording_files(id,stream_id,source_id,logical_stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,finalized,protected)",
            &mut values,
        )?;
        for hour in 0..days * 24 {
            let start = START + i64::from(hour) * 3_600_000 + 1000;
            let kind = if hour % 3 == 0 { "person" } else { "motion" };
            values.push(format!("('event-{source:03}-{hour:05}','cam-{source:03}',NULL,'keeppeek','{kind}',{start},{})",start+10_000));
            if values.len() == 256 {
                insert(
                    &connection,
                    "recording_events(id,camera_id,stream,source,kind,start_time_ms,end_time_ms)",
                    &mut values,
                )?;
            }
        }
        insert(
            &connection,
            "recording_events(id,camera_id,stream,source,kind,start_time_ms,end_time_ms)",
            &mut values,
        )?;
        eprintln!("seeded_source={source} total_sources={sources}");
    }
    Ok(())
}

fn insert(connection: &turso::Connection, table: &str, values: &mut Vec<String>) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    ensure!(values.len() <= 256, "fixture SQL batch exceeds 256 rows");
    pollster::block_on(connection.execute_batch(format!(
        "BEGIN IMMEDIATE; INSERT INTO {table} VALUES {}; COMMIT;",
        values.join(",")
    )))?;
    values.clear();
    Ok(())
}


fn seed_late_events(path: &Path) -> Result<()> {
    let database = pollster::block_on(turso::Builder::new_local(
        path.to_str().context("non-UTF8 fixture path")?).build())?;
    let connection = database.connect()?;
    let start = START + 29 * 86_400_000 + SEGMENT_MS + 1000;
    let mut values = (0..35).map(|round| format!(
        "('late-{round}','cam-000',NULL,'keeppeek','person',{start},{})", start + 10_000))
        .collect::<Vec<_>>();
    insert(&connection,
        "recording_events(id,camera_id,stream,source,kind,start_time_ms,end_time_ms)",
        &mut values)
}
