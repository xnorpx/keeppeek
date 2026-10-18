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
    if archive.state != "ready" {
        return Ok(None);
    }
    let json = archive
        .export_metadata
        .as_deref()
        .ok_or_else(|| unavailable("archive export description is missing"))?;
    if json.len() > 65_536 {
        return Err(unavailable("archive export description is too large"));
    }
    let persisted: PersistedExportJobRecord = serde_json::from_str(json).map_err(unavailable)?;
    if persisted.requester_id != requester {
        return Ok(None);
    }
    let (request, mut job) = persisted.messages().map_err(unavailable)?;
    if archive.kind != "export"
        || archive.export_job_id.as_deref() != Some(job_id)
        || persisted.artifact_id != archive.recording_id
        || request.job_id != job_id
        || request.source_id != job.source_id
        || job.status != proto::ExportJobStatus::Ready as i32
        || job.sha256.as_deref() != Some(&encode_lower_hex(archive.digest))
    {
        return Err(unavailable(
            "archive export description does not match verified source",
        ));
    }
    job.expires_at = None;
    let record = ExportJobRecord {
        requester_id: persisted.requester_id,
        artifact_id: persisted.artifact_id,
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
    Ok(Some((record, archive)))
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
