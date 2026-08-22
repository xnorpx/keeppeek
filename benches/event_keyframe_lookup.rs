use anyhow::Context;
use clap::Parser;
use hdrhistogram::Histogram;
use keeppeek::storage::{
    EventKeyframeLookup, RecordingCatalog,
    benchmark::{
        BENCHMARK_FRAGMENTS_PER_RECORDING, BenchmarkRecordingSeed, EventKeyframeCorpusConfig,
        EventKeyframeCorpusSummary, benchmark_event_id, benchmark_recording_id,
        build_event_keyframe_corpus,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt::Write as _,
    fs::File,
    hint::black_box,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FORMAT_VERSION: u32 = 4;
const LOOKUP_CONTRACT_VERSION: u32 = 1;
const CAMERA_COUNT: u32 = 127;
const DAY_COUNT: u32 = 30;
const START_TIME_MS: i64 = 1_782_864_000_000;
const DAY_MS: i64 = 86_400_000;
const EVENT_BATCH_SIZE: u64 = 2_000;
const REQUEST_POOL_SIZE: usize = 100_000;
const MAX_RECORDED_LATENCY_NS: u64 = 60_000_000_000;

#[derive(Debug, Parser)]
#[command(about = "Benchmark indexed event-to-keyframe lookups without WebRTC")]
struct Args {
    #[arg(long, hide = true)]
    bench: bool,
    #[arg(long)]
    rebuild: bool,
    #[arg(long)]
    prepare_only: bool,
    #[arg(long, default_value_t = 1_024)]
    target_mib: u64,
    #[arg(long, default_value_t = 5)]
    warmup_secs: u64,
    #[arg(long, default_value_t = 20)]
    measurement_secs: u64,
    #[arg(long, default_value_t = 10_000)]
    correctness_lookups: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FixtureManifest {
    name: String,
    source_path: String,
    source_sha256: String,
    file_len: u64,
    keyframe_offset: u64,
    keyframe_len: u64,
    keyframe_sha256: String,
}

#[derive(Debug, Clone)]
struct FixtureRuntime {
    manifest: FixtureManifest,
    keyframe_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CorpusManifest {
    format_version: u32,
    lookup_contract_version: u32,
    target_bytes: u64,
    camera_count: u32,
    day_count: u32,
    streams: Vec<String>,
    start_time_ms: i64,
    logical_database_bytes: u64,
    recording_count: u64,
    fragment_count: u64,
    keyframe_count: u64,
    event_count: u64,
    event_keyframe_link_count: u64,
    media_storage_mode: String,
    fixtures: Vec<FixtureManifest>,
}

struct PreparedCorpus {
    root: PathBuf,
    manifest: CorpusManifest,
    fixtures: Vec<FixtureRuntime>,
    reused: bool,
}

#[derive(Debug, Clone)]
struct LookupRequest {
    event_id: String,
    stream_id: &'static str,
    expected_recording_id: String,
    expected_fragment_sequence: u64,
    expected_event_time_ms: i64,
    expected_fragment_start_ms: i64,
    expected_camera_index: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Workload {
    Resolve,
    Read,
}

impl Workload {
    const fn name(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Read => "read",
        }
    }
}

#[derive(Debug, Serialize)]
struct BenchmarkResult {
    workload: Workload,
    concurrency: usize,
    operations: u64,
    failures: u64,
    returned_bytes: u64,
    elapsed_seconds: f64,
    operations_per_second: f64,
    mebibytes_per_second: f64,
    mean_us: f64,
    p50_us: f64,
    p90_us: f64,
    p95_us: f64,
    p99_us: f64,
    p999_us: f64,
    max_us: f64,
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    generated_at_unix_ms: u128,
    corpus_manifest_sha256: String,
    corpus_root: String,
    corpus_reused: bool,
    logical_database_bytes: u64,
    apparent_corpus_bytes: u64,
    event_count: u64,
    catalog_open_ms: f64,
    architecture: String,
    operating_system: String,
    available_parallelism: usize,
    warmup_seconds: u64,
    measurement_seconds: u64,
    results: Vec<BenchmarkResult>,
}

struct WorkerResult {
    histogram: Histogram<u64>,
    operations: u64,
    failures: u64,
    returned_bytes: u64,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.target_mib == 0 {
        anyhow::bail!("--target-mib must be positive");
    }
    if !args.prepare_only && (args.warmup_secs == 0 || args.measurement_secs == 0) {
        anyhow::bail!("warm-up and measurement durations must be positive");
    }

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures = inspect_fixtures(&repository_root)?;
    let corpus = prepare_corpus(&repository_root, &args, fixtures)?;
    println!(
        "corpus: {} ({:.1} MiB logical, {} events, {})",
        corpus.root.display(),
        corpus.manifest.logical_database_bytes as f64 / 1_048_576.0,
        corpus.manifest.event_count,
        if corpus.reused { "reused" } else { "generated" }
    );
    if args.prepare_only {
        return Ok(());
    }

    let catalog_path = corpus.root.join("catalog.db");
    let opened_at = Instant::now();
    let mut catalog = RecordingCatalog::open(&catalog_path)?;
    catalog.wait_for_maintenance();
    let catalog_open = opened_at.elapsed();
    let lookup = Arc::new(EventKeyframeLookup::new(catalog.handle()));
    let requests = Arc::new(build_requests(
        corpus.manifest.event_count,
        REQUEST_POOL_SIZE,
    ));
    verify_lookups(
        &lookup,
        &requests,
        &corpus.fixtures,
        args.correctness_lookups,
    )?;

    let mut results = Vec::new();
    for workload in [Workload::Resolve, Workload::Read] {
        for concurrency in [1, 8, 32] {
            println!(
                "warming {} at concurrency {concurrency} for {}s",
                workload.name(),
                args.warmup_secs
            );
            let warmup = run_round(
                workload,
                concurrency,
                Duration::from_secs(args.warmup_secs),
                lookup.clone(),
                requests.clone(),
            )?;
            if warmup.failures != 0 {
                anyhow::bail!(
                    "{} warm-up at concurrency {concurrency} had {} failures",
                    workload.name(),
                    warmup.failures
                );
            }
            println!(
                "measuring {} at concurrency {concurrency} for {}s",
                workload.name(),
                args.measurement_secs
            );
            let result = run_round(
                workload,
                concurrency,
                Duration::from_secs(args.measurement_secs),
                lookup.clone(),
                requests.clone(),
            )?;
            print_result(&result);
            if result.failures != 0 {
                anyhow::bail!(
                    "{} benchmark at concurrency {concurrency} had {} failures",
                    workload.name(),
                    result.failures
                );
            }
            results.push(result);
        }
    }

    drop(lookup);
    catalog.shutdown();
    let manifest_bytes = serde_json::to_vec(&corpus.manifest)?;
    let report = BenchmarkReport {
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        corpus_manifest_sha256: sha256_bytes(&manifest_bytes),
        corpus_root: corpus.root.to_string_lossy().into_owned(),
        corpus_reused: corpus.reused,
        logical_database_bytes: corpus.manifest.logical_database_bytes,
        apparent_corpus_bytes: directory_apparent_bytes(&corpus.root)?,
        event_count: corpus.manifest.event_count,
        catalog_open_ms: catalog_open.as_secs_f64() * 1_000.0,
        architecture: std::env::consts::ARCH.to_owned(),
        operating_system: std::env::consts::OS.to_owned(),
        available_parallelism: thread::available_parallelism()?.get(),
        warmup_seconds: args.warmup_secs,
        measurement_seconds: args.measurement_secs,
        results,
    };
    write_report(&repository_root, &report)?;
    Ok(())
}

fn inspect_fixtures(repository_root: &Path) -> anyhow::Result<Vec<FixtureRuntime>> {
    let testdata = repository_root.join("crates/test-camera/testdata");
    [
        ("main-h264", "cc-4k-3840x2160-h264.mp4"),
        ("main-h265", "cc-4k-3840x2160-h265.mp4"),
        ("sub-h264", "cc-4k-640x360-h264.mp4"),
        ("sub-h265", "cc-4k-640x360-h265.mp4"),
    ]
    .into_iter()
    .map(|(name, filename)| inspect_fixture(repository_root, name, &testdata.join(filename)))
    .collect()
}

fn inspect_fixture(
    repository_root: &Path,
    name: &str,
    path: &Path,
) -> anyhow::Result<FixtureRuntime> {
    let source_bytes = std::fs::read(path)?;
    let mut reader = mp4::read_mp4(File::open(path)?)?;
    let (&track_id, track) = reader
        .tracks()
        .iter()
        .find(|(_, track)| {
            matches!(
                track.media_type(),
                Ok(mp4::MediaType::H264 | mp4::MediaType::H265)
            )
        })
        .context("benchmark fixture has no H.264 or H.265 video track")?;
    let sample_count = track.sample_count();
    for sample_id in 1..=sample_count {
        let location = reader
            .sample_location(track_id, sample_id)?
            .context("benchmark fixture sample has no location")?;
        let sample = reader
            .read_sample(track_id, sample_id)?
            .context("benchmark fixture sample is missing")?;
        if !sample.is_sync {
            continue;
        }
        let source_path = path
            .strip_prefix(repository_root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        return Ok(FixtureRuntime {
            manifest: FixtureManifest {
                name: name.to_owned(),
                source_path,
                source_sha256: sha256_bytes(&source_bytes),
                file_len: source_bytes.len() as u64,
                keyframe_offset: location.offset,
                keyframe_len: u64::from(location.size),
                keyframe_sha256: sha256_bytes(&sample.bytes),
            },
            keyframe_bytes: sample.bytes.to_vec(),
        });
    }
    anyhow::bail!("benchmark fixture {} has no sync sample", path.display())
}

fn prepare_corpus(
    repository_root: &Path,
    args: &Args,
    fixtures: Vec<FixtureRuntime>,
) -> anyhow::Result<PreparedCorpus> {
    let target_bytes = args
        .target_mib
        .checked_mul(1_048_576)
        .context("benchmark target size overflows")?;
    let base = repository_root.join("target/perf/event-keyframe-lookup");
    let corpus_name = if args.target_mib == 1_024 {
        "corpus".to_owned()
    } else {
        format!("corpus-{}mib", args.target_mib)
    };
    let root = base.join(corpus_name);
    let fixture_manifests = fixtures
        .iter()
        .map(|fixture| fixture.manifest.clone())
        .collect::<Vec<_>>();
    if !args.rebuild
        && let Some(manifest) = reusable_manifest(&root, target_bytes, &fixture_manifests)?
    {
        return Ok(PreparedCorpus {
            root,
            manifest,
            fixtures,
            reused: true,
        });
    }
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&base)?;
    let building = base.join(format!(".building-{}", uuid::Uuid::new_v4().hyphenated()));
    std::fs::create_dir_all(&building)?;
    let result = build_corpus(repository_root, &root, &building, target_bytes, &fixtures);
    let manifest = match result {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&building);
            return Err(error);
        }
    };
    std::fs::write(
        building.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::rename(&building, &root)?;
    Ok(PreparedCorpus {
        root,
        manifest,
        fixtures,
        reused: false,
    })
}

fn reusable_manifest(
    root: &Path,
    target_bytes: u64,
    fixtures: &[FixtureManifest],
) -> anyhow::Result<Option<CorpusManifest>> {
    let manifest_path = root.join("manifest.json");
    let catalog_path = root.join("catalog.db");
    if !manifest_path.is_file() || !catalog_path.is_file() {
        return Ok(None);
    }
    let manifest: CorpusManifest = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
    let matches = manifest.format_version == FORMAT_VERSION
        && manifest.lookup_contract_version == LOOKUP_CONTRACT_VERSION
        && manifest.target_bytes == target_bytes
        && manifest.camera_count == CAMERA_COUNT
        && manifest.day_count == DAY_COUNT
        && manifest.streams == ["main", "sub"]
        && manifest.start_time_ms == START_TIME_MS
        && manifest.logical_database_bytes >= target_bytes
        && manifest.fixtures == fixtures
        && root.join("media/camera-000/day-00/main.mp4").is_file()
        && root.join("media/camera-126/day-29/sub.mp4").is_file();
    Ok(matches.then_some(manifest))
}

fn build_corpus(
    repository_root: &Path,
    final_root: &Path,
    building_root: &Path,
    target_bytes: u64,
    fixtures: &[FixtureRuntime],
) -> anyhow::Result<CorpusManifest> {
    let mut recordings = Vec::with_capacity(CAMERA_COUNT as usize * DAY_COUNT as usize * 2);
    let mut copied_files = 0u64;
    for camera_index in 0..CAMERA_COUNT {
        for day_index in 0..DAY_COUNT {
            for stream_id in ["main", "sub"] {
                let fixture = fixture_for(fixtures, camera_index, stream_id);
                let relative = PathBuf::from(format!(
                    "media/camera-{camera_index:03}/day-{day_index:02}/{stream_id}.mp4"
                ));
                let building_path = building_root.join(&relative);
                let final_path = final_root.join(&relative);
                std::fs::create_dir_all(building_path.parent().unwrap())?;
                let source_path = repository_root.join(&fixture.manifest.source_path);
                if std::fs::hard_link(&source_path, &building_path).is_err() {
                    std::fs::copy(&source_path, &building_path)?;
                    copied_files = copied_files.saturating_add(1);
                }
                recordings.push(BenchmarkRecordingSeed {
                    recording_id: benchmark_recording_id(camera_index, day_index, stream_id),
                    stream_key: format!("camera-{camera_index:03}/{stream_id}"),
                    source_id: format!("camera-{camera_index:03}"),
                    stream_id: stream_id.to_owned(),
                    path: final_path,
                    started_at_ms: START_TIME_MS
                        .saturating_add(i64::from(day_index).saturating_mul(86_400_000)),
                    file_len: fixture.manifest.file_len,
                    keyframe_offset: fixture.manifest.keyframe_offset,
                    keyframe_len: fixture.manifest.keyframe_len,
                });
            }
        }
    }
    let summary = build_event_keyframe_corpus(&EventKeyframeCorpusConfig {
        catalog_path: building_root.join("catalog.db"),
        recordings,
        target_bytes,
        camera_count: CAMERA_COUNT,
        day_count: DAY_COUNT,
        start_time_ms: START_TIME_MS,
        batch_events: EVENT_BATCH_SIZE,
    })?;
    Ok(manifest_from_summary(
        target_bytes,
        copied_files,
        fixtures,
        summary,
    ))
}

fn manifest_from_summary(
    target_bytes: u64,
    copied_files: u64,
    fixtures: &[FixtureRuntime],
    summary: EventKeyframeCorpusSummary,
) -> CorpusManifest {
    CorpusManifest {
        format_version: FORMAT_VERSION,
        lookup_contract_version: LOOKUP_CONTRACT_VERSION,
        target_bytes,
        camera_count: CAMERA_COUNT,
        day_count: DAY_COUNT,
        streams: vec!["main".to_owned(), "sub".to_owned()],
        start_time_ms: START_TIME_MS,
        logical_database_bytes: summary.logical_database_bytes,
        recording_count: summary.recording_count,
        fragment_count: summary.fragment_count,
        keyframe_count: summary.keyframe_count,
        event_count: summary.event_count,
        event_keyframe_link_count: summary.event_keyframe_link_count,
        media_storage_mode: if copied_files == 0 {
            "hard_link".to_owned()
        } else {
            format!("mixed_with_{copied_files}_copies")
        },
        fixtures: fixtures
            .iter()
            .map(|fixture| fixture.manifest.clone())
            .collect(),
    }
}

fn fixture_for<'a>(
    fixtures: &'a [FixtureRuntime],
    camera_index: u32,
    stream_id: &str,
) -> &'a FixtureRuntime {
    let index = match (stream_id, camera_index % 2) {
        ("main", 0) => 0,
        ("main", _) => 1,
        ("sub", 0) => 2,
        ("sub", _) => 3,
        _ => unreachable!("benchmark stream must be main or sub"),
    };
    &fixtures[index]
}

fn build_requests(event_count: u64, count: usize) -> Vec<LookupRequest> {
    let mut requests = Vec::with_capacity(count);
    let mut state = 0x4b65_6570_5065_656bu64;
    for position in 0..count {
        state = splitmix64(state.wrapping_add(position as u64));
        let event_index = state % event_count;
        let stream_id = if position % 2 == 0 { "main" } else { "sub" };
        let bucket = event_index % (u64::from(CAMERA_COUNT) * u64::from(DAY_COUNT));
        let camera_index = u32::try_from(bucket % u64::from(CAMERA_COUNT)).unwrap();
        let day_index = u32::try_from(bucket / u64::from(CAMERA_COUNT)).unwrap();
        let offset_ms =
            i64::try_from(splitmix64(event_index) % u64::try_from(DAY_MS - 60_000).unwrap())
                .unwrap();
        let fragment_duration_ms = DAY_MS / i64::from(BENCHMARK_FRAGMENTS_PER_RECORDING);
        let fragment_sequence = u64::try_from(offset_ms / fragment_duration_ms + 1).unwrap();
        let day_start_ms = START_TIME_MS + i64::from(day_index) * DAY_MS;
        requests.push(LookupRequest {
            event_id: benchmark_event_id(event_index),
            stream_id,
            expected_recording_id: benchmark_recording_id(camera_index, day_index, stream_id),
            expected_fragment_sequence: fragment_sequence,
            expected_event_time_ms: day_start_ms + offset_ms,
            expected_fragment_start_ms: day_start_ms
                + i64::try_from(fragment_sequence - 1).unwrap() * fragment_duration_ms,
            expected_camera_index: camera_index,
        });
    }
    requests
}

fn verify_lookups(
    lookup: &EventKeyframeLookup,
    requests: &[LookupRequest],
    fixtures: &[FixtureRuntime],
    count: usize,
) -> anyhow::Result<()> {
    let check_count = count.min(requests.len());
    println!("verifying {check_count} event-keyframe lookups");
    for request in requests.iter().take(check_count) {
        let keyframe = lookup
            .read(&request.event_id, request.stream_id)?
            .with_context(|| {
                format!(
                    "missing keyframe for {} {}",
                    request.event_id, request.stream_id
                )
            })?;
        let fixture = fixture_for(fixtures, request.expected_camera_index, request.stream_id);
        if keyframe.location.event_id != request.event_id
            || keyframe.location.stream_id != request.stream_id
            || keyframe.location.recording_id != request.expected_recording_id
            || keyframe.location.fragment_sequence != request.expected_fragment_sequence
            || keyframe.location.event_time_ms != request.expected_event_time_ms
            || keyframe.location.fragment_start_ms != request.expected_fragment_start_ms
            || keyframe.location.byte_offset != fixture.manifest.keyframe_offset
            || keyframe.location.byte_len != fixture.manifest.keyframe_len
        {
            anyhow::bail!(
                "lookup metadata did not match the seeded target for {} {}: {:?}",
                request.event_id,
                request.stream_id,
                keyframe.location,
            );
        }
        if keyframe.bytes != fixture.keyframe_bytes {
            anyhow::bail!(
                "keyframe bytes did not match {} for {} {}",
                fixture.manifest.name,
                request.event_id,
                request.stream_id
            );
        }
    }
    Ok(())
}

fn run_round(
    workload: Workload,
    concurrency: usize,
    duration: Duration,
    lookup: Arc<EventKeyframeLookup>,
    requests: Arc<Vec<LookupRequest>>,
) -> anyhow::Result<BenchmarkResult> {
    let barrier = Arc::new(Barrier::new(concurrency + 1));
    let mut workers = Vec::with_capacity(concurrency);
    for worker_index in 0..concurrency {
        let barrier = barrier.clone();
        let lookup = lookup.clone();
        let requests = requests.clone();
        workers.push(thread::spawn(move || -> anyhow::Result<WorkerResult> {
            let mut histogram = Histogram::<u64>::new_with_bounds(1, MAX_RECORDED_LATENCY_NS, 3)?;
            let mut operations = 0u64;
            let mut failures = 0u64;
            let mut returned_bytes = 0u64;
            let mut cursor = worker_index.saturating_mul(7_919) % requests.len();
            let mut checksum = 0u64;
            barrier.wait();
            let deadline = Instant::now() + duration;
            while Instant::now() < deadline {
                let request = &requests[cursor];
                cursor += 1;
                if cursor == requests.len() {
                    cursor = 0;
                }
                let started = Instant::now();
                match workload {
                    Workload::Resolve => match lookup.resolve(&request.event_id, request.stream_id)
                    {
                        Ok(Some(location)) => {
                            checksum = checksum.wrapping_add(location.byte_offset);
                        }
                        Ok(None) | Err(_) => failures = failures.saturating_add(1),
                    },
                    Workload::Read => match lookup.read(&request.event_id, request.stream_id) {
                        Ok(Some(keyframe)) => {
                            returned_bytes =
                                returned_bytes.saturating_add(keyframe.bytes.len() as u64);
                            checksum = checksum
                                .wrapping_add(keyframe.bytes.first().copied().unwrap_or(0) as u64);
                            black_box(&keyframe.bytes);
                        }
                        Ok(None) | Err(_) => failures = failures.saturating_add(1),
                    },
                }
                histogram.saturating_record(
                    u64::try_from(started.elapsed().as_nanos())
                        .unwrap_or(u64::MAX)
                        .max(1),
                );
                operations = operations.saturating_add(1);
            }
            black_box(checksum);
            Ok(WorkerResult {
                histogram,
                operations,
                failures,
                returned_bytes,
            })
        }));
    }
    barrier.wait();
    let wall_started = Instant::now();
    let mut merged = Histogram::<u64>::new_with_bounds(1, MAX_RECORDED_LATENCY_NS, 3)?;
    let mut operations = 0u64;
    let mut failures = 0u64;
    let mut returned_bytes = 0u64;
    for worker in workers {
        let worker = worker
            .join()
            .map_err(|_| anyhow::anyhow!("benchmark worker panicked"))??;
        merged.add(&worker.histogram)?;
        operations = operations.saturating_add(worker.operations);
        failures = failures.saturating_add(worker.failures);
        returned_bytes = returned_bytes.saturating_add(worker.returned_bytes);
    }
    let elapsed = wall_started.elapsed();
    let elapsed_seconds = elapsed.as_secs_f64();
    Ok(BenchmarkResult {
        workload,
        concurrency,
        operations,
        failures,
        returned_bytes,
        elapsed_seconds,
        operations_per_second: operations as f64 / elapsed_seconds,
        mebibytes_per_second: returned_bytes as f64 / 1_048_576.0 / elapsed_seconds,
        mean_us: merged.mean() / 1_000.0,
        p50_us: nanos_to_micros(merged.value_at_quantile(0.50)),
        p90_us: nanos_to_micros(merged.value_at_quantile(0.90)),
        p95_us: nanos_to_micros(merged.value_at_quantile(0.95)),
        p99_us: nanos_to_micros(merged.value_at_quantile(0.99)),
        p999_us: nanos_to_micros(merged.value_at_quantile(0.999)),
        max_us: nanos_to_micros(merged.max()),
    })
}

fn print_result(result: &BenchmarkResult) {
    println!(
        "{:<7} c={:<2} {:>10.0} ops/s  p50 {:>8.1} us  p95 {:>8.1} us  p99 {:>8.1} us  max {:>9.1} us",
        result.workload.name(),
        result.concurrency,
        result.operations_per_second,
        result.p50_us,
        result.p95_us,
        result.p99_us,
        result.max_us,
    );
}

fn write_report(repository_root: &Path, report: &BenchmarkReport) -> anyhow::Result<()> {
    let results = repository_root.join("target/perf/event-keyframe-lookup/results");
    std::fs::create_dir_all(&results)?;
    let timestamp = report.generated_at_unix_ms;
    let json = serde_json::to_vec_pretty(report)?;
    let markdown = markdown_report(report)?;
    for stem in [format!("run-{timestamp}"), "latest".to_owned()] {
        std::fs::write(results.join(format!("{stem}.json")), &json)?;
        std::fs::write(results.join(format!("{stem}.md")), &markdown)?;
    }
    println!("report: {}", results.join("latest.md").display());
    Ok(())
}

fn markdown_report(report: &BenchmarkReport) -> anyhow::Result<String> {
    let mut output = String::new();
    writeln!(output, "# Event Keyframe Lookup Benchmark")?;
    writeln!(output)?;
    writeln!(
        output,
        "- Corpus: `{}` ({:.1} MiB logical, {} events)",
        report.corpus_root,
        report.logical_database_bytes as f64 / 1_048_576.0,
        report.event_count
    )?;
    writeln!(
        output,
        "- Host: `{}` / `{}` / {} logical CPUs",
        report.operating_system, report.architecture, report.available_parallelism
    )?;
    writeln!(output, "- Catalog open: {:.2} ms", report.catalog_open_ms)?;
    writeln!(
        output,
        "- Warm-up/measurement: {}s / {}s",
        report.warmup_seconds, report.measurement_seconds
    )?;
    writeln!(output)?;
    writeln!(
        output,
        "| Workload | Concurrency | Ops/s | MiB/s | Mean us | p50 us | p95 us | p99 us | p99.9 us | Max us | Failures |"
    )?;
    writeln!(
        output,
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    )?;
    for result in &report.results {
        writeln!(
            output,
            "| {} | {} | {:.0} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {} |",
            result.workload.name(),
            result.concurrency,
            result.operations_per_second,
            result.mebibytes_per_second,
            result.mean_us,
            result.p50_us,
            result.p95_us,
            result.p99_us,
            result.p999_us,
            result.max_us,
            result.failures,
        )?;
    }
    writeln!(output)?;
    writeln!(
        output,
        "The media paths hard-link four small fixtures, so this measures large-catalog lookup, catalog queueing, file open/seek/read, allocation, and copying under an ambient OS cache. It does not model cold reads across unique 30-day video storage."
    )?;
    Ok(output)
}

fn nanos_to_micros(nanoseconds: u64) -> f64 {
    nanoseconds as f64 / 1_000.0
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn sha256_bytes(bytes: impl AsRef<[u8]>) -> String {
    let digest = Sha256::digest(bytes.as_ref());
    digest
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}

fn directory_apparent_bytes(path: &Path) -> anyhow::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_apparent_bytes(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}
