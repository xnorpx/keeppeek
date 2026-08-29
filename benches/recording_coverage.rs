use keeppeek::storage::benchmark::{
    RecordingCoverageCorpusConfig, build_recording_coverage_corpus, measure_recording_coverage,
};
use std::path::PathBuf;

const DEFAULT_CAMERAS: u32 = 127;
const DEFAULT_DAYS: u32 = 30;
const DEFAULT_SAMPLES: usize = 5;
const DEFAULT_P95_BUDGET_MS: f64 = 500.0;
const SNAPSHOT_MEMORY_BUDGET_BYTES: usize = 8 * 1_048_576;

fn main() {
    let camera_count = environment_value("KEEPPEEK_COVERAGE_BENCH_CAMERAS", DEFAULT_CAMERAS);
    let day_count = environment_value("KEEPPEEK_COVERAGE_BENCH_DAYS", DEFAULT_DAYS);
    let samples = environment_value("KEEPPEEK_COVERAGE_BENCH_SAMPLES", DEFAULT_SAMPLES);
    let p95_budget_ms = environment_value(
        "KEEPPEEK_COVERAGE_BENCH_P95_BUDGET_MS",
        DEFAULT_P95_BUDGET_MS,
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("recording-coverage-benchmark")
        .join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let config = RecordingCoverageCorpusConfig {
        catalog_path: root.join("recordings.db"),
        camera_count,
        day_count,
        start_time_ms: 1_700_000_000_000,
    };

    let seeded_fragments = build_recording_coverage_corpus(&config).unwrap();
    let report = measure_recording_coverage(&config, samples).unwrap();
    println!("camera_count={}", report.camera_count);
    println!("stream_count={}", report.stream_count);
    println!("day_count={}", report.day_count);
    println!("seeded_fragments={seeded_fragments}");
    println!("queried_fragments={}", report.fragment_count);
    println!("samples={}", report.samples);
    println!("baseline_median_ms={:.3}", report.baseline_median_ms);
    println!("baseline_p95_ms={:.3}", report.baseline_p95_ms);
    println!("snapshot_median_ms={:.3}", report.snapshot_median_ms);
    println!("snapshot_p95_ms={:.3}", report.snapshot_p95_ms);
    println!("median_delta_percent={:.2}", report.median_delta_percent);
    println!("retained_ranges={}", report.retained_ranges);
    println!("retained_range_limit={}", report.retained_range_limit);
    println!("snapshot_owned_bytes={}", report.snapshot_owned_bytes);
    println!("snapshot_memory_budget_bytes={SNAPSHOT_MEMORY_BUDGET_BYTES}");
    println!("p95_budget_ms={p95_budget_ms:.3}");
    assert!(
        report.snapshot_p95_ms <= p95_budget_ms,
        "coverage snapshot p95 {:.3} ms exceeds {:.3} ms budget",
        report.snapshot_p95_ms,
        p95_budget_ms
    );
    assert!(
        report.retained_ranges <= report.retained_range_limit,
        "coverage snapshot retained {} ranges above {} range limit",
        report.retained_ranges,
        report.retained_range_limit
    );
    assert!(
        report.snapshot_owned_bytes <= SNAPSHOT_MEMORY_BUDGET_BYTES,
        "coverage snapshot owns {} bytes above {} byte budget",
        report.snapshot_owned_bytes,
        SNAPSHOT_MEMORY_BUDGET_BYTES
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn environment_value<T>(name: &str, default: T) -> T
where
    T: std::str::FromStr,
{
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
