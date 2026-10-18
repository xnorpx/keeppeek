use super::*;
use std::sync::MutexGuard;

struct Range {
    camera: CameraEntry,
    start_ms: i64,
    end_ms: i64,
    event_seed: Option<proto::EventExportSeed>,
}

struct Plan {
    record: ExportJobRecord,
    fragments: Vec<CatalogMediaFragment>,
    has_missing_ranges: bool,
    end_ms: i64,
    file_name: String,
}

#[cfg(test)]
pub(in crate::server) fn create(
    state: &ServerState,
    requester: &str,
    request: proto::CreateExportJob,
) -> Result<proto::ExportJob, ControlCommandError> {
    create_with_owner(state, requester, None, request)
}

pub(in crate::server) fn create_for_principal(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::CreateExportJob,
) -> Result<proto::ExportJob, ControlCommandError> {
    create_with_owner(
        state,
        &principal.id(),
        Some(principal.sharing_issuer().creator_id()),
        request,
    )
}

fn create_with_owner(
    state: &ServerState,
    requester: &str,
    owner_key: Option<String>,
    request: proto::CreateExportJob,
) -> Result<proto::ExportJob, ControlCommandError> {
    let configuration = configuration_guard(state)?;
    create_locked(state, requester, request, owner_key, &configuration)
}

pub(in crate::server) fn create_locked(
    state: &ServerState,
    requester: &str,
    request: proto::CreateExportJob,
    owner_key: Option<String>,
    _configuration: &MutexGuard<'_, ()>,
) -> Result<proto::ExportJob, ControlCommandError> {
    available(state)?;
    validate_export_job_id(&request.job_id)?;
    super::archive::ensure_new_job_id(state, &request.job_id)?;
    let range = validate(state, &request)?;
    if let Some(job) = existing_before_plan(state, requester, &request)? {
        return Ok(job);
    }
    let plan = plan(state, requester, request, range, owner_key)?;
    if let Some(existing) = insert(state, &plan)? {
        return Ok(existing);
    }
    let job = plan.record.job.clone();
    if job.status == proto::ExportJobStatus::Running as i32 {
        spawn_export_worker(
            state.clone(),
            plan.record.request,
            plan.fragments,
            plan.record.cancel,
            plan.record.artifact_id,
            plan.end_ms,
            plan.file_name,
        );
    }
    Ok(job)
}

fn configuration_guard(state: &ServerState) -> Result<MutexGuard<'_, ()>, ControlCommandError> {
    state.config_update.try_lock().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "storage coordination is busy",
        )
    })
}

fn available(state: &ServerState) -> Result<(), ControlCommandError> {
    if let Some(error) = &state.export_history_error {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("export history is unavailable: {error}"),
        ));
    }
    maintenance_idle(state)
}

fn maintenance_idle(state: &ServerState) -> Result<(), ControlCommandError> {
    if state.maintenance_active.load(Ordering::Acquire) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "recording maintenance is running",
        ));
    }
    Ok(())
}

fn validate(
    state: &ServerState,
    request: &proto::CreateExportJob,
) -> Result<Range, ControlCommandError> {
    let camera = camera(state, request)?;
    let (start_ms, end_ms) = interval(request)?;
    let event_seed = export_event_seed(
        state,
        request.event_seed.as_ref(),
        &request.source_id,
        start_ms,
        end_ms,
    )?;
    Ok(Range {
        camera,
        start_ms,
        end_ms,
        event_seed,
    })
}

fn camera(
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

fn interval(request: &proto::CreateExportJob) -> Result<(i64, i64), ControlCommandError> {
    let start = required_timestamp_ms(request.start_time.as_ref(), "export start time")?;
    let end = required_timestamp_ms(request.end_time.as_ref(), "export end time")?;
    if end <= start {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export end time must follow its start time",
        ));
    }
    if end.saturating_sub(start)
        > i64::try_from(MAX_EXPORT_DURATION.as_millis()).unwrap_or(i64::MAX)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export range exceeds 2 minutes",
        ));
    }
    Ok((start, end))
}

fn existing_before_plan(
    state: &ServerState,
    requester: &str,
    request: &proto::CreateExportJob,
) -> Result<Option<proto::ExportJob>, ControlCommandError> {
    let jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = jobs.values().find(|record| {
        record.requester_id == requester
            && record.job.status == proto::ExportJobStatus::Running as i32
            && export_requests_match_output(&record.request, request)
    }) {
        return Ok(Some(existing.job.clone()));
    }
    if jobs.contains_key(&request.job_id) {
        return Err(duplicate());
    }
    Ok(None)
}

fn plan(
    state: &ServerState,
    requester: &str,
    request: proto::CreateExportJob,
    range: Range,
    owner_key: Option<String>,
) -> Result<Plan, ControlCommandError> {
    let stream = recording_stream_id(&range.camera, &request.stream_id);
    let catalog = state.catalog.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        )
    })?;
    let fragments = catalog
        .media_fragments_in_range(&stream, range.start_ms, range.end_ms)
        .map_err(|error| stored_catalog_error("query export fragments", error))?;
    let missing = export_missing_ranges(&fragments, range.start_ms, range.end_ms);
    let has_missing_ranges = !missing.is_empty();
    let file_name = export_file_name(
        range
            .camera
            .info
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&range.camera.recording_label),
        range.start_ms,
        range.end_ms,
    );
    let job = job(&request, &range, &fragments, &missing);
    let now = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let running = job.status == proto::ExportJobStatus::Running as i32;
    let record = ExportJobRecord {
        requester_id: requester.to_owned(),
        artifact_id: Uuid::new_v4().simple().to_string(),
        source_identity: source_identity(state, &request.source_id),
        owner_key,
        media_proof: None,
        request,
        job,
        path: None,
        cancel: Arc::new(AtomicBool::new(false)),
        created_at_ms: now,
        started_at_ms: running.then_some(now),
        updated_at_ms: now,
        completed_at_ms: (!running).then_some(now),
        downloaded_at_ms: None,
    };
    Ok(Plan {
        record,
        fragments,
        has_missing_ranges,
        end_ms: range.end_ms,
        file_name,
    })
}

fn job(
    request: &proto::CreateExportJob,
    range: &Range,
    fragments: &[CatalogMediaFragment],
    missing: &[(i64, i64)],
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
            .iter()
            .map(|(start, end)| proto::ExportMissingRange {
                start_time: Some(millis_timestamp(*start)),
                end_time: Some(millis_timestamp(*end)),
            })
            .collect(),
        error: request
            .burn_in_timestamp
            .then(|| "Timestamp burn-in requires a configured re-encoding worker".to_owned()),
        retryable: false,
        burn_in_timestamp: request.burn_in_timestamp,
        event_seed: range.event_seed.clone(),
    }
}

fn source_identity(state: &ServerState, source: &str) -> Option<Uuid> {
    let path = state.camera_config_path.as_deref()?;
    let address = source.parse::<IpAddr>().ok()?;
    let configuration = config::load_config(path).ok()?;
    let identities = configuration.camera_profile_identities().ok()?;
    Uuid::parse_str(identities.get(&address)?)
        .ok()
        .filter(|identity| !identity.is_nil())
}

fn insert(
    state: &ServerState,
    plan: &Plan,
) -> Result<Option<proto::ExportJob>, ControlCommandError> {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    maintenance_idle(state)?;
    let record = &plan.record;
    if let Some(existing) = jobs.values().find(|other| {
        other.requester_id == record.requester_id
            && other.job.status == proto::ExportJobStatus::Running as i32
            && export_requests_match_normalized(other, &record.request, plan.has_missing_ranges)
    }) {
        return Ok(Some(existing.job.clone()));
    }
    if jobs.contains_key(&record.job.job_id) {
        return Err(duplicate());
    }
    if jobs.len() >= MAX_EXPORT_HISTORY_JOBS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "export history is full; wait for active jobs or retention cleanup",
        ));
    }
    jobs.insert(record.job.job_id.clone(), record.clone());
    if let Err(error) = super::history::persist(state, &jobs) {
        jobs.remove(&record.job.job_id);
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to persist export job: {error}"),
        ));
    }
    Ok(None)
}

fn duplicate() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        409,
        "export job ID already exists",
    )
}

#[cfg(test)]
pub(in crate::server) fn retry(
    state: &ServerState,
    requester: &str,
    id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    retry_with_owner(state, requester, None, id)
}

pub(in crate::server) fn retry_for_principal(
    state: &ServerState,
    principal: &ApiPrincipal,
    id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    retry_with_owner(
        state,
        &principal.id(),
        Some(principal.sharing_issuer().creator_id()),
        id,
    )
}

fn retry_with_owner(
    state: &ServerState,
    requester: &str,
    owner_key: Option<String>,
    id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    let configuration = configuration_guard(state)?;
    maintenance_idle(state)?;
    super::archive::ensure_new_job_id(state, id)?;
    let (request, artifact) = retire_retry(state, requester, id)?;
    let _ = cleanup_export_attempt_artifacts(state, id, &artifact);
    create_locked(state, requester, request, owner_key, &configuration)
}

fn retire_retry(
    state: &ServerState,
    requester: &str,
    id: &str,
) -> Result<(proto::CreateExportJob, String), ControlCommandError> {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let record = jobs
        .get(id)
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
    record.cancel.store(true, Ordering::Release);
    let request = record.request.clone();
    let artifact = record.artifact_id.clone();
    cleanup_export_attempt_artifacts(state, id, &artifact).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("export cleanup could not be admitted: {error}"),
        )
    })?;
    let previous = record.clone();
    jobs.remove(id);
    if let Err(error) = super::history::persist(state, &jobs) {
        jobs.insert(id.to_owned(), previous);
        return Err(ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            format!("unable to persist export retry: {error}"),
        ));
    }
    Ok((request, artifact))
}
