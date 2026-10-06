//! Collects bounded stage timings from existing trace spans on a synthetic archive.

use anyhow::{Result, ensure};
use hdrhistogram::Histogram;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tracing::{
    Subscriber,
    span::{Attributes, Id},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

#[path = "../recording_retention_ingest/support.rs"]
mod ingest;

const NAMES: &[&str] = &[
    "upsert_recording",
    "insert_fragment_with_keyframe",
    "update_recording_path",
    "finalize",
    "try_enforce_storage_limit",
    "legacy_bytes",
];

#[derive(Default)]
struct Timings {
    values: Mutex<BTreeMap<&'static str, Vec<u64>>>,
    overflow: AtomicBool,
    active: AtomicBool,
}

struct Capture(Arc<Timings>);
struct Began(Instant);

impl<S> Layer<S> for Capture
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, _: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        if let Some(span) = context.span(id)
            && self.0.active.load(Ordering::Relaxed)
            && NAMES.contains(&span.metadata().name())
        {
            span.extensions_mut().insert(Began(Instant::now()));
        }
    }

    fn on_enter(&self, id: &Id, context: Context<'_, S>) {
        if context
            .span(id)
            .is_some_and(|span| span.metadata().name() == "recording_ingest_measured")
        {
            self.0.active.store(true, Ordering::Relaxed);
        }
    }

    fn on_exit(&self, id: &Id, context: Context<'_, S>) {
        if context
            .span(id)
            .is_some_and(|span| span.metadata().name() == "recording_ingest_measured")
        {
            self.0.active.store(false, Ordering::Relaxed);
        }
    }

    fn on_close(&self, id: Id, context: Context<'_, S>) {
        let Some(span) = context.span(&id) else {
            return;
        };
        let extensions = span.extensions();
        let Some(began) = extensions.get::<Began>() else {
            return;
        };
        let elapsed = u64::try_from(began.0.elapsed().as_micros()).unwrap_or(u64::MAX);
        let mut values = self
            .0
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let samples = values.entry(span.metadata().name()).or_default();
        if samples.len() == 4096 {
            self.0.overflow.store(true, Ordering::Relaxed);
            return;
        }
        samples.push(elapsed.max(1));
    }
}

pub fn measure(path: &Path) -> Result<()> {
    let timings = Arc::new(Timings::default());
    let selected = tracing_subscriber::filter::filter_fn(|metadata| {
        metadata.is_span()
            && (metadata.name() == "recording_ingest_measured"
                || (metadata.target().starts_with("keeppeek::storage")
                    && NAMES.contains(&metadata.name())))
    });
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(tracing_subscriber::filter::LevelFilter::WARN),
        )
        .with(Capture(timings.clone()).with_filter(selected))
        .try_init()
        .map_err(|error| anyhow::anyhow!("profile subscriber failed: {error}"))?;
    let root = std::env::temp_dir().join(format!("retention-ingest-{}", uuid::Uuid::new_v4()));
    let ingest = ingest::measure(&root, false, Some(path), 1);
    let values = timings
        .values
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut stages = BTreeMap::new();
    for (name, raw) in values.iter() {
        let mut samples = Histogram::<u64>::new(3)?;
        for value in raw {
            samples.record(*value)?;
        }
        stages.insert(
            *name,
            serde_json::json!({"calls":raw.len(),"raw_us":raw,
            "median_us":samples.value_at_quantile(0.5),"p95_us":samples.value_at_quantile(0.95),
            "total_us":raw.iter().map(|value| u128::from(*value)).sum::<u128>()}),
        );
    }
    let report = serde_json::json!({"ingest":ingest.as_ref().ok(),"error":ingest.as_ref().err().map(ToString::to_string),
        "stages":stages,"capture_overflow":timings.overflow.load(Ordering::Relaxed),
        "scope":"one diagnostic sample; nested stage durations overlap; only measured ingest/engine-shutdown window; raw stage capture bounded; not acceptance statistics"});
    std::fs::write(
        root.join("stages.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        !timings.overflow.load(Ordering::Relaxed),
        "stage capture bound exceeded; partial report retained"
    );
    ingest?;
    Ok(())
}
