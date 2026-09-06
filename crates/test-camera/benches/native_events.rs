#![forbid(unsafe_code)]

#[path = "native_events/recorder.rs"]
mod recorder;
#[path = "native_events/stats.rs"]
mod stats;

use anyhow::{Context as _, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use test_camera::TestCamera;

const CHILD_MODE: &str = "KEEPPEEK_NATIVE_PERF_MODE";
const CHILD_ROOT: &str = "KEEPPEEK_NATIVE_PERF_ROOT";
const CHILD_IP: &str = "KEEPPEEK_NATIVE_PERF_IP";
const CHILD_TIMEOUT: Duration = Duration::from_secs(6);
const SUITE_TIMEOUT: Duration = Duration::from_secs(90);
const OUTPUT_BYTES_MAX: u64 = 64 * 1024;
const PAIR_COUNT: usize = 10;
const FIRST_BOTH_BUDGET_MS: f64 = 2500.0;
const FRAGMENT_GAP_BUDGET_MS: f64 = 1500.0;
const CADENCE_DELTA_BUDGET_MS: f64 = 1000.0 / 15.0 + 20.0;
const METRICS: [&str; 3] = [
    "first_both_fragment_ms",
    "max_fragment_gap_ms",
    "shutdown_finalize_ms",
];

#[derive(Clone, Copy, Debug)]
enum Condition {
    Disabled,
    Enabled,
}

impl Condition {
    const fn label(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Enabled => "rtsp-metadata",
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).take(4).collect();
    if args == ["--native-events-child"] {
        return child();
    }
    ensure!(
        args.len() <= 2
            && args
                .iter()
                .all(|arg| matches!(arg.as_str(), "--bench" | "--smoke")),
        "usage: cargo bench --locked -p test-camera --bench native_events -- [--smoke]"
    );
    let smoke = args.iter().any(|arg| arg == "--smoke");
    let started = Instant::now();
    let deadline = started + SUITE_TIMEOUT;
    let camera = recorder::camera()?;
    if !smoke {
        run_pair(0, "warmup", &camera, deadline)?;
    }
    let count = if smoke { 1 } else { PAIR_COUNT };
    let mut pairs = Vec::with_capacity(count);
    for pair in 0..count {
        pairs.push(run_pair(
            pair,
            if smoke { "smoke" } else { "measurement" },
            &camera,
            deadline,
        )?);
    }
    if !smoke {
        report(&pairs)?;
    }
    drop(camera);
    ensure!(Instant::now() < deadline, "suite exceeded 90 seconds");
    println!(
        "NATIVE_EVENT_PERF_COMPLETE {}",
        json!({"smoke": smoke, "measured_pairs": count, "samples_per_condition": count,
            "warmup_pairs": usize::from(!smoke), "gates_passed": true,
            "wall_seconds": started.elapsed().as_secs_f64()})
    );
    Ok(())
}

fn run_pair(
    pair: usize,
    phase: &str,
    camera: &TestCamera,
    deadline: Instant,
) -> anyhow::Result<[Value; 2]> {
    let order = if pair.is_multiple_of(2) {
        [Condition::Disabled, Condition::Enabled]
    } else {
        [Condition::Enabled, Condition::Disabled]
    };
    let mut samples = [Value::Null, Value::Null];
    for (position, condition) in order.into_iter().enumerate() {
        let sample = run_sample(condition, camera, deadline)?;
        println!(
            "NATIVE_EVENT_PERF_SAMPLE {}",
            json!({"phase": phase,
            "pair": pair + 1, "position": position + 1, "sample": sample})
        );
        let first = metric(&sample, METRICS[0])?;
        let gap = metric(&sample, METRICS[1])?;
        ensure!(
            first < FIRST_BOTH_BUDGET_MS,
            "{} first fragments {first:.3} ms exceed {FIRST_BOTH_BUDGET_MS} ms guard",
            condition.label()
        );
        ensure!(
            gap < FRAGMENT_GAP_BUDGET_MS,
            "{} fragment gap {gap:.3} ms exceeds {FRAGMENT_GAP_BUDGET_MS} ms guard",
            condition.label()
        );
        let index = match condition {
            Condition::Disabled => 0,
            Condition::Enabled => 1,
        };
        samples[index] = sample;
    }
    for field in [
        "documents_per_stream",
        "metadata_bytes_per_stream",
        "observation_window_ms",
    ] {
        ensure!(
            samples[0][field] == samples[1][field],
            "conditions differ in {field}"
        );
    }
    Ok(samples)
}

fn report(pairs: &[[Value; 2]]) -> anyhow::Result<()> {
    ensure!(
        pairs.len() == PAIR_COUNT,
        "measurement requires exactly {PAIR_COUNT} pairs"
    );
    let mut cadence_delta_ms = None;
    for name in METRICS {
        let disabled: Vec<_> = pairs
            .iter()
            .map(|pair| metric(&pair[0], name))
            .collect::<Result<_, _>>()?;
        let enabled: Vec<_> = pairs
            .iter()
            .map(|pair| metric(&pair[1], name))
            .collect::<Result<_, _>>()?;
        let deltas: Vec<_> = disabled
            .iter()
            .zip(&enabled)
            .map(|(before, after)| after - before)
            .collect();
        let baseline = stats::summarize(&disabled, PAIR_COUNT).map_err(anyhow::Error::msg)?;
        let result = stats::summarize(&enabled, PAIR_COUNT).map_err(anyhow::Error::msg)?;
        let paired = stats::summarize(&deltas, PAIR_COUNT).map_err(anyhow::Error::msg)?;
        println!(
            "NATIVE_EVENT_PERF_SUMMARY {}",
            json!({"metric": name,
            "disabled": summary_json(&baseline), "rtsp_metadata": summary_json(&result),
            "delta_p50_ms": result.p50 - baseline.p50,
            "delta_p95_ms": result.p95 - baseline.p95,
            "delta_p50_percent": (result.p50 / baseline.p50 - 1.0) * 100.0,
            "delta_p95_percent": (result.p95 / baseline.p95 - 1.0) * 100.0,
            "paired_delta": summary_json(&paired)})
        );
        if name == "max_fragment_gap_ms" {
            cadence_delta_ms = Some(result.p95 - baseline.p95);
        }
    }
    let cadence_delta_ms = cadence_delta_ms.context("missing primary cadence metric")?;
    println!(
        "NATIVE_EVENT_PERF_BUDGET {}",
        json!({
        "enabled_minus_disabled_p95_gap_ms": cadence_delta_ms,
        "p95_gap_delta_budget_ms": CADENCE_DELTA_BUDGET_MS,
        "first_both_fragment_budget_ms": FIRST_BOTH_BUDGET_MS,
        "max_fragment_gap_budget_ms": FRAGMENT_GAP_BUDGET_MS,
        "passed": cadence_delta_ms <= CADENCE_DELTA_BUDGET_MS})
    );
    ensure!(
        cadence_delta_ms <= CADENCE_DELTA_BUDGET_MS,
        "p95 cadence delta {cadence_delta_ms:.3} ms exceeds {CADENCE_DELTA_BUDGET_MS:.3} ms guard"
    );
    Ok(())
}

fn metric(sample: &Value, name: &str) -> anyhow::Result<f64> {
    sample[name]
        .as_f64()
        .filter(|value| value.is_finite() && *value > 0.0)
        .with_context(|| format!("missing or invalid positive {name}"))
}

fn summary_json(summary: &stats::Summary) -> Value {
    json!({"count": summary.count, "p50_ms": summary.p50, "p95_ms": summary.p95,
        "min_ms": summary.min, "max_ms": summary.max})
}

fn child() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .init();
    let condition = match std::env::var(CHILD_MODE)?.as_str() {
        "disabled" => Condition::Disabled,
        "rtsp-metadata" => Condition::Enabled,
        other => bail!("invalid child condition: {other}"),
    };
    let root = PathBuf::from(std::env::var_os(CHILD_ROOT).context("missing child fixture root")?);
    ensure!(root.is_dir(), "child fixture root must already exist");
    let sample = recorder::measure(&root, condition)?;
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("result.json"))?;
    serde_json::to_writer(output, &sample)?;
    Ok(())
}

fn run_sample(
    condition: Condition,
    camera: &TestCamera,
    suite_deadline: Instant,
) -> anyhow::Result<Value> {
    let started = Instant::now();
    ensure!(Instant::now() < suite_deadline, "suite exceeded 90 seconds");
    let root = tempfile::Builder::new()
        .prefix("native-event-perf-")
        .tempdir()?;
    fs::write(
        root.path().join("config.toml"),
        camera.connection().toml_entry("native-perf"),
    )?;
    let stdout_path = root.path().join("stdout.log");
    let stdout = File::create(&stdout_path)?;
    let mut child = ChildGuard(
        Command::new(std::env::current_exe()?)
            .arg("--native-events-child")
            .env(CHILD_MODE, condition.label())
            .env(CHILD_ROOT, root.path())
            .env(CHILD_IP, camera.connection().endpoint_ip().to_string())
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let deadline = suite_deadline.min(Instant::now() + CHILD_TIMEOUT);
    let outcome = wait_child(&mut child.0, deadline, suite_deadline);
    let output = read_bounded(&stdout_path)?;
    if let Err(error) = outcome {
        eprintln!("child stdout: {}", String::from_utf8_lossy(&output));
        return Err(error.context(format!("{} sample failed", condition.label())));
    }
    let mut sample: Value =
        serde_json::from_slice(&read_bounded(&root.path().join("result.json"))?)?;
    ensure!(
        sample["condition"] == condition.label(),
        "wrong child result"
    );
    drop(child);
    root.close()?;
    sample["case_wall_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    Ok(sample)
}

fn wait_child(child: &mut Child, deadline: Instant, suite_deadline: Instant) -> anyhow::Result<()> {
    loop {
        ensure!(
            Instant::now() < suite_deadline,
            "native event suite exceeded 90 seconds"
        );
        ensure!(
            Instant::now() < deadline,
            "native event child exceeded six seconds"
        );
        if let Some(exit) = child.try_wait()? {
            ensure!(exit.success(), "native event child failed: {exit}");
            return Ok(());
        }
        thread::park_timeout(Duration::from_millis(10));
    }
}

fn read_bounded(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(4096);
    File::open(path)?
        .take(OUTPUT_BYTES_MAX + 1)
        .read_to_end(&mut output)?;
    ensure!(
        u64::try_from(output.len())? <= OUTPUT_BYTES_MAX,
        "child output exceeded 64 KiB"
    );
    Ok(output)
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self
            .0
            .try_wait()
            .expect("inspect benchmark child")
            .is_none()
        {
            self.0.kill().expect("terminate benchmark child");
            self.0.wait().expect("reap benchmark child");
        }
    }
}
