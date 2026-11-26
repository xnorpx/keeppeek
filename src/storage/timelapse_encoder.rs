//! Capability-gated incremental silent MP4 timelapse encoder worker.

pub mod overlay;

use crate::storage::{
    RecordingCatalogHandle,
    timelapse::{
        MAX_OUTPUT_BYTES, ManifestOutputMetadata, SampleDecodeWindow, TimelapseManifest,
        TimelapseOutputPreview, TimelapsePlan, TimelapseSample, read_sample_decode_window,
        validate_output_settings,
    },
};
use sha2::{Digest as _, Sha256};
use std::{
    fmt,
    fs::{self, File},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(20);
const HASH_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelapseEncoderCapability {
    pub available: bool,
    pub binary: String,
    pub video_encoder: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TimelapseEncodeRequest {
    pub export_root: PathBuf,
    pub job_id: String,
    pub artifact_id: String,
    pub file_name: String,
    pub manifest_file_name: String,
    pub source_identity: Option<String>,
    pub output_width: u32,
    pub output_height: u32,
    pub output_fps: u32,
    pub timestamp_overlay: bool,
    pub allow_partial: bool,
    pub confirmed_expensive: bool,
    pub max_output_bytes: u64,
    pub ffmpeg_binary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelapseEncodeResult {
    pub video_path: PathBuf,
    pub manifest_path: PathBuf,
    pub video_sha256: String,
    pub video_bytes: u64,
    pub manifest_sha256: String,
    pub manifest_bytes: u64,
    pub encoded_frames: u32,
    pub manifest: TimelapseManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelapseEncodeError {
    EncoderUnavailable(String),
    InvalidRequest(String),
    PartialResultRequiresAllowance {
        gap_count: u32,
    },
    ConfirmationRequired,
    DiskPressure(String),
    Cancelled,
    Timeout,
    OutputSizeExceeded {
        limit_bytes: u64,
    },
    DecodeFailed {
        interval_start_ms: i64,
        interval_end_ms: i64,
        detail: String,
    },
    EncodeFailed(String),
}

impl fmt::Display for TimelapseEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EncoderUnavailable(detail) => {
                write!(formatter, "timelapse encoder unavailable: {detail}")
            }
            Self::InvalidRequest(detail) => {
                write!(formatter, "invalid timelapse request: {detail}")
            }
            Self::PartialResultRequiresAllowance { gap_count } => write!(
                formatter,
                "timelapse range has {gap_count} missing or unavailable interval(s); allow partial output to proceed"
            ),
            Self::ConfirmationRequired => {
                formatter.write_str("expensive timelapse job requires explicit confirmation")
            }
            Self::DiskPressure(detail) => {
                write!(formatter, "storage headroom below safety reserve: {detail}")
            }
            Self::Cancelled => formatter.write_str("timelapse generation was cancelled"),
            Self::Timeout => {
                formatter.write_str("timelapse generation exceeded wall-clock deadline")
            }
            Self::OutputSizeExceeded { limit_bytes } => {
                write!(
                    formatter,
                    "timelapse output exceeded limit of {limit_bytes} bytes"
                )
            }
            Self::DecodeFailed {
                interval_start_ms,
                interval_end_ms,
                detail,
            } => write!(
                formatter,
                "decode failed for source interval [{interval_start_ms}, {interval_end_ms}]: {detail}"
            ),
            Self::EncodeFailed(detail) => write!(formatter, "timelapse encoder failed: {detail}"),
        }
    }
}

impl std::error::Error for TimelapseEncodeError {}

/// Probes whether the local FFmpeg worker and H.264 encoder are available.
pub fn probe_local_encoder(ffmpeg_binary: &str) -> TimelapseEncoderCapability {
    let output = Command::new(ffmpeg_binary)
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return TimelapseEncoderCapability {
            available: false,
            binary: ffmpeg_binary.to_owned(),
            video_encoder: None,
        };
    };
    if !output.status.success() {
        return TimelapseEncoderCapability {
            available: false,
            binary: ffmpeg_binary.to_owned(),
            video_encoder: None,
        };
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let video_encoder = if listing.contains("libx264") {
        Some("libx264".to_owned())
    } else if listing.contains("libopenh264") {
        Some("libopenh264".to_owned())
    } else {
        None
    };
    TimelapseEncoderCapability {
        available: video_encoder.is_some(),
        binary: ffmpeg_binary.to_owned(),
        video_encoder,
    }
}

/// Encodes a planned timelapse incrementally into a silent MP4 and provenance manifest.
pub fn encode_timelapse_mp4<F, P>(
    catalog: &RecordingCatalogHandle,
    plan: &TimelapsePlan,
    request: &TimelapseEncodeRequest,
    deadline: Instant,
    cancel: &AtomicBool,
    mut check_headroom: F,
    mut on_progress: P,
) -> Result<TimelapseEncodeResult, TimelapseEncodeError>
where
    F: FnMut() -> Result<(), String>,
    P: FnMut(u32, u32),
{
    validate_encode_admission(plan, request, &mut check_headroom)?;
    let capability = probe_local_encoder(&request.ffmpeg_binary);
    let Some(encoder_name) = capability.video_encoder else {
        return Err(TimelapseEncodeError::EncoderUnavailable(format!(
            "{} with H.264 encoder was not found",
            request.ffmpeg_binary
        )));
    };
    let artifact_dir =
        validated_artifact_dir(&request.export_root, &request.job_id, &request.artifact_id)?;
    let staging_dir = artifact_dir.join(".staging");
    fs::create_dir_all(&staging_dir)
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    let guard = StagingCleanupGuard {
        dir: staging_dir.clone(),
        disarm: false,
    };
    let result = run_incremental_pipeline(
        catalog,
        plan,
        request,
        &encoder_name,
        &artifact_dir,
        &staging_dir,
        deadline,
        cancel,
        &mut check_headroom,
        &mut on_progress,
    );
    match result {
        Ok(encoded) => {
            let mut guard = guard;
            guard.disarm = true;
            let _ = fs::remove_dir_all(&staging_dir);
            Ok(encoded)
        }
        Err(error) => {
            drop(guard);
            let _ = fs::remove_dir_all(&artifact_dir);
            Err(error)
        }
    }
}

struct StagingCleanupGuard {
    dir: PathBuf,
    disarm: bool,
}

impl Drop for StagingCleanupGuard {
    fn drop(&mut self) {
        if !self.disarm {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn validate_encode_admission(
    plan: &TimelapsePlan,
    request: &TimelapseEncodeRequest,
    check_headroom: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), TimelapseEncodeError> {
    validate_output_settings(&TimelapseOutputPreview {
        output_fps: request.output_fps,
        max_width: request.output_width,
        max_height: request.output_height,
        timestamp_overlay: request.timestamp_overlay,
    })
    .map_err(|error| TimelapseEncodeError::InvalidRequest(error.to_string()))?;
    if request.max_output_bytes == 0 || request.max_output_bytes > MAX_OUTPUT_BYTES {
        return Err(TimelapseEncodeError::InvalidRequest(
            "max_output_bytes must be positive and within configured cap".to_owned(),
        ));
    }
    if plan.estimate.sample_count == 0 {
        return Err(TimelapseEncodeError::PartialResultRequiresAllowance {
            gap_count: plan.estimate.gap_count.max(1),
        });
    }
    if plan.has_gaps() && !request.allow_partial {
        return Err(TimelapseEncodeError::PartialResultRequiresAllowance {
            gap_count: plan.estimate.gap_count,
        });
    }
    if plan.estimate.requires_confirmation && !request.confirmed_expensive {
        return Err(TimelapseEncodeError::ConfirmationRequired);
    }
    validate_safe_component(&request.job_id, "job_id")?;
    validate_safe_component(&request.artifact_id, "artifact_id")?;
    validate_safe_component(&request.file_name, "file_name")?;
    validate_safe_component(&request.manifest_file_name, "manifest_file_name")?;
    check_headroom().map_err(TimelapseEncodeError::DiskPressure)?;
    Ok(())
}

fn validate_safe_component(value: &str, label: &str) -> Result<(), TimelapseEncodeError> {
    if value.is_empty()
        || value.len() > 160
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || value.chars().any(char::is_control)
    {
        return Err(TimelapseEncodeError::InvalidRequest(format!(
            "unsafe {label} path component"
        )));
    }
    Ok(())
}

fn validated_artifact_dir(
    export_root: &Path,
    job_id: &str,
    artifact_id: &str,
) -> Result<PathBuf, TimelapseEncodeError> {
    fs::create_dir_all(export_root)
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    let canonical_root = fs::canonicalize(export_root)
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    let target = canonical_root.join(job_id).join(artifact_id);
    if !target.starts_with(&canonical_root) {
        return Err(TimelapseEncodeError::InvalidRequest(
            "artifact directory escapes export root".to_owned(),
        ));
    }
    Ok(target)
}

#[allow(clippy::too_many_arguments)]
fn run_incremental_pipeline(
    catalog: &RecordingCatalogHandle,
    plan: &TimelapsePlan,
    request: &TimelapseEncodeRequest,
    encoder_name: &str,
    artifact_dir: &Path,
    staging_dir: &Path,
    deadline: Instant,
    cancel: &AtomicBool,
    check_headroom: &mut impl FnMut() -> Result<(), String>,
    on_progress: &mut impl FnMut(u32, u32),
) -> Result<TimelapseEncodeResult, TimelapseEncodeError> {
    let temp_mp4 = staging_dir.join("output.tmp.mp4");
    let mut encoder = spawn_silent_encoder(request, encoder_name, &temp_mp4)?;
    let stderr_drain = spawn_stderr_drain(encoder.stderr.take());
    let mut stdin = encoder
        .stdin
        .take()
        .ok_or_else(|| TimelapseEncodeError::EncodeFailed("missing encoder stdin".to_owned()))?;
    let total = plan.estimate.sample_count;
    let mut encoded = 0_u32;
    let stream_res = (|| -> Result<(), TimelapseEncodeError> {
        for sample in plan.samples() {
            check_runtime_guards(deadline, cancel, check_headroom)?;
            let mut rgb = decode_sample_rgb24(
                catalog,
                sample,
                request.output_width,
                request.output_height,
                &request.ffmpeg_binary,
                deadline,
                cancel,
            )?;
            if request.timestamp_overlay {
                let text =
                    overlay::format_site_timestamp(sample.actual_time_ms, &plan.site_timezone)
                        .map_err(|error| TimelapseEncodeError::InvalidRequest(error.to_string()))?;
                overlay::draw_timestamp_overlay(
                    &mut rgb,
                    request.output_width,
                    request.output_height,
                    &text,
                );
            }
            stdin
                .write_all(&rgb)
                .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
            drop(rgb);
            encoded = encoded.saturating_add(1);
            on_progress(encoded, total);
            check_staging_size(&temp_mp4, request.max_output_bytes)?;
        }
        stdin
            .flush()
            .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
        Ok(())
    })();
    drop(stdin);
    if let Err(error) = stream_res {
        kill_and_reap(&mut encoder);
        let _ = stderr_drain.join();
        return Err(error);
    }
    wait_child_bounded(&mut encoder, stderr_drain, deadline, cancel, "encoder")?;
    publish_artifacts(plan, request, artifact_dir, staging_dir, &temp_mp4, encoded)
}

fn spawn_silent_encoder(
    request: &TimelapseEncodeRequest,
    encoder_name: &str,
    output_path: &Path,
) -> Result<Child, TimelapseEncodeError> {
    let size = format!("{}x{}", request.output_width, request.output_height);
    let fps = request.output_fps.to_string();
    let mut command = Command::new(&request.ffmpeg_binary);
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-s",
        &size,
        "-r",
        &fps,
        "-i",
        "pipe:0",
        "-an",
        "-c:v",
        encoder_name,
        "-pix_fmt",
        "yuv420p",
    ]);
    if encoder_name == "libx264" {
        command.args(["-preset", "veryfast", "-crf", "23"]);
    }
    command
        .args(["-movflags", "+faststart"])
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
        .spawn()
        .map_err(|error| TimelapseEncodeError::EncoderUnavailable(error.to_string()))
}

fn publish_artifacts(
    plan: &TimelapsePlan,
    request: &TimelapseEncodeRequest,
    artifact_dir: &Path,
    staging_dir: &Path,
    temp_mp4: &Path,
    encoded_frames: u32,
) -> Result<TimelapseEncodeResult, TimelapseEncodeError> {
    let (video_bytes, video_sha256) = stream_sha256_and_size(temp_mp4, request.max_output_bytes)?;
    let manifest = TimelapseManifest::from_plan(
        plan,
        ManifestOutputMetadata {
            source_identity: request.source_identity.clone(),
            output_file_name: &request.file_name,
            output_sha256: &video_sha256,
            output_bytes: video_bytes,
            output_width: request.output_width,
            output_height: request.output_height,
            output_fps: request.output_fps,
            timestamp_overlay: request.timestamp_overlay,
            allow_partial: request.allow_partial,
        },
    );
    let (manifest_bytes_vec, manifest_sha256) = manifest
        .to_json_and_sha256()
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    let manifest_bytes = u64::try_from(manifest_bytes_vec.len()).unwrap_or(0);
    let temp_manifest = staging_dir.join("manifest.tmp.json");
    fs::write(&temp_manifest, &manifest_bytes_vec)
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    let video_path = artifact_dir.join(&request.file_name);
    let manifest_path = artifact_dir.join(&request.manifest_file_name);
    fs::rename(temp_mp4, &video_path)
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    fs::rename(&temp_manifest, &manifest_path)
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    Ok(TimelapseEncodeResult {
        video_path,
        manifest_path,
        video_sha256,
        video_bytes,
        manifest_sha256,
        manifest_bytes,
        encoded_frames,
        manifest,
    })
}

fn check_runtime_guards(
    deadline: Instant,
    cancel: &AtomicBool,
    check_headroom: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), TimelapseEncodeError> {
    if cancel.load(Ordering::Acquire) {
        return Err(TimelapseEncodeError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(TimelapseEncodeError::Timeout);
    }
    check_headroom().map_err(TimelapseEncodeError::DiskPressure)
}

fn check_staging_size(path: &Path, limit_bytes: u64) -> Result<(), TimelapseEncodeError> {
    if let Ok(meta) = fs::metadata(path)
        && meta.len() > limit_bytes
    {
        return Err(TimelapseEncodeError::OutputSizeExceeded { limit_bytes });
    }
    Ok(())
}

fn stream_sha256_and_size(
    path: &Path,
    limit_bytes: u64,
) -> Result<(u64, String), TimelapseEncodeError> {
    let mut file =
        File::open(path).map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK_BYTES];
    let mut total = 0_u64;
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > limit_bytes {
            return Err(TimelapseEncodeError::OutputSizeExceeded { limit_bytes });
        }
        hasher.update(&buf[..read]);
    }
    Ok((total, encode_lower_hex(&hasher.finalize())))
}

fn decode_sample_rgb24(
    catalog: &RecordingCatalogHandle,
    sample: &TimelapseSample,
    output_width: u32,
    output_height: u32,
    ffmpeg_binary: &str,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, TimelapseEncodeError> {
    let interval_start_ms = sample.fragment.start_ms;
    let interval_end_ms = sample
        .fragment
        .start_ms
        .saturating_add(i64::try_from(sample.fragment.duration_ms).unwrap_or(0));
    let window = read_sample_decode_window(catalog, sample, deadline, cancel).map_err(|error| {
        TimelapseEncodeError::DecodeFailed {
            interval_start_ms,
            interval_end_ms,
            detail: error.to_string(),
        }
    })?;
    let (input_format, annexb) =
        build_annexb_stream(&window).map_err(|error| TimelapseEncodeError::DecodeFailed {
            interval_start_ms,
            interval_end_ms,
            detail: error.to_string(),
        })?;
    run_single_sample_decode(
        ffmpeg_binary,
        input_format,
        annexb,
        window.target_presentation_rank,
        output_width,
        output_height,
        interval_start_ms,
        interval_end_ms,
        deadline,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_single_sample_decode(
    ffmpeg_binary: &str,
    input_format: &str,
    annexb: Vec<u8>,
    target_rank: u32,
    output_width: u32,
    output_height: u32,
    interval_start_ms: i64,
    interval_end_ms: i64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, TimelapseEncodeError> {
    let mut child = spawn_sample_decoder(ffmpeg_binary, input_format, output_width, output_height)?;
    let stderr_drain = spawn_stderr_drain(child.stderr.take());
    let mut stdin = child.stdin.take();
    let writer = thread::spawn(move || {
        if let Some(mut pipe) = stdin.take() {
            let _ = pipe.write_all(&annexb);
        }
    });
    let frame_bytes = (output_width as usize)
        .saturating_mul(output_height as usize)
        .saturating_mul(3);
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| TimelapseEncodeError::DecodeFailed {
            interval_start_ms,
            interval_end_ms,
            detail: "missing decoder stdout".to_owned(),
        })?;
    let selected = read_target_rgb_frame(
        &mut stdout,
        &mut child,
        frame_bytes,
        target_rank,
        deadline,
        cancel,
    );
    drop(stdout);
    let _ = writer.join();
    if let Ok(Some(frame)) = selected {
        kill_and_reap(&mut child);
        let _ = stderr_drain.join();
        return Ok(frame);
    }
    if let Err(error) = selected {
        kill_and_reap(&mut child);
        let _ = stderr_drain.join();
        return Err(error);
    }
    wait_child_bounded(&mut child, stderr_drain, deadline, cancel, "decoder").map_err(|error| {
        match error {
            TimelapseEncodeError::Cancelled | TimelapseEncodeError::Timeout => error,
            other => TimelapseEncodeError::DecodeFailed {
                interval_start_ms,
                interval_end_ms,
                detail: other.to_string(),
            },
        }
    })?;
    Err(TimelapseEncodeError::DecodeFailed {
        interval_start_ms,
        interval_end_ms,
        detail: "decoder produced fewer frames than target presentation rank".to_owned(),
    })
}

fn spawn_sample_decoder(
    ffmpeg_binary: &str,
    input_format: &str,
    output_width: u32,
    output_height: u32,
) -> Result<Child, TimelapseEncodeError> {
    let filter = format!(
        "scale={output_width}:{output_height}:force_original_aspect_ratio=decrease,pad={output_width}:{output_height}:(ow-iw)/2:(oh-ih)/2"
    );
    Command::new(ffmpeg_binary)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            input_format,
            "-i",
            "pipe:0",
            "-vf",
            &filter,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| TimelapseEncodeError::EncoderUnavailable(error.to_string()))
}

fn read_target_rgb_frame(
    stdout: &mut std::process::ChildStdout,
    child: &mut Child,
    frame_bytes: usize,
    target_rank: u32,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Option<Vec<u8>>, TimelapseEncodeError> {
    let mut frame = vec![0u8; frame_bytes];
    for index in 0..=target_rank {
        if cancel.load(Ordering::Acquire) {
            kill_and_reap(child);
            return Err(TimelapseEncodeError::Cancelled);
        }
        if Instant::now() >= deadline {
            kill_and_reap(child);
            return Err(TimelapseEncodeError::Timeout);
        }
        if stdout.read_exact(&mut frame).is_err() {
            return Ok(None);
        }
        if index == target_rank {
            return Ok(Some(frame));
        }
    }
    Ok(None)
}

fn build_annexb_stream(window: &SampleDecodeWindow) -> anyhow::Result<(&'static str, Vec<u8>)> {
    let codec = window.decoder.codec.as_str();
    let (format, is_hevc) = if codec.starts_with("avc1") || codec.starts_with("avc3") {
        ("h264", false)
    } else if codec.starts_with("hvc1") || codec.starts_with("hev1") {
        ("hevc", true)
    } else {
        anyhow::bail!("unsupported video codec {codec}");
    };
    let mut out = Vec::with_capacity(window.encoded_bytes as usize + 512);
    if is_hevc {
        append_hvcc_parameter_sets(&window.decoder.description, &mut out)?;
    } else {
        append_avcc_parameter_sets(&window.decoder.description, &mut out)?;
    }
    let length_size = usize::from(window.decoder.nal_length_size);
    anyhow::ensure!((1..=4).contains(&length_size), "invalid NAL length size");
    for sample in &window.samples {
        append_length_prefixed_nalus(&sample.payload, length_size, &mut out)?;
    }
    Ok((format, out))
}

fn append_avcc_parameter_sets(desc: &[u8], out: &mut Vec<u8>) -> anyhow::Result<()> {
    anyhow::ensure!(desc.len() >= 7, "truncated avcC configuration");
    let mut pos = 5usize;
    let num_sps = usize::from(desc[pos] & 0x1F);
    pos += 1;
    for _ in 0..num_sps {
        anyhow::ensure!(pos + 2 <= desc.len(), "truncated avcC SPS length");
        let len = usize::from(u16::from_be_bytes([desc[pos], desc[pos + 1]]));
        pos += 2;
        anyhow::ensure!(pos + len <= desc.len(), "truncated avcC SPS payload");
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&desc[pos..pos + len]);
        pos += len;
    }
    anyhow::ensure!(pos < desc.len(), "truncated avcC PPS count");
    let num_pps = usize::from(desc[pos]);
    pos += 1;
    for _ in 0..num_pps {
        anyhow::ensure!(pos + 2 <= desc.len(), "truncated avcC PPS length");
        let len = usize::from(u16::from_be_bytes([desc[pos], desc[pos + 1]]));
        pos += 2;
        anyhow::ensure!(pos + len <= desc.len(), "truncated avcC PPS payload");
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&desc[pos..pos + len]);
        pos += len;
    }
    Ok(())
}

fn append_hvcc_parameter_sets(desc: &[u8], out: &mut Vec<u8>) -> anyhow::Result<()> {
    anyhow::ensure!(desc.len() >= 23, "truncated hvcC configuration");
    let num_arrays = usize::from(desc[22]);
    let mut pos = 23usize;
    for _ in 0..num_arrays {
        anyhow::ensure!(pos + 3 <= desc.len(), "truncated hvcC array header");
        pos += 1;
        let num_nalus = usize::from(u16::from_be_bytes([desc[pos], desc[pos + 1]]));
        pos += 2;
        for _ in 0..num_nalus {
            anyhow::ensure!(pos + 2 <= desc.len(), "truncated hvcC NALU length");
            let len = usize::from(u16::from_be_bytes([desc[pos], desc[pos + 1]]));
            pos += 2;
            anyhow::ensure!(pos + len <= desc.len(), "truncated hvcC NALU payload");
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&desc[pos..pos + len]);
            pos += len;
        }
    }
    Ok(())
}

fn append_length_prefixed_nalus(
    payload: &[u8],
    length_size: usize,
    out: &mut Vec<u8>,
) -> anyhow::Result<()> {
    let mut pos = 0usize;
    while pos + length_size <= payload.len() {
        let mut nal_len = 0usize;
        for &byte in &payload[pos..pos + length_size] {
            nal_len = (nal_len << 8) | usize::from(byte);
        }
        pos += length_size;
        anyhow::ensure!(pos + nal_len <= payload.len(), "truncated NALU in sample");
        if nal_len > 0 {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&payload[pos..pos + nal_len]);
        }
        pos += nal_len;
    }
    Ok(())
}

const MAX_STDERR_CAPTURE_BYTES: usize = 4 * 1_024;

/// Continuously drains child stderr so diagnostic output cannot stall the pipe buffer.
fn spawn_stderr_drain(stderr: Option<std::process::ChildStderr>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let Some(mut pipe) = stderr else {
            return String::new();
        };
        let mut captured = Vec::new();
        let mut chunk = [0u8; 4_096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if captured.len() < MAX_STDERR_CAPTURE_BYTES {
                        let keep = n.min(MAX_STDERR_CAPTURE_BYTES - captured.len());
                        captured.extend_from_slice(&chunk[..keep]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&captured).into_owned()
    })
}

fn wait_child_bounded(
    child: &mut Child,
    stderr_drain: thread::JoinHandle<String>,
    deadline: Instant,
    cancel: &AtomicBool,
    role: &str,
) -> Result<(), TimelapseEncodeError> {
    loop {
        if cancel.load(Ordering::Acquire) {
            kill_and_reap(child);
            let _ = stderr_drain.join();
            return Err(TimelapseEncodeError::Cancelled);
        }
        if Instant::now() >= deadline {
            kill_and_reap(child);
            let _ = stderr_drain.join();
            return Err(TimelapseEncodeError::Timeout);
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let _ = stderr_drain.join();
                return Ok(());
            }
            Ok(Some(status)) => {
                let stderr = stderr_drain.join().unwrap_or_default();
                return Err(TimelapseEncodeError::EncodeFailed(format!(
                    "{role} exited with {status}: {}",
                    stderr.trim()
                )));
            }
            Ok(None) => thread::sleep(CHILD_POLL_INTERVAL),
            Err(error) => {
                kill_and_reap(child);
                let _ = stderr_drain.join();
                return Err(TimelapseEncodeError::EncodeFailed(error.to_string()));
            }
        }
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_drain_prevents_pipe_deadlock_above_os_buffer() {
        let mut child = if cfg!(windows) {
            Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    "$line = 'x' * 1024; 1..80 | ForEach-Object { [Console]::Error.WriteLine($line) }",
                ])
                .stderr(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .expect("spawn noisy stderr powershell child")
        } else {
            Command::new("sh")
                .args([
                    "-c",
                    "i=0; while [ $i -lt 80 ]; do printf '%01024d\\n' $i >&2; i=$((i+1)); done",
                ])
                .stderr(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .expect("spawn noisy stderr shell child")
        };
        let drain = spawn_stderr_drain(child.stderr.take());
        let cancel = AtomicBool::new(false);
        wait_child_bounded(
            &mut child,
            drain,
            Instant::now() + Duration::from_secs(15),
            &cancel,
            "noisy-stderr-test",
        )
        .expect("child writing >80 KiB to stderr must exit without deadlocking");
    }
}
