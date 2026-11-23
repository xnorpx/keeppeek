//! Keeps the original export owner and download description with a verified archive copy.

use super::*;
use crate::storage::{
    archive::{VerifiedReader, target::Work},
    catalog::archive::{Job, Reply, Request},
};

pub(in crate::server) struct CopyMetadata {
    pub original_job_id: String,
    pub json: String,
}

pub(in crate::server) fn metadata_for_copy(
    state: &ServerState,
    artifact_id: &str,
) -> Result<CopyMetadata, ControlCommandError> {
    let jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let record = jobs
        .values()
        .find(|record| {
            record.artifact_id == artifact_id
                && record.job.status == proto::ExportJobStatus::Ready as i32
        })
        .ok_or_else(|| rejected("archive copy requires a completed export in retained history"))?;
    if record.job.sha256.is_none() {
        return Err(rejected("completed export has no checksum"));
    }
    let json = serde_json::to_string(&PersistedExportJobRecord::from_record(record))
        .map_err(unavailable)?;
    if json.is_empty() || json.len() > 65_536 {
        return Err(rejected("archive export description exceeds 64 KiB"));
    }
    Ok(CopyMetadata {
        original_job_id: record.job.job_id.clone(),
        json,
    })
}

pub(in crate::server) fn ensure_new_job_id(
    state: &ServerState,
    job_id: &str,
) -> Result<(), ControlCommandError> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Ok(());
    };
    let Reply::Job(existing) = catalog
        .archive(Request::FindExport(job_id.into()))
        .map_err(unavailable)?
    else {
        return Err(unavailable(
            "archive export lookup returned an invalid reply",
        ));
    };
    if existing.is_some() {
        return Err(rejected("export job ID belongs to a durable archive copy"));
    }
    Ok(())
}

pub(in crate::server) fn snapshot(
    state: &ServerState,
    requester: &str,
    job_id: &str,
) -> Result<Option<(ExportJobRecord, Job)>, ControlCommandError> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Ok(None);
    };
    let Reply::Job(Some(archive)) = catalog
        .archive(Request::FindExport(job_id.into()))
        .map_err(unavailable)?
    else {
        return Ok(None);
    };
    if archive.state != "ready" || archive.version.is_none() {
        return Ok(None);
    }
    if archive.kind != "export" || archive.export_job_id.as_deref() != Some(job_id) {
        return Err(unavailable("archive export identity changed"));
    }
    let json = archive
        .export_metadata
        .as_deref()
        .ok_or_else(|| unavailable("archive export description is missing"))?;
    Ok(verified_record(
        requester,
        job_id,
        &archive.recording_id,
        archive.digest,
        json,
    )?
    .map(|record| (record, *archive)))
}

fn verified_record(
    requester: &str,
    job_id: &str,
    artifact_id: &str,
    digest: [u8; 32],
    json: &str,
) -> Result<Option<ExportJobRecord>, ControlCommandError> {
    if json.len() > 65_536 {
        return Err(unavailable("archive export description is too large"));
    }
    let persisted: PersistedExportJobRecord = serde_json::from_str(json).map_err(unavailable)?;
    if persisted.requester_id != requester {
        return Ok(None);
    }
    let (request, mut job) = persisted.messages().map_err(unavailable)?;
    if persisted.artifact_id != artifact_id
        || request.job_id != job_id
        || request.source_id != job.source_id
        || job.status != proto::ExportJobStatus::Ready as i32
        || job.sha256.as_deref() != Some(&encode_lower_hex(digest))
    {
        return Err(unavailable(
            "archive export description does not match verified source",
        ));
    }
    job.expires_at = None;
    let record = ExportJobRecord {
        recording_stream_id: persisted.recording_stream_id,
        requester_id: persisted.requester_id,
        artifact_id: persisted.artifact_id,
        source_identity: persisted.source_identity,
        owner_key: persisted.owner_key,
        media_proof: persisted.media_proof,
        request,
        job,
        path: None,
        cancel: Arc::new(AtomicBool::new(false)),
        created_at_ms: persisted.created_at_ms,
        started_at_ms: persisted.started_at_ms,
        updated_at_ms: persisted.updated_at_ms,
        completed_at_ms: persisted.completed_at_ms,
        downloaded_at_ms: persisted.downloaded_at_ms,
    };
    Ok(Some(record))
}

pub(in crate::server) fn get(
    state: &ServerState,
    requester: &str,
    id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    match export_job(state, requester, id) {
        Ok(job) => Ok(job),
        Err(missing) => {
            let Some(catalog) = state.catalog.as_ref() else {
                return Err(missing);
            };
            let query = Request::GetOwnerExport {
                requester: requester.into(),
                original_id: id.into(),
            };
            let Ok(Reply::ExportDescriptions(rows)) = catalog.archive(query) else {
                return Err(unavailable("archive export description could not be read"));
            };
            let Some(row) = rows.first() else {
                return Err(missing);
            };
            discoverable(state, requester, row)?.ok_or(missing)
        }
    }
}

pub(in crate::server) fn list(
    state: &ServerState,
    requester: &str,
) -> Result<proto::ExportJobList, ControlCommandError> {
    let mut result = proto::ExportJobList {
        jobs: export_jobs(state, requester),
    };
    let budget =
        (crate::webrtc::MAX_CONTROL_MESSAGE_BYTES - 1_024).saturating_sub(result.encoded_len());
    let Some(catalog) = state.catalog.as_ref() else {
        return Ok(result);
    };
    if budget == 0 || result.jobs.len() > MAX_EXPORT_HISTORY_JOBS {
        return Ok(result);
    }
    let query = Request::ListOwnerExports {
        requester: requester.to_owned(),
        retained: result.jobs.iter().map(|job| job.job_id.clone()).collect(),
    };
    let descriptions = match catalog.archive(query) {
        Ok(Reply::ExportDescriptions(rows)) => rows,
        _ if !result.jobs.is_empty() => return Ok(result),
        _ => return Err(unavailable("archive export descriptions could not be read")),
    };

    let mut remaining = budget;
    for row in descriptions {
        let Ok(Some(job)) = discoverable(state, requester, &row) else {
            continue;
        };
        let size = job.encoded_len();
        let encoded = 1 + prost::length_delimiter_len(size) + size;
        if encoded <= remaining {
            remaining -= encoded;
            result.jobs.push(job);
        }
    }
    result.jobs.sort_unstable_by(|left, right| {
        export_requested_start(right)
            .cmp(&export_requested_start(left))
            .then_with(|| left.job_id.cmp(&right.job_id))
    });
    Ok(result)
}

fn discoverable(
    state: &ServerState,
    requester: &str,
    row: &crate::storage::catalog::archive::exports::Description,
) -> Result<Option<proto::ExportJob>, ControlCommandError> {
    let Some(record) = verified_record(
        requester,
        &row.original_id,
        &row.artifact_id,
        row.digest,
        &row.metadata,
    )?
    else {
        return Ok(None);
    };
    if privacy_active_for_media(state, &record.job.source_id)? {
        return Ok(None);
    }
    Ok(Some(record.job))
}

pub(in crate::server) fn payload(
    state: &ServerState,
    archive: &Job,
) -> Result<Vec<u8>, ControlCommandError> {
    if archive.bytes > MAX_EXPORT_DOWNLOAD_BYTES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            413,
            "export file exceeds the browser download limit",
        ));
    }
    let catalog = state
        .catalog
        .as_ref()
        .ok_or_else(|| unavailable("export catalog unavailable"))?;
    let (descriptor, _lease) = catalog
        .leased_archive_export(archive)
        .map_err(unavailable)?;
    let never = || false;
    let work = Work {
        deadline: Instant::now() + Duration::from_secs(120),
        cancelled: &never,
    };
    let mut reader = VerifiedReader::new(descriptor, work).map_err(unavailable)?;
    let mut bytes = vec![0; usize::try_from(archive.bytes).map_err(unavailable)?];
    reader.read_exact(&mut bytes).map_err(unavailable)?;
    Ok(bytes)
}

fn rejected(message: impl std::fmt::Display) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, 409, message.to_string())
}

fn unavailable(message: impl std::fmt::Display) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        format!("archive export is unavailable: {message}"),
    )
}
