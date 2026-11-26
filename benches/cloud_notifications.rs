use std::{
    hint::black_box,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hdrhistogram::Histogram;
use keeppeek::{
    config::cloud_notifications::Settings,
    notifications::{
        Cooldown, CooldownScope, Lifecycle, Runtime, Stage,
        cloud::{CloudSignalRuntime, SignalTransport},
        cloud_protocol::CANONICAL_BODY_BYTES,
        cloud_state::SignalDeliveryOutcome,
        model::{
            Action, AttachmentPolicy, Candidate, Channel, EnrichmentPolicy, FailurePolicy, Filter,
            Rule, Schedule, Severity, Template, Trigger,
        },
    },
};

const STORM_CANDIDATES: usize = 10_000;
const RUN_COUNT: usize = 5;
const SEED: [u8; 32] = [0x61; 32];
const NVR_ID: &str = "01020304-0506-0708-090a-0b0c0d0e0f10";

struct BenchmarkTransport {
    attempts: AtomicU64,
    wire_bytes: AtomicU64,
}

impl SignalTransport for BenchmarkTransport {
    fn send_signal(
        &self,
        _service_origin: &str,
        body: &[u8; CANONICAL_BODY_BYTES],
    ) -> SignalDeliveryOutcome {
        self.attempts.fetch_add(1, Ordering::Relaxed);
        self.wire_bytes
            .fetch_add(u64::try_from(body.len()).unwrap_or(0), Ordering::Relaxed);
        SignalDeliveryOutcome::Accepted
    }
}

struct PassMetrics {
    p50_ns: u64,
    p95_ns: u64,
    max_ns: u64,
    accepted_candidates: u64,
    dropped_candidates: u64,
    pending: usize,
    claimed: usize,
    attempts: u64,
    wire_bytes: u64,
    durable_bytes: u64,
    suppressed_last_day: u32,
    cpu_ms: f64,
    peak_rss_kib: u64,
}

fn make_rule(with_cloud: bool) -> Rule {
    let mut actions = vec![Action {
        enabled: true,
        channel: Channel::Browser,
        destination: String::new(),
        template: Template {
            title: "Person".to_owned(),
            body: "Open {{notification.deep_link}}".to_owned(),
        },
        attachment: AttachmentPolicy::Never,
        allow_second_delivery: false,
    }];
    if with_cloud {
        actions.push(Action {
            enabled: true,
            channel: Channel::KeeppeekService,
            destination: String::new(),
            template: Template {
                title: "Generic".to_owned(),
                body: String::new(),
            },
            attachment: AttachmentPolicy::Never,
            allow_second_delivery: false,
        });
    }
    Rule {
        id: "bench-rule".to_owned(),
        name: "Bench rule".to_owned(),
        enabled: true,
        revision: 0,
        owner_id: "owner-1".to_owned(),
        triggers: vec![Trigger::EventCreated],
        filter: Filter::default(),
        schedule: Schedule {
            timezone: "UTC".to_owned(),
            active_windows: Vec::new(),
            quiet_hours: None,
        },
        cooldowns: vec![Cooldown {
            scope: CooldownScope::Event,
            duration_ms: 1_000,
        }],
        rate_limits: Vec::new(),
        critical_bypass: None,
        enrichment: EnrichmentPolicy {
            deadline_ms: 10_000,
            maximum_revisions: 4,
            maximum_attempts: 2,
            maximum_attachment_bytes: 1_048_576,
            wake_after_deadline: false,
        },
        actions,
        failure: FailurePolicy {
            maximum_attempts: 3,
            maximum_retry_interval_ms: 5_000,
            expiry_ms: 60_000,
        },
    }
}

fn current_unix_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

fn make_candidate(base_ms: i64, seq: usize) -> Candidate {
    Candidate {
        trigger: Trigger::EventCreated,
        source_id: "cam-1".to_owned(),
        source_name: Some("Front Door".to_owned()),
        source_identity: format!("evt-{seq}"),
        lifecycle: Lifecycle::Event,
        event_kind: Some("person".to_owned()),
        payload: None,
        group_ids: Vec::new(),
        zone: None,
        confidence: Some(0.9),
        attachment_path: None,
        canonical_attachment: None,
        icon_key: None,
        image_available: false,
        duration_ms: None,
        severity: Severity::Info,
        reviewed: None,
        bookmarked: None,
        privacy_active: false,
        revision: 1,
        stage: Stage::Preliminary,
        occurred_at_ms: base_ms + i64::try_from(seq).unwrap_or(0),
        deep_link: "/events/bench".to_owned(),
    }
}

fn durable_store_bytes(dir: &Path) -> u64 {
    ["cloud-notifications.db", "cloud-notifications.db-wal"]
        .into_iter()
        .map(|name| std::fs::metadata(dir.join(name)).map_or(0, |meta| meta.len()))
        .sum()
}

#[cfg(windows)]
fn sample_process_cpu_ms_and_peak_rss_kib() -> (f64, u64) {
    use windows::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    };

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle and all FILETIME outputs are initialized stack values.
    let cpu_ms = if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .is_ok()
    {
        let k = (u64::from(kernel.dwHighDateTime) << 32) | u64::from(kernel.dwLowDateTime);
        let u = (u64::from(user.dwHighDateTime) << 32) | u64::from(user.dwLowDateTime);
        (k.saturating_add(u) as f64) / 10_000.0
    } else {
        0.0
    };
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).unwrap_or(0);
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle and counters has exact byte size `size`.
    let peak_rss_kib =
        if unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) }.is_ok() {
            u64::try_from(counters.PeakWorkingSetSize).unwrap_or(0) / 1_024
        } else {
            0
        };
    (cpu_ms, peak_rss_kib)
}

#[cfg(not(windows))]
fn sample_process_cpu_ms_and_peak_rss_kib() -> (f64, u64) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let pid = Pid::from_u32(std::process::id());
    let Ok(mut system) = System::new() else {
        return (0.0, 0);
    };
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );
    let Some(process) = system.process(pid) else {
        return (0.0, 0);
    };
    (
        process.accumulated_cpu_time() as f64,
        process.memory() / 1_024,
    )
}

fn run_enqueue_pass(with_cloud: bool) -> PassMetrics {
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-cloud-bench-{}-{}",
        if with_cloud { "cloud" } else { "base" },
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.toml");
    std::fs::write(
        dir.join("secrets.toml"),
        format!("CLOUD_KEY = \"{}\"\n", STANDARD.encode(SEED)),
    )
    .unwrap();
    let transport = Arc::new(BenchmarkTransport {
        attempts: AtomicU64::new(0),
        wire_bytes: AtomicU64::new(0),
    });
    let base_ms = current_unix_ms();
    let cloud = CloudSignalRuntime::open_with_transport(
        config_path.clone(),
        &dir.join("cloud-notifications.db"),
        Settings {
            revision: 1,
            enabled: with_cloud,
            image_previews_enabled: false,
            service_origin: Some("https://service.keeppeek.example".to_owned()),
            nvr_id: Some(NVR_ID.to_owned()),
            signing_key_version: Some(1),
            signing_key: Some("{secret:CLOUD_KEY}".to_owned()),
        },
        transport.clone(),
        base_ms,
    )
    .unwrap();

    let runtime = Runtime::open_with_cloud(&config_path, cloud.clone()).unwrap();
    let handle = runtime.handle();
    let saved = handle
        .save_draft(make_rule(with_cloud), 0, base_ms)
        .unwrap();
    handle
        .activate(
            "bench-rule",
            "owner-1",
            0,
            saved.draft_revision,
            base_ms + 1,
        )
        .unwrap();

    let candidates: Vec<Candidate> = (0..STORM_CANDIDATES)
        .map(|seq| make_candidate(base_ms, seq))
        .collect();
    let (cpu_before, rss_before) = sample_process_cpu_ms_and_peak_rss_kib();
    let mut hist = Histogram::<u64>::new(3).unwrap();
    for candidate in candidates {
        let started = Instant::now();
        handle.publish_blocking(black_box(candidate)).unwrap();
        let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        hist.record(elapsed_ns.max(1)).unwrap();
    }
    let _ = handle.rules("owner-1").unwrap();
    cloud.flush_staged().unwrap();
    let (cpu_after, rss_after) = sample_process_cpu_ms_and_peak_rss_kib();
    let (accepted_candidates, dropped_candidates) = handle.candidate_counts();
    let (pending, claimed) = cloud.store().queue_depth();
    let status = cloud.status(base_ms + 20_000);
    let durable_bytes = durable_store_bytes(&dir);
    runtime.shutdown();
    let _ = std::fs::remove_dir_all(dir);

    assert_eq!(
        accepted_candidates,
        u64::try_from(STORM_CANDIDATES).unwrap()
    );
    assert_eq!(dropped_candidates, 0);
    let attempts = transport.attempts.load(Ordering::Relaxed);
    let wire_bytes = transport.wire_bytes.load(Ordering::Relaxed);
    if with_cloud {
        assert!(pending <= 1 && claimed <= 1);
        assert_eq!(
            wire_bytes,
            attempts * u64::try_from(CANONICAL_BODY_BYTES).unwrap()
        );
        let accounted = u64::from(status.suppressed_last_day)
            + attempts
            + u64::try_from(pending + claimed).unwrap();
        assert_eq!(
            accounted,
            u64::try_from(STORM_CANDIDATES).unwrap(),
            "all {STORM_CANDIDATES} candidates must be accounted for across suppressed ({}), accepted ({attempts}), and bounded pending/claimed ({pending}/{claimed})",
            status.suppressed_last_day
        );
    }
    PassMetrics {
        p50_ns: hist.value_at_quantile(0.50),
        p95_ns: hist.value_at_quantile(0.95),
        max_ns: hist.max(),
        accepted_candidates,
        dropped_candidates,
        pending,
        claimed,
        attempts,
        wire_bytes,
        durable_bytes,
        suppressed_last_day: status.suppressed_last_day,
        cpu_ms: (cpu_after - cpu_before).max(0.0),
        peak_rss_kib: rss_after.max(rss_before),
    }
}

fn main() {
    let mut base_p95s = Vec::with_capacity(RUN_COUNT);
    let mut cloud_p95s = Vec::with_capacity(RUN_COUNT);
    let mut base_cpus = Vec::with_capacity(RUN_COUNT);
    let mut cloud_cpus = Vec::with_capacity(RUN_COUNT);
    let mut max_rss_kib = 0_u64;
    for run in 1..=RUN_COUNT {
        let base = run_enqueue_pass(false);
        let cloud = run_enqueue_pass(true);
        base_p95s.push(base.p95_ns);
        cloud_p95s.push(cloud.p95_ns);
        base_cpus.push(base.cpu_ms);
        cloud_cpus.push(cloud.cpu_ms);
        max_rss_kib = max_rss_kib.max(base.peak_rss_kib).max(cloud.peak_rss_kib);
        println!(
            "run={run} candidates={STORM_CANDIDATES} accepted=({}/{}) dropped=({}/{}) baseline_ns=({},{},{}) cloud_ns=({},{},{}) cpu_ms=({:.2},{:.2}) peak_rss_kib=({},{}) pending={} claimed={} attempts={} wire_bytes={} durable_bytes={} suppressed={}",
            base.accepted_candidates,
            cloud.accepted_candidates,
            base.dropped_candidates,
            cloud.dropped_candidates,
            base.p50_ns,
            base.p95_ns,
            base.max_ns,
            cloud.p50_ns,
            cloud.p95_ns,
            cloud.max_ns,
            base.cpu_ms,
            cloud.cpu_ms,
            base.peak_rss_kib,
            cloud.peak_rss_kib,
            cloud.pending,
            cloud.claimed,
            cloud.attempts,
            cloud.wire_bytes,
            cloud.durable_bytes,
            cloud.suppressed_last_day
        );
    }
    base_p95s.sort_unstable();
    cloud_p95s.sort_unstable();
    base_cpus.sort_by(f64::total_cmp);
    cloud_cpus.sort_by(f64::total_cmp);
    let median_base = base_p95s[RUN_COUNT / 2];
    let median_cloud = cloud_p95s[RUN_COUNT / 2];
    let median_base_cpu = base_cpus[RUN_COUNT / 2];
    let median_cloud_cpu = cloud_cpus[RUN_COUNT / 2];
    let delta_pct = if median_base == 0 {
        0.0
    } else {
        ((median_cloud as f64 - median_base as f64) / median_base as f64) * 100.0
    };
    println!(
        "summary runs={RUN_COUNT} median_baseline_p95_ns={median_base} median_cloud_p95_ns={median_cloud} delta_pct={delta_pct:.2}% median_cpu_ms=({median_base_cpu:.2},{median_cloud_cpu:.2}) peak_rss_kib={max_rss_kib} body_bytes_per_signal={CANONICAL_BODY_BYTES}"
    );
}
