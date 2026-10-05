use super::*;
use crate::storage::volumes::runtime::management::MovePreview;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(in crate::server) struct Registry {
    pub(super) metadata: super::metadata::Registry,
    plans: Mutex<HashMap<String, Plan>>,
}

#[derive(Clone)]
struct Plan {
    actor: String,
    revision: String,
    expires: Instant,
    preview: MovePreview,
}

pub(super) fn preview(
    state: &ServerState,
    actor: &str,
    request: proto::PreviewStorageMove,
) -> Result<proto::StorageMovePreview> {
    let _config = state.config_update.try_lock().map_err(|_| {
        error(
            proto::ErrorCode::Rejected,
            409,
            "configuration is changing; retry the preview",
        )
    })?;
    let revision = camera_configuration_revision(state)?;
    let object = object(request.object.ok_or_else(|| {
        error(
            proto::ErrorCode::InvalidRequest,
            400,
            "storage object is required",
        )
    })?)?;
    let role = super::super::role_from_wire(request.role).map_err(failure)?;
    let source_id = source(state, &object)?;
    let camera = state.camera(&source_id).ok_or_else(|| {
        error(
            proto::ErrorCode::NotFound,
            404,
            "object source is no longer configured",
        )
    })?;
    let groups = camera.groups.iter().map(String::as_str).collect::<Vec<_>>();
    let preview = manager(state)?
        .preview_move(
            object,
            &request.destination_volume_id,
            &PlacementRequest {
                role,
                source: &source_id,
                group: "",
                required_bytes: 1,
            },
            &groups,
        )
        .map_err(failure)?;
    if revision != camera_configuration_revision(state)? {
        return Err(error(
            proto::ErrorCode::Rejected,
            409,
            "configuration changed; preview again",
        ));
    }
    store_preview(state, actor, revision, preview)
}

fn store_preview(
    state: &ServerState,
    actor: &str,
    revision: String,
    preview: MovePreview,
) -> Result<proto::StorageMovePreview> {
    let token = uuid::Uuid::new_v4().to_string();
    let result = proto::StorageMovePreview {
        preview_token: token.clone(),
        configuration_revision: revision.clone(),
        job_id: token.clone(),
        source: Some(location(preview.source())),
        destination_volume_id: preview.destination().to_owned(),
        expires_in_seconds: 300,
        adopts_legacy: preview.adopts_legacy(),
    };
    let mut plans = state.volume_previews.plans.lock().map_err(|_| {
        error(
            proto::ErrorCode::Unavailable,
            503,
            "volume previews are unavailable",
        )
    })?;
    plans.retain(|_, plan| plan.expires > Instant::now());
    if plans.len() >= 64 {
        return Err(error(
            proto::ErrorCode::Rejected,
            429,
            "too many pending volume previews",
        ));
    }
    plans.insert(
        token,
        Plan {
            actor: actor.to_owned(),
            revision,
            expires: Instant::now() + Duration::from_secs(300),
            preview,
        },
    );
    Ok(result)
}

fn source(state: &ServerState, object: &locations::Object) -> Result<String> {
    if object.kind == locations::Kind::Export {
        let jobs = state.export_jobs.lock().map_err(|_| {
            error(
                proto::ErrorCode::Unavailable,
                503,
                "export history is unavailable",
            )
        })?;
        return jobs
            .values()
            .find(|job| job.artifact_id == object.id)
            .map(|job| job.request.source_id.clone())
            .ok_or_else(|| {
                error(
                    proto::ErrorCode::NotFound,
                    404,
                    "export owner is unavailable",
                )
            });
    }
    match catalog(state, Request::ObjectSource(object.clone()))? {
        Reply::ObjectSource(Some(source)) => Ok(source),
        _ => Err(error(
            proto::ErrorCode::NotFound,
            404,
            "object owner is unavailable",
        )),
    }
}

pub(super) fn confirm(
    state: &ServerState,
    actor: &str,
    request: proto::ConfirmStorageMove,
) -> Result<proto::StorageMoveJob> {
    let _config = state.config_update.try_lock().map_err(|_| {
        error(
            proto::ErrorCode::Unavailable,
            503,
            "configuration is unavailable",
        )
    })?;
    let plan = load_plan(state, actor, &request.preview_token)?;
    if plan.revision != request.expected_configuration_revision
        || plan.revision != camera_configuration_revision(state)?
    {
        return Err(error(
            proto::ErrorCode::Rejected,
            409,
            "configuration changed; preview again",
        ));
    }
    let worker = state.storage_config.volume_mover.as_ref().ok_or_else(|| {
        error(
            proto::ErrorCode::Unavailable,
            503,
            "volume worker is unavailable",
        )
    })?;
    manager(state)?
        .admit_move(&request.preview_token, &plan.preview)
        .map_err(failure)?;
    // Admission is durable. A failed wakeup leaves the same job for the next worker scan.
    if let Err(cause) = worker.scan() {
        tracing::warn!(%cause, "confirmed move awaits volume worker recovery");
    }
    get(state, &request.preview_token)
}

fn load_plan(state: &ServerState, actor: &str, token: &str) -> Result<Plan> {
    state
        .volume_previews
        .plans
        .lock()
        .map_err(|_| {
            error(
                proto::ErrorCode::Unavailable,
                503,
                "volume previews are unavailable",
            )
        })?
        .get(token)
        .filter(|plan| plan.actor == actor && plan.expires > Instant::now())
        .cloned()
        .ok_or_else(|| {
            error(
                proto::ErrorCode::Rejected,
                409,
                "volume preview expired; preview again",
            )
        })
}

fn job(value: &locations::moves::Job) -> proto::StorageMoveJob {
    proto::StorageMoveJob {
        job_id: value.id.clone(),
        source: Some(location(&value.source)),
        destination_volume_id: value.destination.volume.clone(),
        phase: value.phase.clone(),
        cancellation_requested: value.cancellation_requested,
    }
}

pub(super) fn get(state: &ServerState, id: &str) -> Result<proto::StorageMoveJob> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(error(
            proto::ErrorCode::InvalidRequest,
            400,
            "invalid move job ID",
        ));
    }
    match catalog(state, Request::FindMove(id.to_owned()))? {
        Reply::OptionalMove(Some(value)) => Ok(job(&value)),
        _ => Err(error(
            proto::ErrorCode::NotFound,
            404,
            "move job is unavailable",
        )),
    }
}

pub(super) fn list(
    state: &ServerState,
    request: proto::ListStorageMoves,
) -> Result<proto::StorageMoveList> {
    let after = (!request.after_job_id.is_empty()).then_some(request.after_job_id);
    let Reply::Moves(jobs) = catalog(
        state,
        Request::Moves(locations::moves::Page {
            after,
            limit: 16,
            include_terminal: true,
        }),
    )?
    else {
        unreachable!("move page reply")
    };
    let next_after_job_id = if jobs.len() == 16 {
        jobs.last().expect("full page").id.clone()
    } else {
        String::new()
    };
    Ok(proto::StorageMoveList {
        jobs: jobs.iter().map(job).collect(),
        next_after_job_id,
    })
}

pub(super) fn cancel(state: &ServerState, id: &str) -> Result<proto::StorageMoveJob> {
    get(state, id)?;
    catalog(
        state,
        Request::AdvanceMove(locations::moves::Step::Cancel(id.to_owned())),
    )?;
    if let Some(worker) = &state.storage_config.volume_mover {
        let _ = worker.scan();
    }
    get(state, id)
}
