use keeppeek::storage::{
    CatalogFragment, CatalogRecording, RecordingCatalog,
    timelapse::{DEFAULT_TOLERANCE_MS, TimelapseCadence, generate_target_times},
};
use std::{fs, path::PathBuf, time::Instant};

const DEFAULT_CAMERAS: u32 = 127;
const DEFAULT_DAYS: u32 = 30;
const TARGET_CAMERA_INDEX: u32 = 63;
const TARGET_FRAGMENTS_PER_DAY: u64 = 288;
const SIBLING_FRAGMENTS_PER_DAY: u64 = 24;
const DEFAULT_TARGETS: u32 = 120;
const DEFAULT_TRIALS: usize = 15;

fn main() {
    let root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("timelapse-benchmark")
        .join(std::process::id().to_string());
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let db_path = root.join("recordings.db");
    let catalog = RecordingCatalog::open(&db_path).unwrap();
    let start_ms = 1_770_000_000_000_i64;
    let day_ms = 86_400_000_i64;
    let total_ms = i64::from(DEFAULT_DAYS) * day_ms;

    // Seed 127 cameras x 30 days (target camera at 288 5-minute fragments/day = 8,640 fragments;
    // 126 sibling cameras at 24 hourly fragments/day = 90,720 fragments; 99,360 total).
    seed_corpus(&catalog, &root, start_ms, DEFAULT_CAMERAS, DEFAULT_DAYS);
    let stream_id = format!("cam-{TARGET_CAMERA_INDEX:03}/main");
    let targets = generate_target_times(
        start_ms,
        start_ms + total_ms,
        TimelapseCadence::TargetCount(DEFAULT_TARGETS),
    )
    .unwrap();

    let mut full_scan_ms = Vec::with_capacity(DEFAULT_TRIALS);
    let mut sparse_lookup_ms = Vec::with_capacity(DEFAULT_TRIALS);
    for _ in 0..DEFAULT_TRIALS {
        let t0 = Instant::now();
        let all = catalog
            .handle()
            .media_fragments_in_range(&stream_id, start_ms, start_ms + total_ms)
            .unwrap();
        full_scan_ms.push(t0.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(
            all.len(),
            (u64::from(DEFAULT_DAYS) * TARGET_FRAGMENTS_PER_DAY) as usize
        );

        let t1 = Instant::now();
        let lookup = catalog
            .handle()
            .timelapse_lookup(
                &stream_id,
                start_ms,
                start_ms + total_ms,
                &targets,
                DEFAULT_TOLERANCE_MS,
            )
            .unwrap();
        sparse_lookup_ms.push(t1.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(lookup.candidates.len(), DEFAULT_TARGETS as usize);
        assert!(lookup.candidates.iter().all(Option::is_some));
    }

    full_scan_ms.sort_by(f64::total_cmp);
    sparse_lookup_ms.sort_by(f64::total_cmp);
    println!(
        "cameras={DEFAULT_CAMERAS} days={DEFAULT_DAYS} targets={DEFAULT_TARGETS} trials={DEFAULT_TRIALS}"
    );
    println!(
        "baseline_full_scan_ms median={:.3} p95={:.3} max={:.3}",
        percentile(&full_scan_ms, 50),
        percentile(&full_scan_ms, 95),
        full_scan_ms[full_scan_ms.len() - 1]
    );
    println!(
        "sparse_timelapse_lookup_ms median={:.3} p95={:.3} max={:.3}",
        percentile(&sparse_lookup_ms, 50),
        percentile(&sparse_lookup_ms, 95),
        sparse_lookup_ms[sparse_lookup_ms.len() - 1]
    );

    catalog.shutdown();
    let _ = fs::remove_dir_all(&root);
}

fn seed_corpus(
    catalog: &RecordingCatalog,
    root: &std::path::Path,
    start_ms: i64,
    cameras: u32,
    days: u32,
) {
    let day_ms = 86_400_000_i64;
    for cam in 0..cameras {
        let stream_id = format!("cam-{cam:03}/main");
        let frags_per_day = if cam == TARGET_CAMERA_INDEX {
            TARGET_FRAGMENTS_PER_DAY
        } else {
            SIBLING_FRAGMENTS_PER_DAY
        };
        let frag_duration_ms = day_ms / (frags_per_day as i64);
        for day in 0..days {
            let rec_start = start_ms + i64::from(day) * day_ms;
            let rec_id = format!("rec-{cam:03}-{day:02}");
            let path = root.join(format!("{rec_id}.mp4"));
            catalog
                .handle()
                .upsert_recording(CatalogRecording {
                    id: rec_id.clone(),
                    stream_id: stream_id.clone(),
                    source_id: Some(format!("cam-{cam:03}")),
                    logical_stream_id: Some("main".to_owned()),
                    started_at_ms: rec_start,
                    ended_at_ms: Some(rec_start + day_ms),
                    path: path.to_string_lossy().into_owned(),
                    init_offset: 0,
                    init_len: 128,
                    finalized: true,
                })
                .unwrap();
            for seq in 0..frags_per_day {
                catalog
                    .handle()
                    .insert_fragment(CatalogFragment {
                        recording_id: rec_id.clone(),
                        sequence: seq + 1,
                        start_ms: rec_start + (seq as i64) * frag_duration_ms,
                        duration_ms: frag_duration_ms as u64,
                        byte_offset: 128 + seq * 4_096,
                        byte_len: 4_096,
                        random_access: true,
                    })
                    .unwrap();
            }
        }
    }
}

fn percentile(sorted: &[f64], p: usize) -> f64 {
    let idx = ((sorted.len() * p).div_ceil(100)).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}
