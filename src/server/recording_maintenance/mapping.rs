use super::{ControlCommandError, error, maintenance, proto};
use maintenance::jobs::{self, execution};

pub(super) fn scope(
    request: proto::RecordingMaintenanceScope,
) -> Result<maintenance::Scope, ControlCommandError> {
    let source_id = request.source_id;
    let stream_id = request.stream_id;
    match request.selection {
        Some(proto::recording_maintenance_scope::Selection::RecordingId(recording_id)) => {
            Ok(maintenance::Scope::Recording {
                source_id,
                stream_id,
                recording_id,
            })
        }
        Some(proto::recording_maintenance_scope::Selection::Range(range)) => {
            Ok(maintenance::Scope::TimeRange {
                source_id,
                stream_id,
                start_ms: range.start_ms,
                end_ms: range.end_ms,
            })
        }
        None => Err(error(
            proto::ErrorCode::InvalidRequest,
            400,
            "recording ID or interval is required",
        )),
    }
}

fn wire_scope(scope: &maintenance::Scope) -> proto::RecordingMaintenanceScope {
    match scope {
        maintenance::Scope::Recording {
            source_id,
            stream_id,
            recording_id,
        } => proto::RecordingMaintenanceScope {
            source_id: source_id.clone(),
            stream_id: stream_id.clone(),
            selection: Some(proto::recording_maintenance_scope::Selection::RecordingId(
                recording_id.clone(),
            )),
        },
        maintenance::Scope::TimeRange {
            source_id,
            stream_id,
            start_ms,
            end_ms,
        } => proto::RecordingMaintenanceScope {
            source_id: source_id.clone(),
            stream_id: stream_id.clone(),
            selection: Some(proto::recording_maintenance_scope::Selection::Range(
                proto::RecordingMaintenanceRange {
                    start_ms: *start_ms,
                    end_ms: *end_ms,
                },
            )),
        },
    }
}

pub(super) fn confirmation_text(count: usize) -> String {
    format!("DELETE {count}")
}

pub(super) fn blocked(
    snapshot: &maintenance::Snapshot,
    reason: jobs::Reason,
    export_ids: Vec<String>,
) -> proto::RecordingDeletionJob {
    let mut result = proto::RecordingDeletionJob {
        scope: Some(wire_scope(&snapshot.scope)),
        revision: snapshot.revision,
        reason: match reason {
            jobs::Reason::Operator => proto::RecordingDeletionReason::Operator,
            jobs::Reason::Privacy => proto::RecordingDeletionReason::Privacy,
        } as i32,
        status: proto::RecordingDeletionStatus::Blocked as i32,
        bytes: snapshot.catalog_bytes,
        bookmark_event_ids: snapshot.evidence.bookmarks.clone(),
        related_export_ids: export_ids,
        gaps: snapshot
            .evidence
            .gaps
            .iter()
            .map(|range| proto::RecordingMaintenanceRange {
                start_ms: range.start_ms,
                end_ms: range.end_ms,
            })
            .collect(),
        ..Default::default()
    };
    result.objects = snapshot
        .recordings
        .iter()
        .map(|recording| {
            let error = if !recording.finalized {
                Some("Recording is active")
            } else if recording.protected {
                Some("Recording is protected")
            } else if recording.cleanup_pending {
                Some("Retention is pending")
            } else if recording.file_identity.is_none() {
                Some("File identity is unavailable")
            } else if recording.ended_at_ms.is_none() {
                Some("Recording end is unknown")
            } else {
                None
            };
            proto::RecordingDeletionObject {
                recording_id: recording.recording_id.clone(),
                start_ms: recording.started_at_ms,
                end_ms: recording.ended_at_ms,
                bytes: recording.catalog_bytes,
                active: !recording.finalized,
                protected: recording.protected,
                retention_pending: recording.cleanup_pending,
                status: proto::RecordingDeletionStatus::Blocked as i32,
                error: error.map(str::to_owned),
            }
        })
        .collect();
    result
}

pub(super) fn job(
    job: &jobs::Job,
    report: Option<&execution::Report>,
    include_objects: bool,
) -> proto::RecordingDeletionJob {
    let status = match job.state {
        jobs::State::Prepared => proto::RecordingDeletionStatus::Prepared,
        jobs::State::Expired => proto::RecordingDeletionStatus::Expired,
        jobs::State::Cancelled => proto::RecordingDeletionStatus::Cancelled,
        jobs::State::Queued => proto::RecordingDeletionStatus::Queued,
    };
    let objects = if include_objects {
        objects(job, report, status)
    } else {
        Vec::new()
    };
    let status = report.map_or(status, |report| job_status(job, report, status));
    proto::RecordingDeletionJob {
        job_id: job.id.clone(),
        scope: Some(wire_scope(&job.snapshot.scope)),
        reason: match job.reason {
            jobs::Reason::Operator => proto::RecordingDeletionReason::Operator,
            jobs::Reason::Privacy => proto::RecordingDeletionReason::Privacy,
        } as i32,
        revision: job.revision,
        status: status as i32,
        objects,
        bytes: job.snapshot.catalog_bytes,
        created_at_ms: job.created_at_ms,
        expires_at_ms: job.expires_at_ms,
        confirmation_nonce: job
            .confirmation
            .as_ref()
            .map(|nonce| nonce.as_str().to_owned()),
        required_confirmation_text: confirmation_text(job.snapshot.recordings.len()),
        gaps: job
            .snapshot
            .evidence
            .gaps
            .iter()
            .map(|range| proto::RecordingMaintenanceRange {
                start_ms: range.start_ms,
                end_ms: range.end_ms,
            })
            .collect(),
        bookmark_event_ids: job.snapshot.evidence.bookmarks.clone(),
        related_export_ids: job.export_ids.clone(),
        consequences: vec![
            "Selected whole recordings and their playback indexes are removed permanently."
                .to_owned(),
            "Events and bookmarks remain; existing export artifacts are not deleted.".to_owned(),
        ],
        deleted_count: report.map_or(0, |report| report.deleted),
        failed_count: report.map_or(0, |report| report.failed),
        cancelled: report.is_some_and(|report| report.cancelled),
    }
}

fn objects(
    job: &jobs::Job,
    report: Option<&execution::Report>,
    status: proto::RecordingDeletionStatus,
) -> Vec<proto::RecordingDeletionObject> {
    job.snapshot
        .recordings
        .iter()
        .map(|recording| {
            let result = report.and_then(|report| {
                report
                    .objects
                    .iter()
                    .find(|object| object.recording_id == recording.recording_id)
            });
            proto::RecordingDeletionObject {
                recording_id: recording.recording_id.clone(),
                start_ms: recording.started_at_ms,
                end_ms: recording.ended_at_ms,
                bytes: recording.catalog_bytes,
                status: result.map_or(status, |result| object_status(result.status)) as i32,
                error: result.and_then(|result| result.error.clone()),
                active: !recording.finalized,
                protected: recording.protected,
                retention_pending: recording.cleanup_pending,
            }
        })
        .collect()
}

fn job_status(
    job: &jobs::Job,
    report: &execution::Report,
    pending: proto::RecordingDeletionStatus,
) -> proto::RecordingDeletionStatus {
    if report.deleted as usize == job.snapshot.recordings.len() {
        proto::RecordingDeletionStatus::Deleted
    } else if report.cancelled {
        proto::RecordingDeletionStatus::Cancelled
    } else if report.failed > 0 {
        proto::RecordingDeletionStatus::Failed
    } else if report.objects.iter().any(|object| {
        matches!(
            object.status,
            execution::Status::Working | execution::Status::Staged
        )
    }) {
        proto::RecordingDeletionStatus::Working
    } else {
        pending
    }
}

const fn object_status(status: execution::Status) -> proto::RecordingDeletionStatus {
    match status {
        execution::Status::Reserved => proto::RecordingDeletionStatus::Queued,
        execution::Status::Working => proto::RecordingDeletionStatus::Working,
        execution::Status::Staged => proto::RecordingDeletionStatus::Staged,
        execution::Status::Deleted => proto::RecordingDeletionStatus::Deleted,
        execution::Status::Failed => proto::RecordingDeletionStatus::Failed,
        execution::Status::Cancelled => proto::RecordingDeletionStatus::Cancelled,
    }
}
