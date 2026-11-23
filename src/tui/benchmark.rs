//! Paired release benchmark for the local dashboard's effect on the ingress counters.
//! Each observation starts a fresh process so allocator high-water marks from
//! the enabled condition cannot contaminate the disabled condition.
use super::{Latest, render, snapshot::Snapshot};
use crate::{
    cameras::{CameraConfig, VideoEncoding},
    config::Config,
    keeppeek::StreamKind,
    logging::LogHub,
    runtime::{FacadeSender, Router, RouterMessage},
    server::{ServerState, dashboard_health},
    stats::{
        CameraReport, HealthRegistry, IngressSnapshot, IngressStats, audio_report, video_report,
    },
    storage::{RecordingDemand, RecordingHealthRegistry, StorageConfig},
    webrtc::WebRtc,
};
use ratatui::{Terminal, backend::TestBackend};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use sysinfo::{ProcessesToUpdate, System};

const CAMERAS: u8 = 127;
const PAIRS: usize = 30;
const TICK: Duration = Duration::from_millis(40);
const REPORT_PERIOD: Duration = Duration::from_secs(10);
// Give every staggered source a full reporting period before measuring.
const WARMUP_SECONDS: u64 = REPORT_PERIOD.as_secs();
const MEASURE_SECONDS_DEFAULT: u64 = 40;
const MEASURE_SECONDS_ENV: &str = "KEEPPEEK_TUI_BENCHMARK_MEASURE_SECONDS";
const CHILD_MODE: &str = "KEEPPEEK_TUI_BENCHMARK_CHILD";
const MARKER: &str = "KEEPPEEK_TUI_SAMPLE ";

fn benchmark_measure_seconds() -> u64 {
    let Ok(configured) = std::env::var(MEASURE_SECONDS_ENV) else {
        return MEASURE_SECONDS_DEFAULT;
    };
    let value = configured
        .parse::<u64>()
        .expect("benchmark measurement window must be an integer");
    assert!(
        (20..=90).contains(&value) && value.is_multiple_of(10),
        "benchmark measurement window must be 20 to 90 seconds in ten-second increments"
    );
    value
}

struct Source {
    camera: CameraConfig,
    kind: StreamKind,
    ingress: IngressStats,
    previous: IngressSnapshot,
}

impl Source {
    fn new(camera: CameraConfig, kind: StreamKind) -> Self {
        let mut ingress = IngressStats::new();
        let (width, height) = match kind {
            StreamKind::Main => (1920, 1080),
            StreamKind::Sub => (640, 360),
        };
        ingress.set_stream_info(VideoEncoding::H264, width, height, 25.0);
        let previous = ingress.snapshot();
        Self {
            camera,
            kind,
            ingress,
            previous,
        }
    }

    fn receive(&mut self, tick: usize) {
        self.ingress.on_video_frame(tick.is_multiple_of(25), 16_384);
        self.ingress.on_audio_frame(160);
        self.ingress.on_audio_frame(160);
    }

    fn publish(&mut self, registry: &HealthRegistry) {
        let now = self.ingress.snapshot();
        let rates = now.rates_since(&self.previous);
        let video = video_report(
            self.kind,
            &now,
            &rates,
            self.ingress.codec.as_ref(),
            self.ingress.width,
            self.ingress.height,
            self.ingress.expected_fps,
        );
        let audio = audio_report(&now, &rates, Some("aac")).unwrap();
        registry.publish_from(
            CameraReport {
                ip: self.camera.ip,
                name: self.camera.name.clone(),
                brand: None,
                port: 554,
                streams: vec![video, audio],
            },
            self.kind,
        );
        self.previous = now;
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Sample {
    ingress_batch_ns: Vec<u64>,
    collector_us: Vec<u64>,
    projection_us: Vec<u64>,
    render_us: Vec<u64>,
    process_cpu_ms: u64,
    rss_bytes: u64,
    snapshot_rows: usize,
    pending_snapshots: usize,
    superseded_snapshots: usize,
}

fn micros(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_micros()).unwrap()
}

fn wait_until(start: Instant, seconds: u64) {
    thread::sleep((start + Duration::from_secs(seconds)).saturating_duration_since(Instant::now()));
}

fn process_sample(system: &mut System) -> (u64, u64) {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    let process = system.process(pid).unwrap();
    (process.accumulated_cpu_time(), process.memory())
}

fn fleet(count: u8) -> (ServerState, HealthRegistry, Vec<Source>) {
    let cameras: Vec<CameraConfig> = (1..=count)
        .map(|n| {
            serde_json::from_value(serde_json::json!({
                "ip": IpAddr::V4(Ipv4Addr::new(192, 0, 2, n)), "name": format!("camera-{n:03}"), "recording_mode": "both",
            }))
            .unwrap()
        })
        .collect();
    let registry = HealthRegistry::new();
    let mut sources: Vec<_> = cameras
        .iter()
        .flat_map(|camera| {
            [StreamKind::Main, StreamKind::Sub].map(|kind| Source::new(camera.clone(), kind))
        })
        .collect();
    for source in &mut sources {
        source.receive(0);
        source.publish(&registry);
    }
    let state = ServerState::new(
        &Config::default(),
        &HashMap::from([("cameras".into(), cameras)]),
        &HashMap::new(),
        &StorageConfig::default(),
        RecordingDemand::new(Duration::ZERO),
        WebRtc::new(),
    )
    .with_health_registry(registry.clone());
    (state, registry, sources)
}

fn collect_samples(
    start: Instant,
    measure_seconds: u64,
    latest: Latest,
    sender: &FacadeSender<RouterMessage>,
    state: &ServerState,
) -> Sample {
    let redactor = LogHub::default();
    let mut metrics = Sample::default();
    for second in (0..WARMUP_SECONDS + measure_seconds).step_by(2) {
        wait_until(start, second);
        let collect = Instant::now();
        let health = dashboard_health(sender, state).expect("fixture health");
        let collector_us = micros(collect);
        let projection = Instant::now();
        let next = Arc::new(Snapshot::from_health(health, &redactor));
        let projection_us = micros(projection);
        assert_eq!(next.rows.len(), usize::from(CAMERAS) * 4);
        metrics.snapshot_rows = metrics.snapshot_rows.max(next.rows.len());
        let mut slot = latest.lock().unwrap();
        metrics.superseded_snapshots += usize::from(slot.update(Some(next), Instant::now()));
        metrics.pending_snapshots = metrics
            .pending_snapshots
            .max(usize::from(slot.unread_snapshot));
        if second >= WARMUP_SECONDS {
            metrics.collector_us.push(collector_us);
            metrics.projection_us.push(projection_us);
        }
    }
    metrics
}

fn render_samples(start: Instant, measure_seconds: u64, latest: Latest) -> Vec<u64> {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut nav = render::Navigation::default();
    let mut durations = Vec::new();
    for second in 0..WARMUP_SECONDS + measure_seconds {
        wait_until(start, second);
        let current = latest.lock().unwrap().read_for_render().snapshot;
        let elapsed = current
            .as_ref()
            .map_or(Duration::ZERO, |s| s.collected.elapsed());
        let draw = Instant::now();
        terminal
            .draw(|frame| render::draw(frame, current.as_deref(), &mut nav, elapsed, None))
            .unwrap();
        if second >= WARMUP_SECONDS {
            assert!(
                current.is_some(),
                "dashboard did not present its first observation"
            );
            durations.push(micros(draw));
        }
    }
    durations
}

fn ingress_samples(
    start: Instant,
    measure_seconds: u64,
    sources: &mut [Source],
    registry: &HealthRegistry,
    system: &mut System,
) -> (Vec<u64>, u64, u64) {
    // Measure complete ten-second reporting periods to avoid favoring either condition.
    let ticks_per_second = 1_000 / TICK.as_millis();
    let warmup_ticks = usize::try_from(u128::from(WARMUP_SECONDS) * ticks_per_second).unwrap();
    let report_ticks = usize::try_from(REPORT_PERIOD.as_millis() / TICK.as_millis()).unwrap();
    let measured_ticks = usize::try_from(u128::from(measure_seconds) * ticks_per_second).unwrap();
    let mut ingress_batch_ns = Vec::with_capacity(measured_ticks);
    let mut cpu_start = 0;
    let mut rss_bytes = 0;
    for tick in 0..warmup_ticks + measured_ticks {
        thread::sleep(
            (start + TICK * u32::try_from(tick).unwrap()).saturating_duration_since(Instant::now()),
        );
        if tick == warmup_ticks {
            cpu_start = process_sample(system).0;
        }
        let begin = Instant::now();
        for (i, source) in sources.iter_mut().enumerate() {
            source.receive(tick + 1);
            if (tick + i).is_multiple_of(report_ticks) {
                source.publish(registry);
            }
        }
        let elapsed = u64::try_from(begin.elapsed().as_nanos()).unwrap();
        if tick >= warmup_ticks {
            ingress_batch_ns.push(elapsed);
        }
        if tick >= warmup_ticks && tick.is_multiple_of(25) {
            rss_bytes = rss_bytes.max(process_sample(system).1);
        }
    }
    thread::sleep(
        (start + Duration::from_secs(WARMUP_SECONDS + measure_seconds))
            .saturating_duration_since(Instant::now()),
    );
    let (cpu_end, rss_end) = process_sample(system);
    let process_cpu_ms = cpu_end.saturating_sub(cpu_start);
    rss_bytes = rss_bytes.max(rss_end);
    (ingress_batch_ns, process_cpu_ms, rss_bytes)
}

fn run_observation(enabled: bool, measure_seconds: u64) -> Sample {
    let (state, registry, mut sources) = fleet(CAMERAS);
    let (mut router, sender) = Router::new().unwrap();
    let router_thread = thread::spawn(move || {
        while !router.is_shutting_down() {
            router.wait_and_drain(None).unwrap();
        }
    });
    let mut system = System::new().expect("process metrics must be available for qualification");
    let start = Instant::now();
    let result = thread::scope(|scope| {
        let latest = Latest::default();
        let collector = enabled.then(|| {
            let slot = latest.clone();
            let (sender, state) = (&sender, &state);
            scope.spawn(move || collect_samples(start, measure_seconds, slot, sender, state))
        });
        let renderer =
            enabled.then(|| scope.spawn(move || render_samples(start, measure_seconds, latest)));
        let (ingress_batch_ns, process_cpu_ms, rss_bytes) =
            ingress_samples(start, measure_seconds, &mut sources, &registry, &mut system);
        let mut sample = collector.map_or_else(Sample::default, |thread| thread.join().unwrap());
        if let Some(renderer) = renderer {
            sample.render_us = renderer.join().unwrap();
        }
        sample.ingress_batch_ns = ingress_batch_ns;
        sample.process_cpu_ms = process_cpu_ms;
        sample.rss_bytes = rss_bytes;
        sample
    });
    sender.send(RouterMessage::Shutdown).unwrap();
    router_thread.join().unwrap();
    result
}

#[test]
fn audio_rows_leave_container_recording_evidence_with_video() {
    let (state, _, _) = fleet(1);
    let writers = RecordingHealthRegistry::default();
    for source in ["main", "sub"] {
        writers.note_progress(&format!("camera-001/{source}"), Duration::from_secs(1));
    }
    let state = state.with_recording_health(writers);
    let (mut router, sender) = Router::new().unwrap();
    let worker = thread::spawn(move || {
        while !router.is_shutting_down() {
            router.wait_and_drain(None).unwrap();
        }
    });
    let snapshot = Snapshot::from_health(
        dashboard_health(&sender, &state).unwrap(),
        &LogHub::default(),
    );
    sender.send(RouterMessage::Shutdown).unwrap();
    worker.join().unwrap();
    assert_eq!(snapshot.rows.len(), 4);
    for row in snapshot.rows {
        let measurement = row.measurement.unwrap();
        let video_recording = (row.medium == "video").then_some(true);
        assert_eq!(measurement.recording_requested, video_recording);
        assert_eq!(measurement.recording_progress, video_recording);
    }
}

fn percentile(values: &[u64], percent: usize) -> u64 {
    let mut ordered = values.to_vec();
    ordered.sort_unstable();
    ordered[(ordered.len() * percent).div_ceil(100) - 1]
}

fn observation(enabled: bool) -> Sample {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-dashboard-bench-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tui::benchmark::dashboard_overhead",
            "--ignored",
            "--nocapture",
        ])
        .current_dir(&directory)
        .env("APPDATA", &directory)
        .env("HOME", &directory)
        .env("XDG_CONFIG_HOME", &directory)
        .env(CHILD_MODE, if enabled { "on" } else { "off" })
        .output()
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix(MARKER))
        .expect("child observation");
    serde_json::from_str(line).unwrap()
}

#[test]
#[ignore = "30 paired release samples, approximately 42 minutes at the default window"]
fn dashboard_overhead() {
    assert!(
        !std::hint::black_box(cfg!(debug_assertions)),
        "use cargo test --release for measured results"
    );
    let measure_seconds = benchmark_measure_seconds();
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        assert!(matches!(mode.as_str(), "on" | "off"));
        println!(
            "{MARKER}{}",
            serde_json::to_string(&run_observation(mode == "on", measure_seconds)).unwrap()
        );
        return;
    }
    let mut off_samples = vec![];
    let mut on_samples = vec![];
    for pair in 0..PAIRS {
        let first_enabled = !pair.is_multiple_of(2);
        let first = observation(first_enabled);
        let second = observation(!first_enabled);
        let (off, on) = if first_enabled {
            (second, first)
        } else {
            (first, second)
        };
        println!(
            "{}",
            serde_json::json!({"pair": pair + 1, "off": off, "on": on})
        );
        off_samples.push(off);
        on_samples.push(on);
    }
    for p in [50, 95] {
        let summarize = |samples: &[Sample]| {
            let ingress: Vec<_> = samples
                .iter()
                .flat_map(|s| s.ingress_batch_ns.iter().copied())
                .collect();
            let cpu: Vec<_> = samples.iter().map(|s| s.process_cpu_ms).collect();
            let rss: Vec<_> = samples.iter().map(|s| s.rss_bytes).collect();
            [
                percentile(&ingress, p),
                percentile(&cpu, p),
                percentile(&rss, p),
            ]
        };
        // With 30 independent pairs, the 10th/21st order statistics give a
        // two-sided 95.72% binomial interval for the median paired change.
        let mut paired_changes: Vec<_> = off_samples
            .iter()
            .zip(&on_samples)
            .map(|(off, on)| {
                let a = percentile(&off.ingress_batch_ns, p) as f64;
                let b = percentile(&on.ingress_batch_ns, p) as f64;
                (b - a) * 100.0 / a
            })
            .collect();
        paired_changes.sort_by(f64::total_cmp);
        let off = summarize(&off_samples);
        let on = summarize(&on_samples);
        let delta = std::array::from_fn::<_, 3, _>(|i| i128::from(on[i]) - i128::from(off[i]));
        let collectors: Vec<_> = on_samples
            .iter()
            .flat_map(|s| s.collector_us.iter().copied())
            .collect();
        let projections: Vec<_> = on_samples
            .iter()
            .flat_map(|s| s.projection_us.iter().copied())
            .collect();
        let renders: Vec<_> = on_samples
            .iter()
            .flat_map(|s| s.render_us.iter().copied())
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "percentile": p, "measurement_seconds": measure_seconds,
                "units": ["fleet_ingress_ns", "process_cpu_ms", "process_rss_bytes"],
                "off": off, "on": on, "delta": delta, "ingress_change_percent": 100.0 * delta[0] as f64 / off[0] as f64,
                "paired_ingress_change_percent_interval": [paired_changes[9], paired_changes[20]],
            "collector_us": percentile(&collectors, p), "projection_us": percentile(&projections, p),
                "render_us": percentile(&renders, p),
                "snapshot_rows_high_water": on_samples.iter().map(|s| s.snapshot_rows).max(),
                "pending_snapshots_high_water": on_samples.iter().map(|s| s.pending_snapshots).max(),
            "superseded_snapshots_high_water": on_samples.iter().map(|s| s.superseded_snapshots).max(),
                "recording_gaps": "requires separate recorded-media qualification",
            })
        );
    }
}
