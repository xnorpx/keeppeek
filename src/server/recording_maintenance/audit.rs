use super::{ApiPrincipal, ServerControlHandler, jobs};
use crate::storage::catalog::maintenance::jobs::execution::{Report, Status};
use crate::webrtc::SessionId;

pub(super) fn record(
    controller: &ServerControlHandler,
    session_id: SessionId,
    principal: &ApiPrincipal,
    job: &jobs::Job,
    result: &anyhow::Result<Report>,
) {
    emit(job, &principal.id(), result);
    super::super::record_access_audit(
        &controller.state,
        i64::try_from(super::super::unix_time_ms()).unwrap_or(i64::MAX),
        Some(&principal.id()),
        Some(principal.role),
        "recording_delete",
        Some(&job.id),
        outcome(result),
        controller.session_classification(session_id),
    );
}

fn emit(job: &jobs::Job, principal_id: &str, result: &anyhow::Result<Report>) {
    use crate::storage::catalog::maintenance::Scope;
    let (source_id, stream_id) = match &job.snapshot.scope {
        Scope::Recording {
            source_id,
            stream_id,
            ..
        }
        | Scope::TimeRange {
            source_id,
            stream_id,
            ..
        } => (source_id, stream_id),
    };
    let (requested_start_ms, requested_end_ms) = match job.snapshot.scope {
        Scope::Recording { .. } => (None, None),
        Scope::TimeRange {
            start_ms, end_ms, ..
        } => (Some(start_ms), Some(end_ms)),
    };
    let report = result.as_ref().ok();
    tracing::info!(
        event = "recording_maintenance",
        action = "recording_delete",
        principal_id,
        job_id = job.id,
        source_id,
        stream_id,
        reason = match job.reason {
            jobs::Reason::Operator => "operator",
            jobs::Reason::Privacy => "privacy",
        },
        preview_revision = job.revision,
        object_count = job.snapshot.recordings.len(),
        bytes = job.snapshot.catalog_bytes,
        start_ms = job
            .snapshot
            .recordings
            .iter()
            .map(|recording| recording.started_at_ms)
            .min(),
        end_ms = job
            .snapshot
            .recordings
            .iter()
            .filter_map(|recording| recording.ended_at_ms)
            .max(),
        requested_start_ms,
        requested_end_ms,
        bookmark_count = job.snapshot.evidence.bookmarks.len(),
        related_export_count = job.export_ids.len(),
        protected_count = job
            .snapshot
            .recordings
            .iter()
            .filter(|recording| recording.protected)
            .count(),
        hold_override = false,
        deleted_count = report.map(|report| report.deleted),
        failed_count = report.map(|report| report.failed),
        result = outcome(result),
    );
}

pub(super) fn outcome(result: &anyhow::Result<Report>) -> &'static str {
    let Ok(report) = result else {
        return "failed";
    };
    if !report.objects.is_empty()
        && report
            .objects
            .iter()
            .all(|object| object.status == Status::Deleted)
    {
        "success"
    } else if report.cancelled {
        if report
            .objects
            .iter()
            .all(|object| matches!(object.status, Status::Deleted | Status::Cancelled))
        {
            "cancelled"
        } else {
            "cancelled_with_pending"
        }
    } else if report.failed > 0 {
        "partial_failure"
    } else {
        "incomplete"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::catalog::maintenance::jobs::execution::Object;

    #[test]
    fn incomplete_reports_never_audit_success() {
        let report = Report {
            job_id: "job".to_owned(),
            objects: vec![Object {
                recording_id: "recording".to_owned(),
                status: Status::Reserved,
                changed_at_ms: None,
                error: None,
                staged_directory: None,
            }],
            deleted: 0,
            failed: 0,
            cancelled: false,
        };
        assert_eq!(outcome(&Ok(report.clone())), "incomplete");
        assert_eq!(
            outcome(&Ok(Report {
                cancelled: true,
                ..report
            })),
            "cancelled_with_pending"
        );
    }
}
