//! Counts complete bounded runtime calls over an existing generated archive.

use super::*;
use keeppeek::storage::retention::settings::Settings;

pub fn measure(path: &Path) -> Result<()> {
    let counter = counter::Counter::default();
    let subscriber = tracing_subscriber::registry().with(counter.layer());
    tracing::subscriber::with_default(subscriber, || {
        let catalog = RecordingCatalog::open(path)?;
        let handle = catalog.handle();
        let index_batches = prepare_index(&handle)?;
        let settings: Settings = toml::from_str(
            "[default]\ncontinuous_days=1.0\nmotion_days=7.0\n[default.events]\nperson=31.0",
        )?;
        ensure!(
            handle.request_retention_settings(Some(&settings))?,
            "runtime profile activation rejected"
        );
        let mut samples = Vec::with_capacity(30);
        for round in 0..35 {
            let began = Instant::now();
            let (progress, counts) = counter.measure(|| handle.reconcile_retention_runtime(8));
            let elapsed_us = u64::try_from(began.elapsed().as_micros())?;
            let progress = progress?;
            ensure!(
                progress.quarantined == 0 && progress.evaluated <= 8,
                "runtime SQL profile quarantined or exceeded eight records"
            );
            ensure!(
                progress.pending && counts.executions > 0,
                "runtime profile ended unexpectedly"
            );
            if round >= 5 {
                samples.push(
                    serde_json::json!({"counts":counts,"evaluated":progress.evaluated,
                    "quarantined":progress.quarantined,"raw_us":elapsed_us}),
                );
            }
        }
        catalog.shutdown();
        let report = serde_json::json!({"warmup_runs":5,"runs":30,"samples":samples,
            "untimed_index_preparation_batches":index_batches,
            "scope":"first35 complete bounded eight-record runtime activation calls over existing127-camera30-day archive; native compilation and VM instruction-zero program starts include transactions, authority, traversal, evidence, commitments and bookkeeping; trigger/internal programs included; timing contains tracing overhead and is not performance-budget evidence; activation remains incomplete after profile"});
        std::fs::write(
            path.parent()
                .context("fixture parent missing")?
                .join("runtime-sql-count-report.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        Ok(())
    })
}
