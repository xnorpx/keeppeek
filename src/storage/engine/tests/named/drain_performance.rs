//! Reproducible copy and reader measurements for a two-volume drain.

use super::*;
use crate::storage::volumes::PlacementRequest;
use std::io::{Read, Write};

const BYTES: u64 = 8 * MEBIBYTE_BYTES;

#[test]
#[ignore = "local disk benchmark; run alone with --ignored --nocapture"]
fn volume_drain_local_scale() -> anyhow::Result<()> {
    let (root, catalog, initial) = runtime::tests::fixture(64 * MEBIBYTE_BYTES)?;
    let parent = std::env::var_os("KEEPPEEK_VOLUME_BENCH_DESTINATION")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.clone());
    let secondary = parent.join(format!("keeppeek-drain-{}", uuid::Uuid::new_v4()));
    runtime::tests::create_root(&secondary)?;
    println!(
        "cross_filesystem={}",
        crate::storage::volumes::root::Root::open(&root.join("primary"))?
            .identity()
            .filesystem
            != crate::storage::volumes::root::Root::open(&secondary)?
                .identity()
                .filesystem
    );
    let mut configuration = initial.configuration().clone();
    configuration.volumes.push(runtime::tests::volume(
        "secondary",
        secondary.clone(),
        64 * MEBIBYTE_BYTES,
    ));
    let manager = runtime::Manager::new(configuration, catalog.handle())?;
    let object = create_source(&manager)?;
    let unrelated = root.join("primary/unrelated.txt");
    std::fs::write(&unrelated, b"preserve")?;
    let mut samples = Vec::with_capacity(30);
    let mut meter = ProcessMeter::new()?;
    for run in 0..31 {
        let sample = run_sample(&manager, &catalog.handle(), &object, &mut meter)?;
        if run != 0 {
            samples.push(sample);
        }
    }
    report_samples(&samples);
    println!("sampled_process_resident_bytes={}", meter.resident);
    assert_eq!(std::fs::read(unrelated)?, b"preserve");
    let source = location(&catalog.handle(), &object)?;
    assert_eq!(source.bytes, BYTES);
    println!("runs=30 bytes_per_move={BYTES} copy_buffer_bytes=65536");
    drop(manager);
    drop(initial);
    catalog.shutdown();
    std::fs::remove_dir_all(&secondary)?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

struct Sample {
    timings: [f64; 6],
    queries: Vec<f64>,
    readers: Vec<f64>,
}

fn run_sample(
    manager: &runtime::Manager,
    handle: &RecordingCatalogHandle,
    object: &Object,
    meter: &mut ProcessMeter,
) -> anyhow::Result<Sample> {
    let source = location(handle, object)?;
    let destination = if source.volume == "primary" {
        "secondary"
    } else {
        "primary"
    };
    let target = &manager
        .configuration()
        .volumes
        .iter()
        .find(|volume| volume.id.as_str() == destination)
        .expect("configured destination")
        .root;
    let (idle_query, idle_reader) = read_sample(manager, handle, object)?;
    let before = meter.sample();
    let native = native_copy(&manager.owned_path(&source)?, target)?;
    let after_copy = meter.sample();
    let (elapsed, queries, readers) = drain_one(manager, handle, object, destination)?;
    let cpu = meter.sample();
    Ok(Sample {
        timings: [
            native,
            elapsed,
            idle_query,
            idle_reader,
            (after_copy - before) as f64,
            (cpu - after_copy) as f64,
        ],
        queries,
        readers,
    })
}

fn report_samples(samples: &[Sample]) {
    for (index, name) in [
        "native_copy_verify_ms",
        "named_drain_ms",
        "catalog_query_idle_ms",
        "owned_reader_idle_ms",
        "native_copy_cpu_ms",
        "named_drain_and_readers_cpu_ms",
    ]
    .into_iter()
    .enumerate()
    {
        let mut values: Vec<_> = samples.iter().map(|sample| sample.timings[index]).collect();
        report(name, &mut values);
    }
    let mut queries: Vec<_> = samples
        .iter()
        .flat_map(|sample| sample.queries.iter().copied())
        .collect();
    let mut readers: Vec<_> = samples
        .iter()
        .flat_map(|sample| sample.readers.iter().copied())
        .collect();
    report("catalog_query_during_drain_ms", &mut queries);
    report("owned_reader_during_drain_ms", &mut readers);
}

fn create_source(manager: &runtime::Manager) -> anyhow::Result<Object> {
    let object = Object {
        kind: Kind::Export,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let mut writer = manager
        .reserve(VolumeRole::Export, "camera", &[], object.clone(), BYTES)?
        .ok_or_else(|| anyhow::anyhow!("benchmark placement unavailable"))?
        .open()?;
    let buffer = [7_u8; 65_536];
    for _ in 0..BYTES / buffer.len() as u64 {
        writer.write_all(&buffer)?;
    }
    let evidence = writer.evidence()?;
    writer.publish(evidence)?;
    Ok(object)
}

fn location(
    handle: &RecordingCatalogHandle,
    object: &Object,
) -> anyhow::Result<crate::storage::catalog::locations::Location> {
    let Reply::Location(Some(location)) =
        handle.volume_location(Request::Lookup(object.clone()))?
    else {
        anyhow::bail!("benchmark object missing");
    };
    Ok(location)
}

fn native_copy(source: &Path, target: &Path) -> anyhow::Result<f64> {
    use sha2::{Digest, Sha256};
    let path = target.join(format!("{}.baseline", uuid::Uuid::new_v4()));
    let start = Instant::now();
    let mut input = File::open(source)?;
    let mut output = File::options().write(true).create_new(true).open(&path)?;
    let mut buffer = [0_u8; 65_536];
    let mut expected = Sha256::new();
    let mut remaining = BYTES;
    while remaining != 0 {
        input.read_exact(&mut buffer)?;
        expected.update(buffer);
        output.write_all(&buffer)?;
        remaining -= buffer.len() as u64;
    }
    output.sync_all()?;
    drop(output);
    let mut reader = File::open(&path)?;
    let mut actual = Sha256::new();
    for _ in 0..BYTES / buffer.len() as u64 {
        reader.read_exact(&mut buffer)?;
        actual.update(buffer);
    }
    assert_eq!(actual.finalize(), expected.finalize());
    let elapsed = start.elapsed().as_secs_f64() * 1_000.0;
    drop(reader);
    drop(input);
    std::fs::remove_file(path)?;
    Ok(elapsed)
}

fn drain_one(
    manager: &runtime::Manager,
    handle: &RecordingCatalogHandle,
    object: &Object,
    destination: &str,
) -> anyhow::Result<(f64, Vec<f64>, Vec<f64>)> {
    let request = PlacementRequest {
        role: VolumeRole::Export,
        source: "camera",
        group: "",
        required_bytes: BYTES,
    };
    let preview = manager.preview_move(object.clone(), destination, &request, &[])?;
    let id = uuid::Uuid::new_v4().to_string();
    let start = Instant::now();
    manager.admit_move(&id, &preview)?;
    let mut queries = Vec::with_capacity(100);
    let mut readers = Vec::with_capacity(100);
    std::thread::scope(|scope| -> anyhow::Result<()> {
        let worker = scope.spawn(|| manager.resume_move(&id, || false));
        for _ in 0..100 {
            let (query, reader) = read_sample(manager, handle, object)?;
            queries.push(query);
            readers.push(reader);
            if worker.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("drain benchmark worker panicked"))??;
        Ok(())
    })?;
    assert!(manager.retire_move(&id)?);
    assert_eq!(location(handle, object)?.volume, destination);
    Ok((start.elapsed().as_secs_f64() * 1_000.0, queries, readers))
}

fn report(name: &str, values: &mut [f64]) {
    assert!(!values.is_empty());
    values.sort_by(f64::total_cmp);
    let p50 = values.len().div_ceil(2) - 1;
    let p95 = (values.len() * 95).div_ceil(100) - 1;
    println!(
        "{name}: samples={} median={:.3} p95={:.3}",
        values.len(),
        values[p50],
        values[p95]
    );
}

fn read_sample(
    manager: &runtime::Manager,
    handle: &RecordingCatalogHandle,
    object: &Object,
) -> anyhow::Result<(f64, f64)> {
    let sample = Instant::now();
    let current = location(handle, object)?;
    let query = sample.elapsed().as_secs_f64() * 1_000.0;
    let sample = Instant::now();
    let mut reader = manager.open_owned(&current)?;
    let mut bytes = [0_u8; 65_536];
    reader.read_exact(&mut bytes)?;
    assert!(bytes.iter().all(|byte| *byte == 7));
    Ok((query, sample.elapsed().as_secs_f64() * 1_000.0))
}

struct ProcessMeter {
    system: sysinfo::System,
    pid: sysinfo::Pid,
    resident: u64,
}

impl ProcessMeter {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            system: sysinfo::System::new(),
            pid: sysinfo::get_current_pid()?,
            resident: 0,
        })
    }

    fn sample(&mut self) -> u64 {
        self.system
            .refresh_processes(sysinfo::ProcessesToUpdate::Some(&[self.pid]), true);
        let process = self
            .system
            .process(self.pid)
            .expect("benchmark process exists");
        self.resident = self.resident.max(process.memory());
        process.accumulated_cpu_time()
    }
}
