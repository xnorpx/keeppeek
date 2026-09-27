use anyhow::{Context, Result};
use keeppeek::storage::{
    VideoCodec,
    engine::admission_benchmark::{AdmissionReport, FixtureFrame, measure_admission},
};
use serde::Serialize;
use std::{
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Serialize)]
struct WorkloadReport {
    fixture: String,
    fixture_sha256: String,
    camera_count: usize,
    measured_runs: usize,
    warmup_runs: usize,
    runs: Vec<AdmissionReport>,
}

#[derive(Serialize)]
struct Report {
    mode: String,
    revision: String,
    executable_sha256: String,
    operating_system: &'static str,
    architecture: &'static str,
    timer_scope: &'static str,
    workloads: Vec<WorkloadReport>,
}

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures = std::env::var_os("KEEPPEEK_PREROLL_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/event-preroll-fixtures"));
    let mode =
        std::env::var("KEEPPEEK_PREROLL_BENCH_MODE").unwrap_or_else(|_| "disabled".to_owned());
    anyhow::ensure!(matches!(
        mode.as_str(),
        "baseline" | "disabled" | "enabled" | "sink"
    ));
    let samples = std::env::var("KEEPPEEK_PREROLL_BENCH_RUNS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(30);
    anyhow::ensure!((30..=100).contains(&samples), "use30–100 measured runs");
    if mode == "sink" {
        #[cfg(feature = "event-preroll-benchmark-current")]
        return measure_sinks(&root, &fixtures, samples);
        #[cfg(not(feature = "event-preroll-benchmark-current"))]
        anyhow::bail!("the historical baseline has no event sink");
    }
    let mut report = Report {
        mode: mode.clone(),
        revision: revision(&root),
        executable_sha256: executable_hash()?,
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        timer_scope: "RecordingAdmission::ingest_at only; queue drain, history selection, ShortTermBuffer and fixture cloning are outside the per-frame timer",
        workloads: Vec::new(),
    };
    for height in [1080, 2160] {
        for codec in ["h264", "h265"] {
            for gop in [1, 10] {
                let name = format!("{height}p-{codec}-gop{gop}.mp4");
                for cameras in [1, 127] {
                    report.workloads.push(measure(
                        &fixtures,
                        &name,
                        cameras,
                        samples,
                        mode == "enabled",
                    )?);
                }
            }
        }
    }
    let output = std::env::var_os("KEEPPEEK_PREROLL_BENCH_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(format!("target/preroll-{mode}.json")));
    std::fs::write(output, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

fn measure(
    directory: &Path,
    name: &str,
    cameras: usize,
    samples: usize,
    enabled: bool,
) -> Result<WorkloadReport> {
    let path = directory.join(name);
    let frames = read_fixture(&path)?;
    eprintln!("{name}: cameras={cameras} enabled={enabled},3 warmups+{samples} runs");
    for _ in 0..3 {
        std::hint::black_box(measure_admission(&frames, cameras, enabled));
    }
    let runs = (0..samples)
        .map(|_| measure_admission(&frames, cameras, enabled))
        .collect();
    Ok(WorkloadReport {
        fixture: name.to_owned(),
        fixture_sha256: file_hash(&path)?,
        camera_count: cameras,
        measured_runs: samples,
        warmup_runs: 3,
        runs,
    })
}

fn read_fixture(path: &Path) -> Result<Vec<FixtureFrame>> {
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
        .context("fixture has no video track")?;
    let config = track.media_config_for_description(1)?;
    let decoder = track
        .video_decoder_config()?
        .context("fixture has no decoder configuration")?;
    let (codec, parameters) = parameters(&config)?;
    let timescale = track.timescale();
    let count = track.sample_count();
    anyhow::ensure!(timescale > 0 && count <= 1000);
    let mut frames = Vec::with_capacity(count as usize);
    for index in 1..=count {
        let sample = reader
            .read_sample(track_id, index)?
            .context("fixture sample missing")?;
        let mut data = Vec::new();
        if sample.is_sync {
            for parameter in &parameters {
                data.extend_from_slice(&u32::try_from(parameter.len())?.to_be_bytes());
                data.extend_from_slice(parameter);
            }
        }
        data.extend_from_slice(&sample.bytes);
        frames.push(FixtureFrame {
            codec,
            width: u32::from(decoder.width),
            height: u32::from(decoder.height),
            keyframe: sample.is_sync,
            data: data.into(),
            offset: Duration::from_secs_f64(sample.start_time as f64 / f64::from(timescale)),
        });
    }
    validate_fixture(path, &frames)?;
    Ok(frames)
}

fn validate_fixture(path: &Path, frames: &[FixtureFrame]) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid fixture name")?;
    let gop = if name.ends_with("gop1.mp4") {
        1.0
    } else if name.ends_with("gop10.mp4") {
        10.0
    } else {
        anyhow::bail!("unknown fixture GOP")
    };
    let height = if name.starts_with("1080p-") {
        1080
    } else {
        2160
    };
    let codec = if name.contains("-h264-") {
        VideoCodec::H264
    } else {
        VideoCodec::H265
    };
    anyhow::ensure!(
        frames.first().is_some_and(|frame| frame.keyframe),
        "fixture starts without a keyframe"
    );
    anyhow::ensure!(
        frames.iter().all(|frame| frame.height == height
            && frame.width == height * 16 / 9
            && frame.codec == codec),
        "fixture format does not match its label"
    );
    let keyframes = frames
        .iter()
        .filter(|frame| frame.keyframe)
        .map(|frame| frame.offset)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        keyframes.len() >= 2,
        "fixture has insufficient GOP boundaries"
    );
    anyhow::ensure!(
        keyframes
            .windows(2)
            .all(|pair| ((pair[1] - pair[0]).as_secs_f64() - gop).abs() < 0.01),
        "fixture GOP cadence does not match its label"
    );
    Ok(())
}

fn parameters(config: &mp4::MediaConfig) -> Result<(VideoCodec, Vec<&[u8]>)> {
    match config {
        mp4::MediaConfig::AvcConfig(config) => Ok((
            VideoCodec::H264,
            vec![&config.seq_param_set, &config.pic_param_set],
        )),
        mp4::MediaConfig::HevcConfig(config) => Ok((
            VideoCodec::H265,
            vec![&config.vps, &config.sps, &config.pps],
        )),
        _ => anyhow::bail!("unsupported fixture codec"),
    }
}

fn executable_hash() -> Result<String> {
    file_hash(&std::env::current_exe()?)
}

fn file_hash(path: &Path) -> Result<String> {
    use sha2::{Digest as _, Sha256};
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(std::fs::read(path)?);
    let mut output = String::with_capacity(digest.len() * 2);
    for &byte in digest.iter() {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(output)
}

fn revision(root: &Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

#[cfg(feature = "event-preroll-benchmark-current")]
fn measure_sinks(root: &Path, fixtures: &Path, samples: usize) -> Result<()> {
    use keeppeek::storage::engine::admission_benchmark::sink::{measure_sink, output_root};
    let mut results = Vec::new();
    for height in [1080, 2160] {
        for codec in ["h264", "h265"] {
            for gop in [1, 10] {
                let name = format!("{height}p-{codec}-gop{gop}.mp4");
                let frames = read_fixture(&fixtures.join(&name))?;
                let cameras = if gop == 1 && codec == "h265" { 127 } else { 1 };
                let mut runs = Vec::new();
                eprintln!("sink {name}: cameras={cameras},3 warmups+{samples} runs");
                for run in 0..samples + 3 {
                    let directory = output_root().join(format!(
                        "{}-{height}-{codec}-{gop}-{run}",
                        std::process::id()
                    ));
                    let report = measure_sink(&frames, cameras, &directory)?;
                    if run >= 3 {
                        runs.push(report);
                    }
                }
                results
                    .push(serde_json::json!({"fixture": name, "fixture_sha256": file_hash(&fixtures.join(&name))?, "cameras": cameras, "runs": runs}));
            }
        }
    }
    let output = std::env::var_os("KEEPPEEK_PREROLL_BENCH_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/preroll-sink.json"));
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&serde_json::json!({
            "revision": revision(root), "executable_sha256": executable_hash()?,
            "commit_observer": "catalog fragment insertion", "commit_poll_sleep_ms": 1, "workloads": results
        }))?,
    )?;
    Ok(())
}
