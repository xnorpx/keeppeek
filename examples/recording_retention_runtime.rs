//! Measures bounded metadata reconciliation over a 127-source, 30-day recording archive.
//! Historical media is synthetic; ingest uses real H.264 frames. Physical removal is excluded.

use anyhow::{Context, Result, ensure};
use hdrhistogram::Histogram;
use keeppeek::storage::{RecordingCatalog, RecordingCatalogHandle, retention::settings::Settings};
use serde_json::json;
use std::{path::Path, time::Instant};

// Use a future epoch so the production expiry clock cannot remove synthetic archive rows.
const START: i64 = 3_000_000_000_000;
#[path = "recording_retention_ingest/support.rs"]
mod ingest;
const SEGMENT_MS: i64 = 1_800_000;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow::anyhow!("benchmark tracing initialization failed: {error}"))?;
    let sources = argument(1, 127, 127)?;
    let days = argument(2, 30, 30)?;
    let root = std::env::temp_dir().join(format!("retention-scale-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    std::thread::Builder::new()
        .name("retention-scale".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || run(&root, sources, days))?
        .join()
        .map_err(|_| anyhow::anyhow!("retention scale harness panicked"))?
}

fn argument(index: usize, default: u32, max: u32) -> Result<u32> {
    let value = std::env::args()
        .nth(index)
        .map_or(Ok(default), |value| value.parse())?;
    ensure!(
        (1..=max).contains(&value),
        "argument {index} must be 1 to {max}"
    );
    Ok(value)
}

fn seed(path: &Path, sources: u32, days: u32) -> Result<()> {
    let database = pollster::block_on(
        turso::Builder::new_local(path.to_str().context("non-UTF8 fixture path")?).build(),
    )?;
    let connection = database.connect()?;
    prepare_seed(&connection)?;
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

fn rss(system: &mut sysinfo::System) -> u64 {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .expect("benchmark process exists")
        .memory()
}

fn reconcile(
    handle: &RecordingCatalogHandle,
    limit: u64,
    resources: &mut sysinfo::System,
) -> Result<serde_json::Value> {
    let started = Instant::now();
    let mut samples = Histogram::<u64>::new(3)?;
    let mut evaluated = 0_u64;
    let mut peak = rss(resources);
    for call in 0..limit {
        let began = Instant::now();
        let step = handle.reconcile_retention_runtime(8)?;
        ensure!(step.quarantined == 0, "scale fixture quarantined evidence");
        evaluated += u64::from(step.evaluated);
        if call >= 5 {
            samples.record(u64::try_from(began.elapsed().as_micros())?.max(1))?;
        }
        if call % 128 == 0 {
            peak = peak.max(rss(resources));
        }
        if !step.pending {
            return Ok(
                json!({"elapsed_ms":started.elapsed().as_millis(),"evaluated":evaluated,
                "calls":call+1,"warmup_calls":(call+1).min(5),"measured_calls":samples.len(),
                "batch_median_us":(!samples.is_empty()).then(|| samples.value_at_quantile(0.5)),
                "batch_p95_us":(!samples.is_empty()).then(|| samples.value_at_quantile(0.95)),
                "batch_max_us":(!samples.is_empty()).then(|| samples.max()),
                "batch_histogram_us":samples.iter_recorded().map(|value|
                    [value.value_iterated_to(),value.count_since_last_iteration()]).collect::<Vec<_>>(),
                "sampled_peak_rss_bytes":peak}),
            );
        }
        if call % 1024 == 0 {
            eprintln!("reconcile_calls={call} evaluated={evaluated}");
        }
    }
    anyhow::bail!("reconciliation exceeded derived fixture work limit")
}

fn steady(handle: &RecordingCatalogHandle, settings: &Settings) -> Result<serde_json::Value> {
    let policy = settings
        .policy_for("cam-000")?
        .context("benchmark policy missing")?;
    let id = "cam-000-00001-main";
    let revision = handle
        .retention_decision(id)?
        .context("benchmark decision missing")?
        .policy_revision;
    let mut samples = Histogram::<u64>::new(3)?;
    let mut raw = Vec::with_capacity(30);
    for round in 0..35 {
        let began = Instant::now();
        handle.commit_retention(id, revision, &policy)?;
        if round >= 5 {
            let elapsed = u64::try_from(began.elapsed().as_micros())?.max(1);
            samples.record(elapsed)?;
            raw.push(elapsed);
        }
    }
    Ok(
        json!({"warmup_runs":5,"runs":30,"median_us":samples.value_at_quantile(0.5),
        "p95_us":samples.value_at_quantile(0.95),"max_us":samples.max(),"raw_us":raw}),
    )
}

fn late_events(handle: &RecordingCatalogHandle, days: u32) -> Result<serde_json::Value> {
    use keeppeek::storage::metadata::{EventSource, TimelineEvent};
    let mut samples = Histogram::<u64>::new(3)?;
    let mut raw = Vec::with_capacity(30);
    let start = START + i64::from(days - 1) * 86_400_000 + SEGMENT_MS + 1000;
    let segment = (days - 1) * 48 + 1;
    let mut revisions = late_revisions(handle, segment, 1)?;
    for round in 0..35 {
        let began = Instant::now();
        handle.insert_event(TimelineEvent {
            id: format!("late-{round}"),
            revision: 1,
            camera_id: "cam-000".into(),
            stream: None,
            source: EventSource::KeepPeek,
            kind: "person".into(),
            start_time_ms: start,
            end_time_ms: Some(start + 10_000),
            confidence: None,
            bbox: None,
            bbox_attachment_id: None,
            zone: None,
            text: None,
            payload: None,
            attachments: vec![],
            canonical_attachment_id: None,
            icon_key: "event".into(),
            rejected_icon_key: None,
            thumbnail_filename: None,
        })?;
        let mut complete = false;
        for _ in 0..16 {
            let step = handle.reconcile_retention_runtime(8)?;
            ensure!(step.quarantined == 0, "late-event fixture quarantined");
            if !step.pending {
                complete = true;
                break;
            }
        }
        ensure!(complete, "one event exceeded bounded candidate work");
        if round >= 5 {
            let elapsed = u64::try_from(began.elapsed().as_micros())?.max(1);
            samples.record(elapsed)?;
            raw.push(elapsed);
        }
        let updated = late_revisions(handle, segment, 30)?;
        ensure!(
            updated.iter().zip(revisions).all(|(new, old)| *new > old),
            "late event did not advance both stream revisions"
        );
        revisions = updated;
    }
    Ok(
        json!({"warmup_runs":5,"runs":30,"median_us":samples.value_at_quantile(0.5),
        "p95_us":samples.value_at_quantile(0.95),"max_us":samples.max(),"raw_us":raw,"includes":"canonical publication and complete affected-camera reconciliation"}),
    )
}

fn run(root: &Path, sources: u32, days: u32) -> Result<()> {
    let path = root.join("catalog.db");
    RecordingCatalog::open(&path)?.shutdown();
    let began = Instant::now();
    seed(&path, sources, days)?;
    let seed_ms = began.elapsed().as_millis();
    let files = u64::from(sources) * u64::from(days) * 48 * 2;
    let events = u64::from(sources) * u64::from(days) * 24;
    let limit = files.div_ceil(8) + events.div_ceil(128) + 10000;
    let mut resources = sysinfo::System::new();
    let began = Instant::now();
    let catalog = RecordingCatalog::open(&path)?;
    let startup_with_index_rebuild_us = began.elapsed().as_micros();
    let handle = catalog.handle();
    let settings: Settings = toml::from_str(
        "[default]\ncontinuous_days=1.0\nmotion_days=7.0\n[default.events]\nperson=30.0",
    )?;
    let baseline_rss = rss(&mut resources);
    let mut report = json!({"sources":sources,"days":days,"streams":2,"segment_ms":SEGMENT_MS,
        "historical_recordings":files,"events":events,"event_density":"one camera-wide motion/person event per hour",
        "protected":"first main/sub segment of each day","seed_ms":seed_ms,"baseline_rss_bytes":baseline_rss,
        "startup_with_index_rebuild_us":startup_with_index_rebuild_us,
        "startup_scope":"complete catalog reopen including four runtime recording index rebuilds over seeded archive; columns already present; not full old-schema migration qualification",
        "os":std::env::consts::OS,"arch":std::env::consts::ARCH,"complete":false,
        "scope":"synthetic historical metadata; per-batch histograms; one full sweep per phase; real H264 ingest against the same archive; no physical deletion or live pacing"});
    ensure!(
        handle.request_retention_settings(Some(&settings))?,
        "initial transition rejected"
    );
    let initial = reconcile(&handle, limit, &mut resources)?;
    report["initial_activation"] = initial;
    write_report(root, "initial", &report)?;
    let steady = steady(&handle, &settings)?;
    let late_events = late_events(&handle, days)?;
    verify_extension(&handle, 30)?;
    report["steady_commitment"] = steady;
    report["late_events"] = late_events;
    write_report(root, "late-events", &report)?;
    let extended: Settings = toml::from_str(
        "[default]\ncontinuous_days=1.0\nmotion_days=7.0\n[default.events]\nperson=31.0",
    )?;
    ensure!(
        handle.request_retention_settings(Some(&extended))?,
        "extension rejected"
    );
    handle.reconcile_retention_runtime(8)?;
    catalog.shutdown();
    let began = Instant::now();
    let catalog = RecordingCatalog::open(&path)?;
    let reopen_us = began.elapsed().as_micros();
    let handle = catalog.handle();
    let restarted = reconcile(&handle, limit, &mut resources)?;
    verify_extension(&handle, 31)?;
    report["restart_open_us"] = json!(reopen_us);
    report["restarted_extension"] = restarted;
    write_report(root, "restarted", &report)?;
    finish_archive(root, &path, catalog, limit, &mut resources, report)
}

fn finish_archive(
    root: &Path,
    path: &Path,
    catalog: RecordingCatalog,
    limit: u64,
    resources: &mut sysinfo::System,
    mut report: serde_json::Value,
) -> Result<()> {
    let (rollback, enabled, disabled) = archive_ingest(root, path, catalog, limit, resources)?;
    report["disable_rollback"] = rollback;
    report["archive_ingest_enabled"] = enabled;
    report["archive_ingest_disabled"] = disabled;
    report["complete"] = json!(true);
    println!("{}", serde_json::to_string_pretty(&report)?);
    write_report(root, "report", &report)?;
    eprintln!("artifact_directory={}", root.display());
    Ok(())
}

fn write_report(root: &Path, phase: &str, report: &serde_json::Value) -> Result<()> {
    // ponytail: Save completed phases directly so a later failure retains their measurements.
    std::fs::write(
        root.join(format!("{phase}.json")),
        serde_json::to_vec_pretty(report)?,
    )?;
    Ok(())
}

fn verify_extension(handle: &RecordingCatalogHandle, days: i64) -> Result<()> {
    let decision = handle
        .retention_decision("cam-000-00006-main")?
        .context("extension commitment missing")?;
    ensure!(
        decision.deadline_ms == Some(START + 7 * SEGMENT_MS + days * 86_400_000),
        "extension or rollback changed the expected person deadline"
    );
    Ok(())
}

fn late_revisions(handle: &RecordingCatalogHandle, segment: u32, days: i64) -> Result<[u64; 2]> {
    let mut revisions = [0; 2];
    for (index, stream) in ["main", "sub"].into_iter().enumerate() {
        let id = format!("cam-000-{segment:05}-{stream}");
        let decision = handle
            .retention_decision(&id)?
            .context("late-event commitment missing")?;
        ensure!(
            decision.deadline_ms
                == Some(START + i64::from(segment + 1) * SEGMENT_MS + days * 86_400_000),
            "late-event deadline did not match the expected lifetime"
        );
        revisions[index] = decision.event_revision;
    }
    Ok(revisions)
}

fn prepare_seed(connection: &turso::Connection) -> Result<()> {
    // Seed before runtime hooks and indexes so reopening measures native index construction.
    pollster::block_on(connection.execute_batch(
        "BEGIN IMMEDIATE;
        DROP INDEX IF EXISTS recording_retention_pending_files;
        DROP INDEX IF EXISTS recording_retention_pending_generation;
        DROP INDEX IF EXISTS recording_retention_camera_files;
        DROP INDEX IF EXISTS recording_retention_legacy_claims;
        DROP TRIGGER IF EXISTS recording_retention_runtime_file_insert;
        DROP TRIGGER IF EXISTS recording_retention_runtime_file_update;
        COMMIT;",
    ))?;
    Ok(())
}

fn archive_ingest(
    root: &Path,
    path: &Path,
    catalog: RecordingCatalog,
    limit: u64,
    resources: &mut sysinfo::System,
) -> Result<(serde_json::Value, serde_json::Value, serde_json::Value)> {
    catalog.shutdown();
    let enabled = ingest::measure(&root.join("ingest-enabled"), true, Some(path), 35)?;
    let catalog = RecordingCatalog::open(path)?;
    let handle = catalog.handle();
    ensure!(
        handle.request_retention_settings(None)?,
        "rollback request rejected"
    );
    let rollback = reconcile(&handle, limit, resources)?;
    verify_extension(&handle, 31)?;
    catalog.shutdown();
    let disabled = ingest::measure(&root.join("ingest-disabled"), false, Some(path), 35)?;
    let catalog = RecordingCatalog::open(path)?;
    verify_extension(&catalog.handle(), 31)?;
    catalog.shutdown();
    Ok((rollback, enabled, disabled))
}
