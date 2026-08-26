use keeppeek::{
    config::StorageToml,
    storage::{
        MediaFrame, RecordingCatalog, RecordingFrame, StorageConfig, StorageEngine, VideoCodec,
        VideoFrame,
    },
};
use std::{path::PathBuf, time::Duration, time::Instant};

const DEFAULT_SAMPLES: usize = 20;
const DEFAULT_SEGMENTS: usize = 64;

fn main() {
    let samples = environment_usize("KEEPPEEK_STORAGE_BENCH_SAMPLES", DEFAULT_SAMPLES);
    let segments = environment_usize("KEEPPEEK_STORAGE_BENCH_SEGMENTS", DEFAULT_SEGMENTS);
    assert!(samples > 0, "benchmark samples must be greater than zero");
    assert!(segments > 0, "benchmark segments must be greater than zero");

    let _ = run_sample(usize::MAX, segments);
    let mut nanoseconds_per_segment = (0..samples)
        .map(|sample| run_sample(sample, segments) / segments as u128)
        .collect::<Vec<_>>();
    nanoseconds_per_segment.sort_unstable();
    let median = percentile(&nanoseconds_per_segment, 50);
    let p95 = percentile(&nanoseconds_per_segment, 95);

    println!("samples={samples}");
    println!("segments_per_sample={segments}");
    println!("median_ns_per_finalized_segment={median}");
    println!("p95_ns_per_finalized_segment={p95}");
}

fn run_sample(sample: usize, segments: usize) -> u128 {
    let root = benchmark_root(sample);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let storage_toml: StorageToml = toml::from_str(&format!(
        "medium_term_path = {root:?}\n\
         long_term_path = {root:?}\n\
         recording_catalog_path = {:?}\n\
         event_thumbnail_path = {:?}\n\
         short_term_secs = 0\n\
         medium_term_secs = 0\n\
         flush_interval_secs = 0\n\
         long_term_max_gb = 0\n\
         minimum_free_gb = 1\n\
         warning_free_gb = 1\n\
         critical_free_gb = 1\n\
         cleanup_hysteresis_gb = 1\n",
        root.join("recordings.db"),
        root.join(".event-thumbnails"),
    ))
    .unwrap();
    let config = StorageConfig::from_toml(&storage_toml);
    let catalog = RecordingCatalog::open(&config.recording_catalog_path).unwrap();
    let engine = StorageEngine::start_with_catalog(config, catalog.handle());
    let started_at = Instant::now();
    let measured_at = Instant::now();
    for index in 0..segments {
        engine.ingest(
            "benchmark/sub",
            key_frame(started_at + Duration::from_millis(index as u64 * 40)),
        );
    }
    engine.shutdown();
    let elapsed = measured_at.elapsed().as_nanos();
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
    elapsed
}

fn key_frame(received_at: Instant) -> RecordingFrame {
    RecordingFrame {
        received_at,
        timestamp: None,
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: true,
            width: 320,
            height: 240,
            data: vec![
                0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
                0x3c, 0x80, 0, 0, 0, 1, 0x65,
            ]
            .into(),
        }),
    }
}

fn benchmark_root(sample: usize) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("storage-safety-benchmark")
        .join(format!("{}-{sample}", std::process::id()))
}

fn environment_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

const fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
    sorted[rank.saturating_sub(1)]
}
