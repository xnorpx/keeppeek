//! Administrator-owned, indefinite recording preservation through catalog holds.

use super::{ApiPrincipal, ControlCommandError, ServerState, event_search_catalog};
use crate::{
    api::proto,
    storage::{catalog::holds, long_term::inspection::Archive},
};

pub(super) const CAPABILITY: &str = "keeppeek.recording-preservation.v1";
const RECORDING_MARKER: &str = "operator:recording:v1";

pub(super) fn dispatch(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::PreservationCommand,
) -> Result<proto::ok::Result, ControlCommandError> {
    if principal.role != crate::access::AccessRole::Administrator {
        return Err(error(403, "Administrator access is required"));
    }
    let target = command
        .target
        .ok_or_else(|| error(400, "preservation target is required"))?;
    let id = match &target.target {
        Some(proto::preservation_target::Target::RecordingId(id)) => id,
        Some(proto::preservation_target::Target::EventId(_)) => {
            return Err(error(503, "event preservation is unavailable"));
        }
        None => return Err(error(400, "preservation target is required")),
    };
    validate_text(id, 128)?;
    let catalog = event_search_catalog(state)?;
    match command.action {
        Some(proto::preservation_command::Action::Get(_)) => {}
        Some(proto::preservation_command::Action::SaveForever(request)) => {
            mutate(catalog, principal, id, request, true)?;
        }
        Some(proto::preservation_command::Action::Release(request)) => {
            mutate(catalog, principal, id, request, false)?;
        }
        None => return Err(error(400, "preservation action is required")),
    }
    // ponytail: Reuse the catalog hold and archive inspector; event projection remains separate.
    let archive = Archive::open(&state.storage_config.long_term_path).ok();
    let inspection = catalog
        .inspect_recording_hold(id, RECORDING_MARKER, archive.as_ref())
        .map_err(|_| {
            error(
                409,
                "preservation state unavailable; reload before retrying",
            )
        })?;
    Ok(proto::ok::Result::PreservationState(map_state(
        target, inspection,
    )))
}

fn mutate(
    catalog: &crate::storage::catalog::RecordingCatalogHandle,
    principal: &ApiPrincipal,
    id: &str,
    request: proto::MutatePreservation,
    active: bool,
) -> Result<(), ControlCommandError> {
    let revision = request
        .expected_revision
        .ok_or_else(|| error(400, "expected revision is required"))?;
    if revision > i64::MAX as u64 {
        return Err(error(400, "invalid preservation revision"));
    }
    validate_text(&request.reason, 256)?;
    catalog
        .update_recording_hold(
            id,
            RECORDING_MARKER,
            holds::Update {
                expected_revision: (revision != 0).then_some(revision),
                active,
                actor: principal.id(),
                reason: request.reason,
            },
        )
        .map_err(|_| {
            error(
                409,
                "preservation changed or recording unavailable; reload before retrying",
            )
        })?;
    Ok(())
}

fn map_state(
    target: proto::PreservationTarget,
    state: holds::Inspection,
) -> proto::PreservationState {
    let mut gaps = Vec::with_capacity(2);
    if !state.eligible {
        gaps.push(proto::PreservationGapReason::RecordingIneligible as i32);
    }
    if !state.media_available {
        gaps.push(proto::PreservationGapReason::MediaUnavailable as i32);
    }
    let covered = gaps.is_empty() && state.protected;
    let coverage = if !gaps.is_empty() {
        proto::PreservationCoverage::Unavailable
    } else if covered {
        proto::PreservationCoverage::Protected
    } else {
        proto::PreservationCoverage::Unprotected
    };
    let hold = state.hold;
    proto::PreservationState {
        target: Some(target),
        revision: hold.as_ref().map_or(0, |hold| hold.revision),
        marker_active: hold.as_ref().is_some_and(|hold| hold.active),
        actor: hold
            .as_ref()
            .map_or_else(String::new, |hold| hold.actor.clone()),
        reason: hold.map_or_else(String::new, |hold| hold.reason),
        coverage: coverage as i32,
        protected_objects: u64::from(covered),
        protected_bytes: if covered { state.bytes } else { 0 },
        gaps,
        independently_protected: state.independently_protected,
    }
}

fn validate_text(value: &str, limit: usize) -> Result<(), ControlCommandError> {
    if value.len() > limit || value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(error(400, "invalid preservation text"));
    }
    Ok(())
}

fn error(status: u16, message: &str) -> ControlCommandError {
    ControlCommandError::new(
        if status == 400 {
            proto::ErrorCode::InvalidRequest
        } else {
            proto::ErrorCode::Rejected
        },
        status,
        message,
    )
}

#[cfg(test)]
mod tests;
