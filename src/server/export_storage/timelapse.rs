//! Silent timelapse export admission, worker execution, and durable publication.

use super::*;
use crate::storage::{
    timelapse::{
        DEFAULT_TOLERANCE_MS, MANIFEST_SCHEMA_VERSION, MAX_CONCURRENT_JOBS_GLOBAL,
        MAX_CONCURRENT_JOBS_PER_REQUESTER, MAX_OUTPUT_BYTES, TimelapseCadence,
        TimelapseOutputPreview, TimelapsePlan, TimelapsePlanItem, TimelapsePlanRequest,
        plan_timelapse,
    },
    timelapse_encoder::{TimelapseEncodeError, TimelapseEncodeRequest, encode_timelapse_mp4},
};

const TIMELAPSE_EXPORT_DEADLINE: Duration = Duration::from_secs(30 * 60);

pub(super) struct PreparedTimelapse {
    pub(super) plan: TimelapsePlan,
    pub(super) options: proto::TimelapseExportOptions,
    pub(super) job: proto::ExportJob,
    pub(super) file_name: String,
    pub(super) manifest_file_name: String,
    pub(super) artifact_id: String,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) recording_stream_id: String,
}

pub(super) fn prepare_timelapse_export(
    state: &ServerState,
    requester: &str,
    request: &proto::CreateExportJob,
    camera: &CameraEntry,
    options: &proto::TimelapseExportOptions,
) -> Result<PreparedTimelapse, ControlCommandError> {
    validate_timelapse_create_request(state, requester, request)?;
    let start_ms = required_timestamp_ms(request.start_time.as_ref(), "timelapse start time")?;
    let end_ms = required_timestamp_ms(request.end_time.as_ref(), "timelapse end time")?;
    let catalog = state.catalog.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        )
    })?;
    let plan_request =
        build_export_plan_request(state, request, camera, options, start_ms, end_ms)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let plan = plan_timelapse(
        catalog,
        &plan_request,
        Instant::now() + Duration::from_secs(20),
        &cancel,
    )
    .map_err(|error| {
        ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string())
    })?;
    validate_timelapse_plan_guards(state, &plan, options)?;
    let (file_name, manifest_file_name) = timelapse_artifact_names(camera, start_ms, end_ms);
    let job = build_initial_timelapse_job(request, options, &plan, start_ms, end_ms);
    Ok(PreparedTimelapse {
        recording_stream_id: plan_request.catalog_stream_id,
        plan,
        options: options.clone(),
        job,
        file_name,
        manifest_file_name,
        artifact_id: Uuid::new_v4().simple().to_string(),
        cancel,
    })
}

fn validate_timelapse_create_request(
    state: &ServerState,
    requester: &str,
    request: &proto::CreateExportJob,
) -> Result<(), ControlCommandError> {
    if request.guest_range_preparation == Some(true) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "timelapse exports cannot be prepared as guest evidence ranges",
        ));
    }
    if request.burn_in_timestamp {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "use timelapse.timestamp_overlay instead of evidence burn_in_timestamp",
        ));
    }
    if !state.timelapse.encoder_available() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "local silent timelapse encoder worker is unavailable",
        ));
    }
    enforce_timelapse_concurrency(state, requester)
}

fn validate_timelapse_plan_guards(
    state: &ServerState,
    plan: &TimelapsePlan,
    options: &proto::TimelapseExportOptions,
) -> Result<(), ControlCommandError> {
    if let Some(expected) = options.expected_source_fingerprint.as_deref()
        && !expected.is_empty()
        && expected != plan.source_fingerprint
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "recorded media changed since the timelapse preview was created",
        ));
    }
    if plan.estimate.requires_confirmation && !options.confirmed_expensive {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "expensive timelapse export requires explicit confirmation",
        ));
    }
    check_export_headroom(state)
        .map_err(|detail| ControlCommandError::new(proto::ErrorCode::Unavailable, 503, detail))
}

fn timelapse_artifact_names(camera: &CameraEntry, start_ms: i64, end_ms: i64) -> (String, String) {
    let label = camera
        .info
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&camera.recording_label);
    let base_name = export_file_name(label, start_ms, end_ms);
    let file_name = base_name.replace(".mp4", "-timelapse.mp4");
    let manifest_file_name = file_name.replace(".mp4", ".manifest.json");
    (file_name, manifest_file_name)
}

/// Derives the deterministic managed volume object ID for a timelapse provenance manifest.
pub(in crate::server) fn manifest_object_id(artifact_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"keeppeek.timelapse.manifest.v1\0");
    hasher.update(artifact_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes).simple().to_string()
}

pub(super) fn spawn_timelapse_worker(
    state: ServerState,
    request: proto::CreateExportJob,
    prepared: PreparedTimelapse,
) {
    let job_id = request.job_id.clone();
    let artifact_id = prepared.artifact_id.clone();
    let cancel = Arc::clone(&prepared.cancel);
    let outputs = match prepare_timelapse_outputs(&state, &request, &prepared) {
        Ok(outputs) => outputs,
        Err(error) => {
            fail_export_job(&state, &job_id, &cancel, error.to_string());
            let _ = cleanup_export_attempt_artifacts(&state, &job_id, &artifact_id);
            return;
        }
    };
    let state_for_thread = state.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(format!("timelapse-export-{job_id}"))
        .spawn(move || run_timelapse_worker(state_for_thread, request, prepared, outputs))
    {
        tracing::error!(%error, %job_id, "failed to spawn timelapse export worker");
        fail_export_job(
            &state,
            &job_id,
            &cancel,
            format!("unable to start timelapse export worker: {error}"),
        );
        let _ = cleanup_export_attempt_artifacts(&state, &job_id, &artifact_id);
    }
}

struct PreparedOutputs {
    video: super::Output,
    manifest: Option<super::Output>,
}

fn prepare_timelapse_outputs(
    state: &ServerState,
    request: &proto::CreateExportJob,
    prepared: &PreparedTimelapse,
) -> anyhow::Result<PreparedOutputs> {
    let video = super::prepare(state, request, &prepared.artifact_id, &prepared.file_name)?;
    let manifest = if video.reservation.is_some() {
        let manifest_id = manifest_object_id(&prepared.artifact_id);
        let reserved = super::prepare(state, request, &manifest_id, &prepared.manifest_file_name)?;
        anyhow::ensure!(
            reserved.reservation.is_some(),
            "export volume reservation was unavailable for timelapse manifest"
        );
        Some(reserved)
    } else {
        None
    };
    Ok(PreparedOutputs { video, manifest })
}

fn run_timelapse_worker(
    state: ServerState,
    request: proto::CreateExportJob,
    prepared: PreparedTimelapse,
    outputs: PreparedOutputs,
) {
    let Some(catalog) = state.catalog.clone() else {
        fail_export_job(
            &state,
            &request.job_id,
            &prepared.cancel,
            "recording catalog is unavailable".to_owned(),
        );
        let _ = cleanup_export_attempt_artifacts(&state, &request.job_id, &prepared.artifact_id);
        return;
    };
    let managed = outputs.video.reservation.is_some();
    let export_root = if managed {
        state
            .storage_config
            .long_term_path
            .join(".exports")
            .join(".timelapse-staging")
    } else {
        state.storage_config.long_term_path.join(".exports")
    };
    let encode_req = build_encode_request(&state, &request, &prepared, export_root.clone());
    let deadline = Instant::now() + TIMELAPSE_EXPORT_DEADLINE;
    let state_for_headroom = state.clone();
    let state_for_progress = state.clone();
    let job_id_for_progress = request.job_id.clone();
    let result = encode_timelapse_mp4(
        &catalog,
        &prepared.plan,
        &encode_req,
        deadline,
        &prepared.cancel,
        || check_export_headroom(&state_for_headroom),
        |done, total| {
            update_timelapse_progress(&state_for_progress, &job_id_for_progress, done, total);
        },
    );
    let outcome = match result {
        Ok(encoded) => publish_if_managed(outputs, encoded, &prepared.cancel),
        Err(error) => Err(error),
    };
    if managed {
        let _ =
            cleanup_export_attempt_directory(&export_root, &request.job_id, &prepared.artifact_id);
    }
    match outcome {
        Ok((encoded, stored_path)) => {
            complete_timelapse_job(&state, &request.job_id, &prepared, encoded, stored_path);
        }
        Err(TimelapseEncodeError::Cancelled) => {
            mark_timelapse_cancelled(&state, &request.job_id, &prepared.artifact_id);
        }
        Err(error) => {
            let _ =
                cleanup_export_attempt_artifacts(&state, &request.job_id, &prepared.artifact_id);
            fail_export_job(&state, &request.job_id, &prepared.cancel, error.to_string());
        }
    }
}

fn build_encode_request(
    state: &ServerState,
    request: &proto::CreateExportJob,
    prepared: &PreparedTimelapse,
    export_root: PathBuf,
) -> TimelapseEncodeRequest {
    let source_identity =
        super::provenance::source_identity(state, &request.source_id).map(|id| id.to_string());
    TimelapseEncodeRequest {
        export_root,
        job_id: request.job_id.clone(),
        artifact_id: prepared.artifact_id.clone(),
        file_name: prepared.file_name.clone(),
        manifest_file_name: prepared.manifest_file_name.clone(),
        source_identity,
        output_width: prepared.options.max_width,
        output_height: prepared.options.max_height,
        output_fps: prepared.options.output_fps,
        timestamp_overlay: prepared.options.timestamp_overlay,
        allow_partial: request.allow_partial,
        confirmed_expensive: prepared.options.confirmed_expensive,
        max_output_bytes: MAX_OUTPUT_BYTES,
        ffmpeg_binary: "ffmpeg".to_owned(),
    }
}

fn publish_if_managed(
    outputs: PreparedOutputs,
    encoded: crate::storage::timelapse_encoder::TimelapseEncodeResult,
    cancel: &AtomicBool,
) -> Result<
    (
        crate::storage::timelapse_encoder::TimelapseEncodeResult,
        Option<PathBuf>,
    ),
    TimelapseEncodeError,
> {
    let Some(video_res) = outputs.video.reservation else {
        let stored_path = Some(encoded.video_path.clone());
        return Ok((encoded, stored_path));
    };
    let manifest_res = outputs
        .manifest
        .and_then(|out| out.reservation)
        .ok_or_else(|| {
            TimelapseEncodeError::EncodeFailed("missing managed manifest reservation".to_owned())
        })?;
    publish_file_to_reservation(&encoded.video_path, video_res, cancel)?;
    publish_file_to_reservation(&encoded.manifest_path, manifest_res, cancel)?;
    Ok((encoded, None))
}

fn publish_file_to_reservation(
    source_path: &Path,
    reservation: crate::storage::volumes::runtime::Reservation,
    cancel: &AtomicBool,
) -> Result<(), TimelapseEncodeError> {
    if cancel.load(Ordering::Acquire) {
        return Err(TimelapseEncodeError::Cancelled);
    }
    let mut source = File::open(source_path)
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    let mut target = reservation
        .open()
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    std::io::copy(&mut source, &mut target)
        .map_err(|error| TimelapseEncodeError::EncodeFailed(error.to_string()))?;
    let evidence = target
        .evidence_with_progress(|_| {
            anyhow::ensure!(
                !cancel.load(Ordering::Acquire),
                "timelapse export cancelled"
            );
            Ok(())
        })
        .map_err(|error| {
            if cancel.load(Ordering::Acquire) {
                TimelapseEncodeError::Cancelled
            } else {
                TimelapseEncodeError::EncodeFailed(error.to_string())
            }
        })?;
    super::confirm_publication(
        || target.publish(evidence.clone()),
        || cancel.load(Ordering::Acquire),
    )
    .map_err(|error| {
        if cancel.load(Ordering::Acquire) {
            TimelapseEncodeError::Cancelled
        } else {
            TimelapseEncodeError::EncodeFailed(error.to_string())
        }
    })
}

fn update_timelapse_progress(state: &ServerState, job_id: &str, done: u32, total: u32) {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(record) = jobs.get_mut(job_id)
        && record.job.status == proto::ExportJobStatus::Running as i32
    {
        let ratio = if total == 0 {
            900
        } else {
            100 + ((u64::from(done) * 850) / u64::from(total)) as u32
        };
        record.job.progress_per_mille = ratio.min(950);
        record.updated_at_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    }
}

fn complete_timelapse_job(
    state: &ServerState,
    job_id: &str,
    prepared: &PreparedTimelapse,
    encoded: crate::storage::timelapse_encoder::TimelapseEncodeResult,
    stored_path: Option<PathBuf>,
) {
    if prepared.cancel.load(Ordering::Acquire) {
        let _ = cleanup_export_attempt_artifacts(state, job_id, &prepared.artifact_id);
        mark_timelapse_cancelled(state, job_id, &prepared.artifact_id);
        return;
    }
    let now = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let expires_ms =
        now.saturating_add(i64::try_from(EXPORT_JOB_EXPIRY.as_millis()).unwrap_or(i64::MAX));
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(record) = jobs.get_mut(job_id) else {
        drop(jobs);
        let _ = cleanup_export_attempt_artifacts(state, job_id, &prepared.artifact_id);
        return;
    };
    if record.artifact_id != prepared.artifact_id
        || record.job.status != proto::ExportJobStatus::Running as i32
    {
        drop(jobs);
        let _ = cleanup_export_attempt_artifacts(state, job_id, &prepared.artifact_id);
        return;
    }
    record.job.status = proto::ExportJobStatus::Ready as i32;
    record.job.progress_per_mille = 1_000;
    record.job.bytes_written = encoded.video_bytes;
    record.job.file_name = Some(prepared.file_name.clone());
    record.job.sha256 = Some(encoded.video_sha256);
    record.job.expires_at = Some(millis_timestamp(expires_ms));
    record.job.error = None;
    record.job.retryable = false;
    if let Some(summary) = record.job.timelapse.as_mut() {
        summary.sample_count = encoded.encoded_frames;
        summary.output_duration_ms = encoded.manifest.output_duration_ms;
        summary.manifest_file_name = Some(prepared.manifest_file_name.clone());
        summary.manifest_sha256 = Some(encoded.manifest_sha256);
        summary.manifest_bytes = Some(encoded.manifest_bytes);
    }
    record.path = stored_path;
    record.updated_at_ms = now;
    record.completed_at_ms = Some(now);
    persist_export_jobs_logged(state, &jobs, "timelapse ready");
}

fn mark_timelapse_cancelled(state: &ServerState, job_id: &str, artifact_id: &str) {
    let _ = cleanup_export_attempt_artifacts(state, job_id, artifact_id);
    let now = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(record) = jobs.get_mut(job_id)
        && record.artifact_id == artifact_id
    {
        record.job.status = proto::ExportJobStatus::Cancelled as i32;
        record.job.retryable = true;
        record.job.error = None;
        record.path = None;
        record.updated_at_ms = now;
        record.completed_at_ms = Some(now);
        persist_export_jobs_logged(state, &jobs, "timelapse cancelled");
    }
}

fn enforce_timelapse_concurrency(
    state: &ServerState,
    requester: &str,
) -> Result<(), ControlCommandError> {
    let jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut global_running = 0usize;
    let mut requester_running = 0usize;
    for record in jobs.values() {
        if record.request.timelapse.is_some()
            && record.job.status == proto::ExportJobStatus::Running as i32
        {
            global_running += 1;
            if record.requester_id == requester {
                requester_running += 1;
            }
        }
    }
    if requester_running >= MAX_CONCURRENT_JOBS_PER_REQUESTER
        || global_running >= MAX_CONCURRENT_JOBS_GLOBAL
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "another timelapse export job is already running",
        ));
    }
    Ok(())
}

fn check_export_headroom(state: &ServerState) -> Result<(), String> {
    let policy = state.storage_config.safety_policy();
    let critical = policy.critical_free_bytes.max(policy.minimum_free_bytes);
    if critical == 0 {
        return Ok(());
    }
    let root = &state.storage_config.long_term_path;
    let Ok(capacity) = crate::storage::safety::filesystem_capacity(root, 0) else {
        return Ok(());
    };
    let eval = policy.evaluate(capacity);
    if eval.pressure == crate::storage::safety::StoragePressure::Critical {
        return Err("storage headroom is below the configured safety reserve".to_owned());
    }
    Ok(())
}

fn build_export_plan_request(
    state: &ServerState,
    request: &proto::CreateExportJob,
    camera: &CameraEntry,
    options: &proto::TimelapseExportOptions,
    start_ms: i64,
    end_ms: i64,
) -> Result<TimelapsePlanRequest, ControlCommandError> {
    let cadence = match &options.cadence {
        Some(proto::timelapse_export_options::Cadence::Interval(duration)) => {
            TimelapseCadence::IntervalMs(super::super::timelapse::duration_to_positive_ms(
                duration,
                "timelapse interval",
            )?)
        }
        Some(proto::timelapse_export_options::Cadence::TargetSampleCount(count)) => {
            TimelapseCadence::TargetCount(*count)
        }
        None => TimelapseCadence::TargetCount(60),
    };
    let tolerance_ms = match options.tolerance.as_ref() {
        Some(duration) => i64::try_from(super::super::timelapse::duration_to_nonnegative_ms(
            duration,
            "timelapse tolerance",
        )?)
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "timelapse tolerance overflow",
            )
        })?,
        None => DEFAULT_TOLERANCE_MS,
    };
    Ok(TimelapsePlanRequest {
        source_id: request.source_id.clone(),
        stream_id: request.stream_id.clone(),
        catalog_stream_id: recording_stream_id(camera, &request.stream_id),
        start_ms,
        end_ms,
        cadence,
        tolerance_ms,
        include_open_recordings: false,
        privacy_intervals: Vec::new(),
        output_preview: TimelapseOutputPreview {
            output_fps: options.output_fps,
            max_width: options.max_width,
            max_height: options.max_height,
            timestamp_overlay: options.timestamp_overlay,
        },
        site_timezone: super::super::timelapse::camera_site_timezone(state, &camera.info.id),
        encoder_available: state.timelapse.encoder_available(),
    })
}

fn build_initial_timelapse_job(
    request: &proto::CreateExportJob,
    options: &proto::TimelapseExportOptions,
    plan: &TimelapsePlan,
    start_ms: i64,
    end_ms: i64,
) -> proto::ExportJob {
    let missing_ranges: Vec<proto::ExportMissingRange> = plan
        .items
        .iter()
        .filter_map(|item| match item {
            TimelapsePlanItem::Gap(gap) => Some(proto::ExportMissingRange {
                start_time: Some(millis_timestamp(gap.start_ms)),
                end_time: Some(millis_timestamp(gap.end_ms)),
            }),
            TimelapsePlanItem::Sample(_) => None,
        })
        .collect();
    let status = if plan.estimate.sample_count == 0 || (plan.has_gaps() && !request.allow_partial) {
        proto::ExportJobStatus::Partial
    } else {
        proto::ExportJobStatus::Running
    };
    proto::ExportJob {
        job_id: request.job_id.clone(),
        source_id: request.source_id.clone(),
        stream_id: request.stream_id.clone(),
        requested_start_time: Some(millis_timestamp(start_ms)),
        requested_end_time: Some(millis_timestamp(end_ms)),
        aligned_start_time: plan
            .samples()
            .next()
            .map(|sample| millis_timestamp(sample.actual_time_ms)),
        status: status as i32,
        progress_per_mille: if status == proto::ExportJobStatus::Running {
            100
        } else {
            0
        },
        bytes_written: 0,
        estimated_bytes: Some(plan.estimate.estimated_output_bytes),
        file_name: None,
        sha256: None,
        expires_at: None,
        missing_ranges,
        error: None,
        retryable: false,
        burn_in_timestamp: false,
        event_seed: request.event_seed.clone(),
        recorded_audio: None,
        timelapse: Some(proto::TimelapseJobSummary {
            options: Some(options.clone()),
            sample_count: plan.estimate.sample_count,
            gap_count: plan.estimate.gap_count,
            output_duration_ms: plan.estimate.output_duration_ms,
            output_width: options.max_width,
            output_height: options.max_height,
            silent: true,
            source_fingerprint: plan.source_fingerprint.clone(),
            site_timezone: plan.site_timezone.clone(),
            manifest_schema_version: MANIFEST_SCHEMA_VERSION,
            manifest_file_name: None,
            manifest_sha256: None,
            manifest_bytes: None,
        }),
    }
}
