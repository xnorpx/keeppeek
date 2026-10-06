//! Qualifies native SQL compilation counts without retaining SQL text or parameters.

use anyhow::{Context, Result, ensure};
use keeppeek::storage::{
    RecordingCatalog,
    retention::{Policy, Predicate, Rule},
};
use std::{path::Path, time::Instant};
use tracing_subscriber::prelude::*;

#[path = "recording_retention_sql/counter.rs"]
mod counter;
#[path = "recording_retention_sql/runtime.rs"]
mod runtime;

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("provide a closed generated archive catalog")?;
    let path = std::path::PathBuf::from(path).canonicalize()?;
    ensure!(
        path.is_file()
            && path.starts_with(std::env::temp_dir().canonicalize()?)
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("retention-scale-")),
        "catalog must belong to a closed generated retention-scale fixture"
    );
    let runtime = std::env::args()
        .nth(2)
        .is_some_and(|mode| mode == "runtime");
    std::thread::Builder::new()
        .name("retention-sql".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            if runtime {
                runtime::measure(&path)
            } else {
                measure(&path)
            }
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("SQL count measurement panicked"))?
}

fn measure(path: &Path) -> Result<()> {
    let counter = counter::Counter::default();
    let subscriber = tracing_subscriber::registry().with(counter.layer());
    tracing::subscriber::with_default(subscriber, || {
        let catalog = RecordingCatalog::open(path)?;
        let handle = catalog.handle();
        let preparation = Instant::now();
        let index_batches = prepare_index(&handle)?;
        let index_preparation_us = u64::try_from(preparation.elapsed().as_micros())?;
        let id = "cam-000-00006-main";
        let revision = handle
            .retention_decision(id)?
            .map_or(1, |previous| previous.policy_revision + 1);
        let policy = Policy::new(vec![
            Rule::new("continuous", 86_400_000, Predicate::Continuous)?,
            Rule::new("motion", 7 * 86_400_000, Predicate::Motion)?,
            Rule::new(
                "person",
                31 * 86_400_000,
                Predicate::Event {
                    event_type: "person".into(),
                },
            )?,
        ])?;
        let mut counts = Vec::with_capacity(30);
        let mut elapsed_us = Vec::with_capacity(30);
        for round in 0..35 {
            let began = Instant::now();
            let (decision, count) =
                counter.measure(|| handle.commit_retention(id, revision, &policy));
            let elapsed = u64::try_from(began.elapsed().as_micros())?;
            let decision = decision?;
            ensure!(
                decision.deadline_ms == Some(3_000_000_000_000 + 7 * 1_800_000 + 31 * 86_400_000),
                "SQL qualification changed the expected committed floor"
            );
            ensure!(count.executions > 0, "native SQL tracing is unavailable");
            if round >= 5 {
                counts.push(count);
                elapsed_us.push(elapsed);
            }
        }
        catalog.shutdown();
        let report = serde_json::json!({"warmup_runs":5,"runs":30,"native_sql_counts":counts,
            "untimed_index_preparation_batches":index_batches,"untimed_index_preparation_us":index_preparation_us,
            "raw_us":elapsed_us,"recording_id":id,"catalog":path,
            "scope":"full native compilation and instruction-zero program-start count inside synchronous commitment including transaction/authority/evidence/decision statements; SQL text/parameters are not retained; calibration excludes empty batch tails; not SQL VM trigger opcode counts; timing includes tracing overhead; startup excluded"});
        println!("{}", serde_json::to_string_pretty(&report)?);
        let output = path
            .parent()
            .context("fixture parent missing")?
            .join("sql-count-report.json");
        std::fs::write(output, serde_json::to_vec_pretty(&report)?)?;
        Ok(())
    })
}

fn prepare_index(handle: &keeppeek::storage::RecordingCatalogHandle) -> Result<u64> {
    for batch in 1..=400 {
        if handle.reconcile_retention_events(256)? {
            return Ok(batch);
        }
    }
    anyhow::bail!("generated archive index preparation exceeded 400 bounded batches")
}
