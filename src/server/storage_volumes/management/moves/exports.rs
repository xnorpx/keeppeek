//! Keeps export history authoritative during legacy adoption.

use super::*;
use crate::{
    server::ExportJobRecord,
    storage::catalog::locations::legacy::{inventory, roots},
};

pub(super) fn owner(
    state: &ServerState,
    object: &locations::Object,
) -> Result<Option<ExportJobRecord>> {
    if object.kind != locations::Kind::Export
        || matches!(
            catalog(state, Request::Lookup(object.clone()))?,
            Reply::Location(Some(_))
        )
    {
        return Ok(None);
    }
    let jobs = state.export_jobs.lock().map_err(|_| unavailable())?;
    let owner = jobs
        .values()
        .find(|job| job.artifact_id == object.id)
        .ok_or_else(unavailable)?;
    ready(owner)?;
    Ok(Some(owner.clone()))
}

fn ready(owner: &ExportJobRecord) -> Result<()> {
    let valid = owner.job.status == proto::ExportJobStatus::Ready as i32
        && !owner.cancel.load(std::sync::atomic::Ordering::Acquire)
        && owner.job.bytes_written > 0
        && owner.job.sha256.is_some()
        && !owner
            .job
            .expires_at
            .as_ref()
            .and_then(crate::server::timestamp_ms)
            .is_some_and(|expires| {
                i128::from(expires) <= i128::from(crate::server::unix_time_ms())
            })
        && crate::server::safe_export_job_id(&owner.job.job_id)
        && crate::server::safe_export_job_id(&owner.artifact_id)
        && owner
            .job
            .file_name
            .as_deref()
            .is_some_and(crate::server::safe_export_path_component);
    if valid { Ok(()) } else { Err(unavailable()) }
}

pub(super) fn preview(
    state: &ServerState,
    owner: &ExportJobRecord,
    destination: &str,
    request: &PlacementRequest<'_>,
    groups: &[&str],
) -> Result<MovePreview> {
    let Reply::LegacyRoot(roots::State::Bound(binding)) =
        catalog(state, Request::LegacyRoot(roots::Role::Export))?
    else {
        return Err(unavailable());
    };
    let reference = inventory::Reference {
        object: locations::Object {
            kind: locations::Kind::Export,
            id: owner.artifact_id.clone(),
        },
        path: binding
            .root
            .join(&owner.job.job_id)
            .join(&owner.artifact_id)
            .join(
                owner
                    .job
                    .file_name
                    .as_deref()
                    .expect("validated export filename"),
            ),
        revision: 1,
        evidence: None,
    };
    let preview = manager(state)?
        .preview_legacy_export(reference, destination, request, groups)
        .map_err(failure)?;
    if preview.source().bytes != owner.job.bytes_written
        || owner.job.sha256.as_deref()
            != Some(&crate::server::encode_lower_hex(preview.source().digest))
    {
        return Err(unavailable());
    }
    Ok(preview)
}

pub(super) fn admit(
    state: &ServerState,
    id: &str,
    preview: &MovePreview,
    owner: Option<&ExportJobRecord>,
) -> Result<()> {
    if matches!(
        catalog(state, Request::FindMove(id.into()))?,
        Reply::OptionalMove(Some(_))
    ) {
        return manager(state)?.admit_move(id, preview).map_err(failure);
    }
    let Some(owner) = owner else {
        return manager(state)?.admit_move(id, preview).map_err(failure);
    };
    // ponytail: the bounded export history lock covers owner validation and durable admission.
    let jobs = state.export_jobs.lock().map_err(|_| unavailable())?;
    let current = jobs.get(&owner.job.job_id).ok_or_else(unavailable)?;
    ready(current)?;
    if current.artifact_id != owner.artifact_id
        || current.job != owner.job
        || current.request != owner.request
    {
        return Err(unavailable());
    }
    manager(state)?.admit_move(id, preview).map_err(failure)
}

fn unavailable() -> ControlCommandError {
    error(
        proto::ErrorCode::Rejected,
        409,
        "export changed or is unavailable; preview again",
    )
}
