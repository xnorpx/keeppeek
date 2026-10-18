//! Admits export attempts with a captured, retry-stable presentation policy.

use super::super::*;

pub(in crate::server) enum TimingSource {
    Configured,
    Captured(Option<proto::RecordedAudioTiming>),
}

#[derive(Clone, Copy)]
struct Range {
    start_ms: i64,
    end_ms: i64,
}

struct Candidate {
    request: proto::CreateExportJob,
    job: proto::ExportJob,
    fragments: Vec<CatalogMediaFragment>,
    range: Range,
    file_name: String,
    artifact_id: String,
    cancel: Arc<AtomicBool>,
    recording_stream_id: Option<String>,
}

struct Retry {
    request: proto::CreateExportJob,
    artifact: String,
    timing: Option<proto::RecordedAudioTiming>,
}

pub(in crate::server) fn retry(
    state: &ServerState,
    requester: &str,
    job_id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    let _configuration = state.config_update.try_lock().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "storage coordination is busy",
        )
    })?;
    reject_maintenance(state)?;
    super::archive::ensure_new_job_id(state, job_id)?;
    let retry = take_retry(state, requester, job_id)?;
    let _ = cleanup_export_attempt_artifacts(state, job_id, &retry.artifact);
    create(
        state,
        requester,
        retry.request,
        TimingSource::Captured(retry.timing),
    )
}

fn take_retry(
    state: &ServerState,
    requester: &str,
    job_id: &str,
) -> Result<Retry, ControlCommandError> {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let record = jobs
        .get(job_id)
        .filter(|record| record.requester_id == requester)
        .ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::NotFound, 404, "export job was not found")
        })?;
    if !matches!(
        proto::ExportJobStatus::try_from(record.job.status),
        Ok(proto::ExportJobStatus::Failed | proto::ExportJobStatus::Cancelled)
    ) || !record.job.retryable
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "export job is not retryable",
        ));
    }
    recorded_audio::validate_snapshot(record.job.recorded_audio.as_ref())?;
    validate_retry_source(state, record)?;
    let retry = Retry {
        request: record.request.clone(),
        artifact: record.artifact_id.clone(),
        timing: record.job.recorded_audio.clone().map(clear_observation),
    };
    record.cancel.store(true, Ordering::Release);
    cleanup_export_attempt_artifacts(state, job_id, &retry.artifact).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("export cleanup could not be admitted: {error}"),
        )
    })?;
    let previous = record.clone();
    jobs.remove(job_id);
    if let Err(error) = super::history::persist(state, &jobs) {
        jobs.insert(job_id.to_owned(), previous);
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to persist export retry: {error}"),
        ));
    }
    Ok(retry)
}

fn validate_retry_source(
    state: &ServerState,
    record: &ExportJobRecord,
) -> Result<(), ControlCommandError> {
    if record.job.recorded_audio.is_none() {
        return Ok(());
    }
    let camera = source(state, &record.request)?;
    let current = recording_stream_id(&camera, &record.request.stream_id);
    if record.recording_stream_id.as_deref() != Some(current.as_str()) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "recording stream changed; create a new export instead of retrying",
        ));
    }
    Ok(())
}

const fn clear_observation(mut timing: proto::RecordedAudioTiming) -> proto::RecordedAudioTiming {
    timing.first_video_pts_us = None;
    timing.first_audio_pts_us = None;
    timing.stored_skew_us = None;
    timing.audio_present = None;
    timing.clipped_audio_samples = None;
    timing
}

pub(in crate::server) fn create(
    state: &ServerState,
    requester: &str,
    request: proto::CreateExportJob,
    timing: TimingSource,
) -> Result<proto::ExportJob, ControlCommandError> {
    admit(state, &request)?;
    let camera = source(state, &request)?;
    let range = range(&request)?;
    let timing = match timing {
        TimingSource::Configured => Some(recorded_audio::configured(
            &camera,
            recording_stream(&camera, &request.stream_id),
        )?),
        TimingSource::Captured(timing) => timing,
    };
    recorded_audio::validate_snapshot(timing.as_ref())?;
    let captured_stream = timing
        .as_ref()
        .map(|_| recording_stream_id(&camera, &request.stream_id));
    let event = export_event_seed(
        state,
        request.event_seed.as_ref(),
        &request.source_id,
        range.start_ms,
        range.end_ms,
    )?;
    {
        let jobs = state
            .export_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = duplicate(
            &jobs,
            requester,
            &request,
            timing.as_ref(),
            captured_stream.as_deref(),
            None,
        ) {
            return Ok(existing);
        }
        reject_existing_id(&jobs, &request.job_id)?;
    }
    let candidate = prepare(state, request, camera, range, event, timing)?;
    if let Some(existing) = register(state, requester, &candidate)? {
        return Ok(existing);
    }
    let job = candidate.job;
    if job.status == proto::ExportJobStatus::Running as i32 {
        spawn_export_worker(
            state.clone(),
            candidate.request,
            candidate.fragments,
            candidate.cancel,
            candidate.artifact_id,
            candidate.range.end_ms,
            candidate.file_name,
        );
    }
    Ok(job)
}

fn admit(state: &ServerState, request: &proto::CreateExportJob) -> Result<(), ControlCommandError> {
    if let Some(error) = &state.export_history_error {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("export history is unavailable: {error}"),
        ));
    }
    reject_maintenance(state)?;
    validate_export_job_id(&request.job_id)?;
    super::archive::ensure_new_job_id(state, &request.job_id)
}

fn reject_maintenance(state: &ServerState) -> Result<(), ControlCommandError> {
    if state.maintenance_active.load(Ordering::Acquire) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "recording maintenance is running",
        ));
    }
    Ok(())
}

fn source(
    state: &ServerState,
    request: &proto::CreateExportJob,
) -> Result<CameraEntry, ControlCommandError> {
    let camera = state.camera(&request.source_id).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "export source was not found",
        )
    })?;
    if privacy_active_for_media(state, &camera.info.id)? {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "camera privacy is active",
        ));
    }
    if !matches!(request.stream_id.as_str(), "main" | "sub")
        || !camera
            .info
            .profiles
            .iter()
            .any(|profile| profile.stream == request.stream_id)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "export stream was not found",
        ));
    }
    Ok(camera)
}

fn range(request: &proto::CreateExportJob) -> Result<Range, ControlCommandError> {
    let start_ms = required_timestamp_ms(request.start_time.as_ref(), "export start time")?;
    let end_ms = required_timestamp_ms(request.end_time.as_ref(), "export end time")?;
    if end_ms <= start_ms {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export end time must follow its start time",
        ));
    }
    if end_ms.saturating_sub(start_ms)
        > i64::try_from(MAX_EXPORT_DURATION.as_millis()).unwrap_or(i64::MAX)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export range exceeds 2 minutes",
        ));
    }
    Ok(Range { start_ms, end_ms })
}

fn duplicate(
    jobs: &HashMap<String, ExportJobRecord>,
    requester: &str,
    request: &proto::CreateExportJob,
    timing: Option<&proto::RecordedAudioTiming>,
    captured_stream: Option<&str>,
    partial: Option<bool>,
) -> Option<proto::ExportJob> {
    jobs.values()
        .find(|record| {
            record.requester_id == requester
                && record.job.status == proto::ExportJobStatus::Running as i32
                && recorded_audio::same_snapshot(record.job.recorded_audio.as_ref(), timing)
                && record.recording_stream_id.as_deref() == captured_stream
                && partial.map_or_else(
                    || export_requests_match_output(&record.request, request),
                    |partial| export_requests_match_normalized(record, request, partial),
                )
        })
        .map(|record| record.job.clone())
}

fn reject_existing_id(
    jobs: &HashMap<String, ExportJobRecord>,
    id: &str,
) -> Result<(), ControlCommandError> {
    if jobs.contains_key(id) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "export job ID already exists",
        ));
    }
    Ok(())
}

fn prepare(
    state: &ServerState,
    request: proto::CreateExportJob,
    camera: CameraEntry,
    range: Range,
    event: Option<proto::EventExportSeed>,
    timing: Option<proto::RecordedAudioTiming>,
) -> Result<Candidate, ControlCommandError> {
    let catalog = state.catalog.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        )
    })?;
    let stream_id = recording_stream_id(&camera, &request.stream_id);
    let fragments = catalog
        .media_fragments_in_range(&stream_id, range.start_ms, range.end_ms)
        .map_err(|error| stored_catalog_error("query export fragments", error))?;
    let missing = export_missing_ranges(&fragments, range.start_ms, range.end_ms);
    let captured_stream = timing.as_ref().map(|_| stream_id);
    let job = job(&request, range, &fragments, missing, event, timing);
    let label = camera
        .info
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&camera.recording_label);
    Ok(Candidate {
        file_name: export_file_name(label, range.start_ms, range.end_ms),
        request,
        job,
        fragments,
        range,
        artifact_id: Uuid::new_v4().simple().to_string(),
        cancel: Arc::new(AtomicBool::new(false)),
        recording_stream_id: captured_stream,
    })
}

fn job(
    request: &proto::CreateExportJob,
    range: Range,
    fragments: &[CatalogMediaFragment],
    missing: Vec<(i64, i64)>,
    event: Option<proto::EventExportSeed>,
    timing: Option<proto::RecordedAudioTiming>,
) -> proto::ExportJob {
    let status = if request.burn_in_timestamp {
        proto::ExportJobStatus::Failed
    } else if fragments.is_empty() || (!missing.is_empty() && !request.allow_partial) {
        proto::ExportJobStatus::Partial
    } else {
        proto::ExportJobStatus::Running
    };
    proto::ExportJob {
        job_id: request.job_id.clone(),
        source_id: request.source_id.clone(),
        stream_id: request.stream_id.clone(),
        requested_start_time: Some(millis_timestamp(range.start_ms)),
        requested_end_time: Some(millis_timestamp(range.end_ms)),
        aligned_start_time: fragments
            .first()
            .map(|fragment| millis_timestamp(fragment.start_ms)),
        status: status as i32,
        progress_per_mille: if status == proto::ExportJobStatus::Running {
            100
        } else {
            0
        },
        bytes_written: 0,
        estimated_bytes: Some(export_estimated_bytes(fragments)),
        file_name: None,
        sha256: None,
        expires_at: None,
        missing_ranges: missing
            .into_iter()
            .map(|(start, end)| proto::ExportMissingRange {
                start_time: Some(millis_timestamp(start)),
                end_time: Some(millis_timestamp(end)),
            })
            .collect(),
        error: request
            .burn_in_timestamp
            .then(|| "Timestamp burn-in requires a configured re-encoding worker".to_owned()),
        retryable: false,
        burn_in_timestamp: request.burn_in_timestamp,
        event_seed: event,
        recorded_audio: timing,
    }
}

fn register(
    state: &ServerState,
    requester: &str,
    candidate: &Candidate,
) -> Result<Option<proto::ExportJob>, ControlCommandError> {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reject_maintenance(state)?;
    if let Some(existing) = duplicate(
        &jobs,
        requester,
        &candidate.request,
        candidate.job.recorded_audio.as_ref(),
        candidate.recording_stream_id.as_deref(),
        Some(!candidate.job.missing_ranges.is_empty()),
    ) {
        return Ok(Some(existing));
    }
    reject_existing_id(&jobs, &candidate.request.job_id)?;
    if jobs.len() >= MAX_EXPORT_HISTORY_JOBS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "export history is full; wait for active jobs or retention cleanup",
        ));
    }
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let running = candidate.job.status == proto::ExportJobStatus::Running as i32;
    jobs.insert(
        candidate.request.job_id.clone(),
        ExportJobRecord {
            requester_id: requester.to_owned(),
            artifact_id: candidate.artifact_id.clone(),
            request: candidate.request.clone(),
            job: candidate.job.clone(),
            path: None,
            cancel: candidate.cancel.clone(),
            created_at_ms: now_ms,
            started_at_ms: running.then_some(now_ms),
            updated_at_ms: now_ms,
            completed_at_ms: (!running).then_some(now_ms),
            downloaded_at_ms: None,
            recording_stream_id: candidate.recording_stream_id.clone(),
        },
    );
    if let Err(error) = super::history::persist(state, &jobs) {
        jobs.remove(&candidate.request.job_id);
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to persist export job: {error}"),
        ));
    }
    Ok(None)
}

#[cfg(test)]
mod timing_tests {
    use super::*;

    fn running(timing: Option<proto::RecordedAudioTiming>) -> ExportJobRecord {
        let captured_stream = timing.as_ref().map(|_| "camera/sub".to_owned());
        ExportJobRecord {
            requester_id: "owner".to_owned(),
            artifact_id: Uuid::new_v4().simple().to_string(),
            request: proto::CreateExportJob {
                job_id: "original".to_owned(),
                source_id: "camera".to_owned(),
                stream_id: "sub".to_owned(),
                ..Default::default()
            },
            job: proto::ExportJob {
                job_id: "original".to_owned(),
                status: proto::ExportJobStatus::Running as i32,
                recorded_audio: timing,
                ..Default::default()
            },
            path: None,
            cancel: Arc::new(AtomicBool::new(false)),
            created_at_ms: 0,
            started_at_ms: Some(0),
            updated_at_ms: 0,
            completed_at_ms: None,
            downloaded_at_ms: None,
            recording_stream_id: captured_stream,
        }
    }

    #[test]
    fn dedupe_requires_the_same_captured_timing_and_does_not_merge_legacy_unknown() {
        let timing = proto::RecordedAudioTiming {
            configured_offset_ms: 250,
            applied_offset_ms: 250,
            mapping_version: 1,
            configuration_revision: "a".repeat(64),
            ..Default::default()
        };
        let record = running(Some(timing.clone()));
        let mut request = record.request.clone();
        request.job_id = "another".to_owned();
        let jobs = HashMap::from([("original".to_owned(), record)]);
        let stream = Some("camera/sub");
        assert_eq!(
            duplicate(&jobs, "owner", &request, Some(&timing), stream, None)
                .unwrap()
                .job_id,
            "original"
        );
        let changed = proto::RecordedAudioTiming {
            configured_offset_ms: -100,
            applied_offset_ms: -100,
            configuration_revision: "b".repeat(64),
            ..timing
        };
        assert!(duplicate(&jobs, "owner", &request, Some(&changed), stream, None).is_none());
        assert!(duplicate(&jobs, "other-owner", &request, Some(&timing), stream, None).is_none());
        assert!(
            duplicate(
                &jobs,
                "owner",
                &request,
                Some(&timing),
                Some("camera/main"),
                None
            )
            .is_none()
        );
        assert!(duplicate(&jobs, "owner", &request, None, None, None).is_none());
        let legacy = HashMap::from([("original".to_owned(), running(None))]);
        assert!(duplicate(&legacy, "owner", &request, Some(&timing), stream, None).is_none());
        assert!(duplicate(&legacy, "owner", &request, None, None, None).is_some());
    }
}
