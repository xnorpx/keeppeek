use super::{
    ApiPrincipal, ControlCommandError, ServerControlHandler, ServerState, event_search_catalog,
};
use crate::{
    api::proto,
    storage::catalog::maintenance::{self, jobs},
};
use prost::Message as _;

mod audit;
mod mapping;
pub(super) mod reconciliation;
mod worker;

pub(super) fn dispatch(
    handler: &ServerControlHandler,
    session_id: crate::webrtc::SessionId,
    principal: &ApiPrincipal,
    command: proto::RecordingMaintenanceCommand,
) -> Result<proto::ok::Result, ControlCommandError> {
    let state = &handler.state;
    if principal.role != crate::access::AccessRole::Administrator {
        return Err(error(
            proto::ErrorCode::Rejected,
            403,
            "Administrator access is required",
        ));
    }
    let catalog = event_search_catalog(state)?;
    let actor = principal.id();
    match command.action {
        Some(proto::recording_maintenance_command::Action::InspectCatalog(_)) => {
            reconciliation::inspect(state, principal)
        }
        Some(proto::recording_maintenance_command::Action::ApplyRemedy(request)) => {
            reconciliation::apply(state, principal, request)
        }
        Some(proto::recording_maintenance_command::Action::Preview(request)) => {
            preview(state, &actor, request)
        }
        Some(proto::recording_maintenance_command::Action::Get(request)) => {
            get(state, &actor, &request.job_id)
        }
        Some(proto::recording_maintenance_command::Action::Cancel(request)) => {
            catalog
                .recording_deletion_intent(
                    &actor,
                    jobs::Action::Cancel {
                        id: request.job_id.clone(),
                    },
                )
                .map_err(catalog_error)?;
            get(state, &actor, &request.job_id)
        }
        Some(proto::recording_maintenance_command::Action::List(request)) => {
            list(state, &actor, &request.after_job_id)
        }
        Some(proto::recording_maintenance_command::Action::Confirm(request)) => {
            confirm(handler, session_id, principal, request)
        }
        Some(proto::recording_maintenance_command::Action::Retry(request)) => {
            let permit = worker::admit(state)?;
            get(state, &actor, &request.job_id)?;
            worker::start(
                handler,
                session_id,
                principal.clone(),
                request.job_id.clone(),
                permit,
            )?;
            get(state, &actor, &request.job_id)
        }
        None => Err(error(
            proto::ErrorCode::InvalidRequest,
            400,
            "maintenance action is required",
        )),
    }
}

fn list(
    state: &ServerState,
    actor: &str,
    after: &str,
) -> Result<proto::ok::Result, ControlCommandError> {
    let catalog = event_search_catalog(state)?;
    let history = catalog
        .recording_deletion_jobs(actor, after)
        .map_err(catalog_error)?;
    let next_job_id = if history.len() == 16 {
        history.last().map(|job| job.id.clone()).unwrap_or_default()
    } else {
        String::new()
    };
    let jobs = history
        .into_iter()
        .map(|job| {
            let report = catalog
                .recording_deletion_progress(actor, &job.id)
                .map_err(catalog_error)?;
            Ok(mapping::job(&job, Some(&report), false))
        })
        .collect::<Result<Vec<_>, ControlCommandError>>()?;
    let result = proto::RecordingDeletionJobList { jobs, next_job_id };
    if result.encoded_len() > 48 * 1024 {
        return Err(error(
            proto::ErrorCode::Rejected,
            413,
            "maintenance history exceeds response limit",
        ));
    }
    Ok(proto::ok::Result::RecordingDeletionJobs(result))
}

fn confirm(
    handler: &ServerControlHandler,
    session_id: crate::webrtc::SessionId,
    principal: &ApiPrincipal,
    request: proto::ConfirmRecordingDeletion,
) -> Result<proto::ok::Result, ControlCommandError> {
    let state = &handler.state;
    let actor = principal.id();
    let catalog = event_search_catalog(state)?;
    let permit = worker::admit(state)?;
    let job = catalog
        .recording_deletion_intent(
            &actor,
            jobs::Action::Read {
                id: request.job_id.clone(),
            },
        )
        .map_err(catalog_error)?;
    if job.state == jobs::State::Prepared
        && related_exports(state, &job.snapshot)? != job.export_ids
    {
        return Err(error(
            proto::ErrorCode::Rejected,
            409,
            "export relationships changed; preview again",
        ));
    }
    if request.confirmation_text != mapping::confirmation_text(job.snapshot.recordings.len()) {
        return Err(error(
            proto::ErrorCode::InvalidRequest,
            400,
            "confirmation text does not match the preview",
        ));
    }
    let nonce = jobs::Nonce::parse(&request.confirmation_nonce)
        .map_err(|error| catalog_error(error.into()))?;
    catalog
        .recording_deletion_intent(
            &actor,
            jobs::Action::Confirm {
                id: request.job_id.clone(),
                nonce,
                expected_revision: request.expected_revision,
            },
        )
        .map_err(catalog_error)?;
    worker::start(
        handler,
        session_id,
        principal.clone(),
        request.job_id.clone(),
        permit,
    )?;
    get(state, &actor, &request.job_id)
}

fn preview(
    state: &ServerState,
    actor: &str,
    request: proto::PreviewRecordingDeletion,
) -> Result<proto::ok::Result, ControlCommandError> {
    let scope = mapping::scope(request.scope.ok_or_else(|| {
        error(
            proto::ErrorCode::InvalidRequest,
            400,
            "recording scope is required",
        )
    })?)?;
    let reason = match proto::RecordingDeletionReason::try_from(request.reason) {
        Ok(proto::RecordingDeletionReason::Operator) => jobs::Reason::Operator,
        Ok(proto::RecordingDeletionReason::Privacy) => jobs::Reason::Privacy,
        _ => {
            return Err(error(
                proto::ErrorCode::InvalidRequest,
                400,
                "deletion reason is required",
            ));
        }
    };
    let catalog = event_search_catalog(state)?;
    let snapshot = catalog
        .recording_maintenance_snapshot(scope.clone())
        .map_err(catalog_error)?;
    let exports = related_exports(state, &snapshot)?;
    if snapshot.recordings.iter().any(|recording| {
        !recording.finalized
            || recording.protected
            || recording.cleanup_pending
            || recording.file_identity.is_none()
            || recording.ended_at_ms.is_none()
    }) {
        return bounded(mapping::blocked(&snapshot, reason, exports));
    }
    let mut job = catalog
        .recording_deletion_intent(
            actor,
            jobs::Action::Prepare(jobs::Intent {
                scope,
                reason,
                expected_revision: snapshot.revision,
            }),
        )
        .map_err(catalog_error)?;
    let nonce = job.confirmation.take();
    job = catalog
        .recording_deletion_intent(
            actor,
            jobs::Action::BindExports {
                id: job.id,
                export_ids: exports,
            },
        )
        .map_err(catalog_error)?;
    job.confirmation = nonce;
    bounded(mapping::job(&job, None, true))
}

fn related_exports(
    state: &ServerState,
    snapshot: &maintenance::Snapshot,
) -> Result<Vec<String>, ControlCommandError> {
    let (source, stream) = match &snapshot.scope {
        maintenance::Scope::Recording {
            source_id,
            stream_id,
            ..
        }
        | maintenance::Scope::TimeRange {
            source_id,
            stream_id,
            ..
        } => (source_id, stream_id),
    };
    let jobs = state.export_jobs.lock().unwrap();
    let mut ids = Vec::new();
    for record in jobs
        .values()
        .filter(|record| record.request.source_id == *source && record.request.stream_id == *stream)
    {
        let start =
            super::required_timestamp_ms(record.request.start_time.as_ref(), "export start")?;
        let end = super::required_timestamp_ms(record.request.end_time.as_ref(), "export end")?;
        if snapshot.recordings.iter().any(|recording| {
            recording.started_at_ms < end && recording.ended_at_ms.is_none_or(|stop| stop > start)
        }) {
            if ids.len() >= 128 {
                return Err(error(
                    proto::ErrorCode::Rejected,
                    413,
                    "export relationships exceed preview limit",
                ));
            }
            ids.push(record.job.job_id.clone());
        }
    }
    ids.sort_unstable();
    Ok(ids)
}

fn get(
    state: &ServerState,
    actor: &str,
    id: &str,
) -> Result<proto::ok::Result, ControlCommandError> {
    let catalog = event_search_catalog(state)?;
    let job = catalog
        .recording_deletion_intent(actor, jobs::Action::Read { id: id.to_owned() })
        .map_err(catalog_error)?;
    let report = catalog
        .recording_deletion_progress(actor, id)
        .map_err(catalog_error)?;
    bounded(mapping::job(&job, Some(&report), true))
}

fn bounded(job: proto::RecordingDeletionJob) -> Result<proto::ok::Result, ControlCommandError> {
    if job.encoded_len() > 48 * 1024 {
        return Err(error(
            proto::ErrorCode::Rejected,
            413,
            "maintenance preview exceeds response limit",
        ));
    }
    Ok(proto::ok::Result::RecordingDeletionJob(job))
}

fn catalog_error(failure: anyhow::Error) -> ControlCommandError {
    let failure = failure
        .downcast_ref::<jobs::Failure>()
        .copied()
        .unwrap_or(jobs::Failure::Unavailable);
    let code = match failure {
        jobs::Failure::Invalid => 400,
        jobs::Failure::NotFound => 404,
        jobs::Failure::Quota => 429,
        jobs::Failure::Unavailable => 503,
        _ => 409,
    };
    error(proto::ErrorCode::Rejected, code, &failure.to_string())
}

fn error(code: proto::ErrorCode, status: u16, message: &str) -> ControlCommandError {
    ControlCommandError::new(code, status, message)
}

#[cfg(test)]
mod tests;
