//! Measures named ownership overhead against the same writer without volume accounting.

use super::*;

#[test]
#[ignore = "local disk benchmark; run alone with --ignored --nocapture"]
fn named_writer_local_scale() -> anyhow::Result<()> {
    let (root, catalog, mut config) = named_fixture()?;
    config.flush_interval = Duration::from_secs(60);
    let mut baseline = config.clone();
    baseline.volume_runtime = None;
    baseline.medium_term_path = root.join("baseline");
    baseline
        .long_term_path
        .clone_from(&baseline.medium_term_path);
    std::fs::create_dir(&baseline.medium_term_path)?;
    let mut workers = [
        WriterWorker::new(
            baseline,
            RecordingDemand::new(Duration::ZERO),
            Some(catalog.handle()),
        ),
        WriterWorker::new(
            config,
            RecordingDemand::new(Duration::ZERO),
            Some(catalog.handle()),
        ),
    ];
    let mut elapsed = [Vec::with_capacity(30), Vec::with_capacity(30)];
    let mut ingest = [Vec::with_capacity(15_360), Vec::with_capacity(15_360)];
    for run in 0..31 {
        // Alternate execution order to reduce cache and background load bias.
        for index in [run % 2, (run + 1) % 2] {
            let (duration, samples) =
                record_batch(&mut workers[index], &catalog.handle(), index == 1)?;
            if run != 0 {
                elapsed[index].push(duration.as_secs_f64() * 1000.0);
                ingest[index].extend(samples);
            }
        }
    }
    for (name, timings) in ["baseline", "named"].into_iter().zip(&mut ingest) {
        timings.sort_by(f64::total_cmp);
        println!(
            "{name}_ingest_ms: samples={} median={:.3} p95={:.3}",
            timings.len(),
            timings[timings.len().div_ceil(2) - 1],
            timings[(timings.len() * 95).div_ceil(100) - 1]
        );
    }
    for (name, timings) in ["baseline", "named"].into_iter().zip(&mut elapsed) {
        timings.sort_by(f64::total_cmp);
        println!(
            "{name}: runs=30 cameras=8 frames_per_camera=64 median_ms={:.3} p95_ms={:.3}",
            timings[14], timings[28]
        );
    }
    drop(workers);
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

fn record_batch(
    worker: &mut WriterWorker,
    catalog: &RecordingCatalogHandle,
    named: bool,
) -> anyhow::Result<(Duration, Vec<f64>)> {
    let identities: Vec<_> = (0..8)
        .map(|camera| RecordingStreamIdentity::legacy(format!("camera-{camera}")))
        .collect();
    let start = Instant::now();
    let mut ingest_timings = Vec::with_capacity(512);
    for frame in 0..64 {
        for identity in &identities {
            let ingest_start = Instant::now();
            worker.ingest(
                identity.clone(),
                key_frame(start + Duration::from_millis(frame * 40)),
            );
            ingest_timings.push(ingest_start.elapsed().as_secs_f64() * 1_000.0);
        }
    }
    worker.shutdown_flush();
    let mut files = Vec::with_capacity(identities.len());
    for identity in &identities {
        let writer = worker
            .pipelines
            .get_mut(&identity.storage_key)
            .unwrap()
            .medium_term
            .take()
            .unwrap();
        let id = writer.recording_id().to_owned();
        let path = writer.finalize()?;
        let path = worker.move_to_long_term(&identity.storage_key, &path, &id)?;
        files.push((id, path));
    }
    let duration = start.elapsed();
    for (id, path) in files {
        assert_eq!(samples(&path), 64);
        if named {
            assert_published(catalog, &id, &path);
        }
    }
    Ok((duration, ingest_timings))
}
