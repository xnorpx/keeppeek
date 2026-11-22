use super::*;
use crate::shares::{ExportBinding, IssuerBinding};
use crate::storage::{catalog::readers::LeaseSet, volumes::root::OwnedFile};

pub(in crate::server) struct ValidatedEvidence {
    pub binding: ExportBinding,
    pub camera_name: String,
    pub has_gaps: bool,
}

pub(in crate::server) struct OpenedEvidence {
    pub file: OwnedFile,
    pub lease: LeaseSet,
}

pub(super) fn preview(
    state: &ServerState,
    principal: &ApiPrincipal,
    id: &str,
    range: Option<&proto::ShareRecordingRange>,
) -> Result<ValidatedEvidence, ControlCommandError> {
    if principal.role != AccessRole::Administrator {
        return Err(denied());
    }
    let record = record(state, id)?;
    let issuer = principal.sharing_issuer();
    let policy = camera_access::for_principal(state, principal)?;
    if range.is_some()
        && record.requester_id == principal.id()
        && record.job.status == proto::ExportJobStatus::Ready as i32
        && (record.owner_key.is_none() || record.source_identity.is_none())
    {
        camera_access::require_camera(&policy, &record.job.source_id)?;
        return Err(missing_range_proof());
    }
    require_owner(&record, &issuer)?;
    let current_source = source(state, record.source_identity.ok_or_else(denied)?)?;
    camera_access::require_camera(&policy, &current_source)?;
    if range.is_some()
        && record.media_proof.is_none()
        && record.job.status == proto::ExportJobStatus::Ready as i32
    {
        return Err(missing_range_proof());
    }
    let binding = binding(&record)?;
    if let Some(selected) = range {
        check_range(&record, &binding, &current_source, selected)?;
    }
    let checked = live_evidence(state, &record, binding)?;
    let opened = open_guest(state, &checked.binding)?;
    if range.is_some() {
        super::super::guest_review::inspect_range_codec(opened.file, checked.binding.bytes)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::InvalidData {
                    ControlCommandError::new(
                        proto::ErrorCode::Rejected,
                        409,
                        "the selected recording could not be verified as browser-compatible",
                    )
                } else {
                    unavailable()
                }
            })?;
    }
    Ok(checked)
}

fn missing_range_proof() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        409,
        "the completed export has no recorded media proof",
    )
}

// The original selection and the pinned artifact must both fit the requested interval.
fn check_range(
    record: &ExportJobRecord,
    binding: &ExportBinding,
    current_source: &str,
    selected: &proto::ShareRecordingRange,
) -> Result<(), ControlCommandError> {
    if selected.start_ms < 0
        || selected.end_ms <= selected.start_ms
        || selected.end_ms.saturating_sub(selected.start_ms) > crate::shares::MAX_MEDIA_DURATION_MS
    {
        return Err(invalid("select a recording interval of up to two minutes"));
    }
    // Corrected audio may come from neighboring source times. This proof describes
    // the output presentation timeline, so it cannot authorize those samples.
    if record
        .job
        .recorded_audio
        .as_ref()
        .is_some_and(|timing| timing.applied_offset_ms != 0)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "recorded audio correction prevents verification of the selected source interval",
        ));
    }
    if selected.source_id != current_source
        || selected.source_id != binding.source_id
        || selected.start_ms
            != required_timestamp_ms(record.request.start_time.as_ref(), "export start")?
        || selected.end_ms != required_timestamp_ms(record.request.end_time.as_ref(), "export end")?
        || binding.start_ms < selected.start_ms
        || binding.end_ms > selected.end_ms
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "the recording cannot be shared within the selected range",
        ));
    }
    Ok(())
}

pub(in crate::server) fn validate_guest(
    state: &ServerState,
    grant: &GrantSummary,
) -> Result<ValidatedEvidence, ControlCommandError> {
    context(state)?;
    let now_ms = i64::try_from(unix_time_ms()).map_err(|_| unavailable())?;
    if !authentication::sharing_administrator(state, &grant.intent.issuer, now_ms) {
        return Err(denied());
    }
    let record = record(state, &grant.intent.binding.job_id)?;
    require_owner(&record, &grant.intent.issuer)?;
    let current = binding(&record)?;
    if current != grant.intent.binding {
        return Err(denied());
    }
    live_evidence(state, &record, current)
}

fn live_evidence(
    state: &ServerState,
    record: &ExportJobRecord,
    binding: ExportBinding,
) -> Result<ValidatedEvidence, ControlCommandError> {
    if state.maintenance_active.load(Ordering::Acquire) || state.export_history_error.is_some() {
        return Err(unavailable());
    }
    let current_source = source(state, binding.source_identity)?;
    let camera_name = camera_label(state, &current_source)?;
    if privacy_active_for_media(state, &current_source)? {
        return Err(denied());
    }
    let now_ms = i64::try_from(unix_time_ms()).map_err(|_| unavailable())?;
    if now_ms >= binding.available_until_ms {
        return Err(denied());
    }
    Ok(ValidatedEvidence {
        binding,
        camera_name,
        has_gaps: !record.job.missing_ranges.is_empty(),
    })
}

pub(in crate::server) fn open_guest(
    state: &ServerState,
    expected: &ExportBinding,
) -> Result<OpenedEvidence, ControlCommandError> {
    let record = record(state, &expected.job_id)?;
    if binding(&record)? != *expected {
        return Err(denied());
    }
    let catalog = state.catalog.as_ref().ok_or_else(unavailable)?;
    let (location, lease) = catalog
        .leased_export(&expected.artifact_id)
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    if location.bytes != expected.bytes {
        return Err(denied());
    }
    if location.digest != expected.sha256 {
        return Err(denied());
    }
    let manager = state
        .storage_config
        .volume_runtime
        .as_ref()
        .ok_or_else(unavailable)?;
    let file = manager.open_owned(&location).map_err(|_| unavailable())?;
    Ok(OpenedEvidence { file, lease })
}

fn record(state: &ServerState, id: &str) -> Result<ExportJobRecord, ControlCommandError> {
    if id.is_empty() || id.len() > 128 {
        return Err(invalid("invalid export identity"));
    }
    state
        .export_jobs
        .lock()
        .map_err(|_| unavailable())?
        .get(id)
        .cloned()
        .ok_or_else(denied)
}

fn require_owner(
    record: &ExportJobRecord,
    issuer: &IssuerBinding,
) -> Result<(), ControlCommandError> {
    if record.owner_key.as_deref() == Some(issuer.creator_id().as_str()) {
        Ok(())
    } else {
        Err(denied())
    }
}

fn binding(record: &ExportJobRecord) -> Result<ExportBinding, ControlCommandError> {
    if record.job.status != proto::ExportJobStatus::Ready as i32 {
        return Err(denied());
    }
    let bounds = record.verified_media_bounds().ok_or_else(denied)?;
    let start_ms = bounds.start_ms;
    let end_ms = bounds.end_ms;
    let available_until_ms =
        required_timestamp_ms(record.job.expires_at.as_ref(), "export expiry")?;
    if end_ms <= start_ms
        || end_ms.saturating_sub(start_ms) > crate::shares::MAX_MEDIA_DURATION_MS
        || record.job.bytes_written == 0
        || record.job.bytes_written > crate::shares::MAX_MEDIA_BYTES
    {
        return Err(denied());
    }
    Ok(ExportBinding {
        job_id: record.job.job_id.clone(),
        artifact_id: record.artifact_id.clone(),
        sha256: digest(record.job.sha256.as_deref().ok_or_else(denied)?)?,
        source_id: record.job.source_id.clone(),
        source_identity: record
            .source_identity
            .filter(|id| !id.is_nil())
            .ok_or_else(denied)?,
        start_ms,
        end_ms,
        bytes: record.job.bytes_written,
        available_until_ms,
    })
}

fn digest(value: &str) -> Result<[u8; 32], ControlCommandError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(denied());
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(|_| denied())?;
    }
    Ok(digest)
}

fn camera_label(state: &ServerState, source: &str) -> Result<String, ControlCommandError> {
    let cameras = state.cameras.read().map_err(|_| unavailable())?;
    let camera = cameras
        .iter()
        .find(|camera| camera.info.id == source)
        .ok_or_else(denied)?;
    Ok(bounded_label(
        camera.info.name.as_deref().unwrap_or("Camera"),
    ))
}

fn bounded_label(name: &str) -> String {
    let mut label = String::with_capacity(128);
    for character in name.chars().take(128) {
        if character.is_control() {
            return "Camera".into();
        }
        if label.len() + character.len_utf8() > 128 {
            break;
        }
        label.push(character);
    }
    if label.trim().is_empty() {
        "Camera".into()
    } else {
        label.trim().to_owned()
    }
}
