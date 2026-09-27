//! Identical bearer-only harness for the baseline and feature worktrees.
//! Run the ignored test in release mode, alone, on the same otherwise idle machine.

use super::{AccessKey, AccessManager, ApiPrincipal, Request, ServerState, api_principal};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, hint::black_box, io::Read, sync::Arc, time::Instant};

const SAMPLES: usize = 10_000;
const WARMUP: usize = 1_000;

fn fixture() -> (ServerState, Request) {
    let mut state = ServerState::empty();
    let key = AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
    state.access_manager = AccessManager::ephemeral(key);
    state.allowed_origins = Arc::new(HashSet::from(["https://keeppeek.example".to_owned()]));
    let request = Request::fake_https_from(
        "203.0.113.1:4567".parse().unwrap(),
        "GET",
        "/api/session",
        vec![
            ("Host".into(), "keeppeek.example".into()),
            ("Origin".into(), "https://keeppeek.example".into()),
            ("Sec-Fetch-Site".into(), "same-origin".into()),
            (
                "Authorization".into(),
                format!("Bearer {}", key.canonical()),
            ),
        ],
        vec![],
    );
    (state, request)
}

fn measured(state: &ServerState, request: &Request) -> (u64, ApiPrincipal) {
    let started = Instant::now();
    let result = api_principal(black_box(request), black_box(state));
    let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap();
    assert!(result.is_ok(), "synthetic bearer must authenticate");
    (elapsed, black_box(result.unwrap().principal))
}

fn executable_sha256() -> String {
    let mut file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
    let size = file.metadata().unwrap().len();
    assert!(size <= 2 * 1024 * 1024 * 1024);
    let mut bytes = [0; 65_536];
    let mut digest = Sha256::new();
    let mut read = 0_u64;
    for _ in 0..=size.div_ceil(65_536) {
        let count = file.read(&mut bytes).unwrap();
        if count == 0 {
            break;
        }
        read += u64::try_from(count).unwrap();
        digest.update(&bytes[..count]);
    }
    assert_eq!(read, size);
    hex(&digest.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn percentile(samples: &[u64], percent: usize) -> u64 {
    assert!(!samples.is_empty() && (1..=100).contains(&percent));
    samples[(samples.len() * percent).div_ceil(100) - 1]
}

#[test]
#[ignore = "release before/after workload; run alone with --nocapture --test-threads=1"]
fn issue123_bearer_comparison() {
    assert!(!black_box(cfg!(debug_assertions)), "use --release");
    let mut system = sysinfo::System::new();
    system.refresh_cpu_all();
    let executable = executable_sha256();
    let (state, request) = fixture();
    for _ in 0..WARMUP {
        black_box(measured(&state, &request));
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        samples.push(measured(&state, &request).0);
    }
    samples.sort_unstable();
    println!(
        "ISSUE123_BEARER {}",
        json!({
            "schema":1, "workload":"bearer_api_principal", "unit":"ns",
            "samples":SAMPLES, "warmup":WARMUP, "percentile":"nearest rank",
            "p50":percentile(&samples, 50), "p95":percentile(&samples, 95),
            "max":samples.last().unwrap(), "profile":"release",
            "build_label":std::env::var("KEEPPEEK_BENCH_BUILD").ok(),
            "run":std::env::var("KEEPPEEK_BENCH_RUN").ok(),
            "harness_sha256":hex(&Sha256::digest(include_bytes!("bearer_benchmarks.rs"))),
            "executable_sha256":executable, "version":env!("CARGO_PKG_VERSION"),
            "os":sysinfo::System::long_os_version(), "arch":std::env::consts::ARCH,
            "cpu":system.cpus().first().map(sysinfo::Cpu::brand),
            "logical_cpus":system.cpus().len(),
            "scope":"real authorization function; synthetic HTTPS request; no socket/browser",
            "clock":"real monotonic clock", "credential_count":1,
            "absolute_regression_budget_ns":1_000_000, "relative_regression_budget_percent":5
        })
    );
    state.webrtc.shutdown();
}
