//! Administrator operations over the volume catalog and its existing move worker.

use super::super::{
    ApiPrincipal, ControlCommandError, ServerState, camera_configuration_revision, current_config,
};
use crate::{
    api::proto,
    storage::{
        catalog::locations::{self, Reply, Request},
        volumes::{PlacementRequest, VolumeHealth, runtime::Manager},
    },
};
use prost::Message as _;
mod metadata;
mod moves;
mod names;
pub(in crate::server) use moves::Registry;
type Result<T> = std::result::Result<T, ControlCommandError>;
#[cfg(test)]
mod tests;

pub(in crate::server) fn dispatch(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::StorageVolumeCommand,
) -> Result<proto::ok::Result> {
    if principal.role != crate::access::AccessRole::Administrator {
        return Err(error(
            proto::ErrorCode::Rejected,
            403,
            "Administrator access is required",
        ));
    }
    use proto::storage_volume_command::Action;
    use proto::storage_volume_result::Result as Wire;
    let names = names::Names::load(state)?;
    let mut result = match names.resolve(command.action)? {
        Some(Action::PreviewMetadata(request)) => {
            Wire::MetadataPreview(metadata::preview(state, &principal.id(), request)?)
        }
        Some(Action::ConfirmMetadata(request)) => {
            Wire::Metadata(metadata::confirm(state, &principal.id(), request)?)
        }
        Some(Action::Metadata(_)) => Wire::Metadata(metadata::status(state)?),
        Some(Action::CancelMetadata(request)) => Wire::Metadata(metadata::cancel(state, request)?),
        Some(Action::List(_)) => Wire::Volumes(list(state)?),
        Some(Action::Probe(request)) => Wire::Probe(probe(state, &request.volume_id)?),
        Some(Action::Placement(request)) => Wire::Placement(placement(state, request)?),
        Some(Action::Objects(request)) => Wire::Objects(objects(state, request)?),
        Some(Action::PreviewMove(request)) => {
            Wire::Preview(moves::preview(state, &principal.id(), request)?)
        }
        Some(Action::ConfirmMove(request)) => {
            Wire::Job(moves::confirm(state, &principal.id(), request)?)
        }
        Some(Action::Moves(request)) => Wire::Jobs(moves::list(state, request)?),
        Some(Action::GetMove(request)) => Wire::Job(moves::get(state, &request.job_id)?),
        Some(Action::CancelMove(request)) => Wire::Job(moves::cancel(state, &request.job_id)?),
        Some(Action::SetDraining(request)) => Wire::Volumes(set_draining(state, request)?),
        None => {
            return Err(error(
                proto::ErrorCode::InvalidRequest,
                400,
                "volume action is required",
            ));
        }
    };
    names.response(&mut result);
    let result = proto::StorageVolumeResult {
        result: Some(result),
    };
    if result.encoded_len() > 48 * 1024 {
        return Err(error(
            proto::ErrorCode::Rejected,
            413,
            "volume response exceeds its limit",
        ));
    }
    Ok(proto::ok::Result::StorageVolumeResult(result))
}

fn manager(state: &ServerState) -> Result<&Manager> {
    let manager = state
        .storage_config
        .volume_runtime
        .as_deref()
        .ok_or_else(|| {
            error(
                proto::ErrorCode::Unavailable,
                503,
                "volume runtime is unavailable",
            )
        })?;
    if let Some(path) = &state.camera_config_path {
        let config = crate::config::load_config(path).map_err(failure)?;
        if config.storage.named_volumes.as_ref() != Some(manager.configuration()) {
            return Err(error(
                proto::ErrorCode::Rejected,
                409,
                "volume configuration has pending changes; restart before moving files",
            ));
        }
    }
    Ok(manager)
}

fn set_draining(
    state: &ServerState,
    request: proto::SetStorageVolumeDraining,
) -> Result<proto::StorageVolumeList> {
    let _config = state.config_update.try_lock().map_err(|_| {
        error(
            proto::ErrorCode::Rejected,
            409,
            "configuration is changing; refresh and retry",
        )
    })?;
    if request.expected_configuration_revision != camera_configuration_revision(state)? {
        return Err(error(
            proto::ErrorCode::Rejected,
            409,
            "configuration changed; refresh and retry",
        ));
    }
    manager(state)?
        .set_draining(&request.volume_id, request.draining)
        .map_err(failure)?;
    list(state)
}

fn catalog(state: &ServerState, request: Request) -> Result<Reply> {
    super::super::event_search_catalog(state)?
        .volume_location(request)
        .map_err(failure)
}

fn failure(cause: anyhow::Error) -> ControlCommandError {
    tracing::debug!(%cause, "storage volume operation refused");
    error(
        proto::ErrorCode::Rejected,
        409,
        "storage changed or is unavailable; refresh the preview and retry",
    )
}

fn error(code: proto::ErrorCode, status: u16, message: &str) -> ControlCommandError {
    ControlCommandError::new(code, status, message)
}

fn list(state: &ServerState) -> Result<proto::StorageVolumeList> {
    let configuration = current_config(state);
    let names = names::Names::load(state)?;
    let observations = state
        .storage_config
        .volume_runtime
        .as_ref()
        .map(|manager| manager.observations())
        .transpose()
        .map_err(failure)?
        .unwrap_or_default();
    let usage = if state.catalog.is_some() {
        match catalog(state, Request::Usage)? {
            Reply::Usage(usage) => usage,
            _ => unreachable!("usage reply"),
        }
    } else {
        vec![]
    };
    let volumes = configuration
        .storage
        .named_volumes
        .map_or_else(Vec::new, |config| {
            config
                .volumes
                .into_iter()
                .map(|volume| {
                    let private = names.private(&volume.id).unwrap_or(&volume.id);
                    let observation = observations.iter().find(|o| o.id.as_str() == private);
                    let usage = usage.iter().find(|u| u.volume == private);
                    let online = observation.is_some_and(|o| o.health == VolumeHealth::Online);
                    proto::StorageVolumeStatus {
                        volume_id: private.to_owned(),
                        online,
                        available_bytes: observation.filter(|_| online).map(|o| o.available_bytes),
                        owned_bytes: usage.map_or(0, |u| u.allocated_bytes),
                        reserved_bytes: usage.map_or(0, |u| u.reserved_bytes),
                        configured_draining: usage.map_or(
                            volume.state == crate::storage::volumes::VolumeState::Draining,
                            |u| u.configured_draining,
                        ),
                        operator_draining: usage.is_some_and(|u| u.operator_draining),
                    }
                })
                .collect()
        });
    Ok(proto::StorageVolumeList {
        configuration_revision: camera_configuration_revision(state)?,
        volumes,
        runtime_available: manager(state).is_ok(),
    })
}

fn probe(state: &ServerState, id: &str) -> Result<proto::StorageVolumeStatus> {
    crate::storage::volumes::VolumeId::parse(id).map_err(failure)?;
    let path = state.camera_config_path.as_deref().ok_or_else(|| {
        error(
            proto::ErrorCode::Unavailable,
            503,
            "configuration is unavailable",
        )
    })?;
    let config = crate::config::load_config(path).map_err(failure)?;
    let volume = config
        .storage
        .named_volumes
        .as_ref()
        .and_then(|c| c.volumes.iter().find(|v| v.id.as_str() == id))
        .ok_or_else(|| error(proto::ErrorCode::NotFound, 404, "volume is not configured"))?;
    let sample =
        crate::storage::volumes::root::Root::open(&volume.root).and_then(|root| root.capacity(0));
    let mut status = list(state)?
        .volumes
        .into_iter()
        .find(|v| v.volume_id == id)
        .unwrap_or_default();
    status.volume_id = id.to_owned();
    status.online = sample.is_ok();
    status.available_bytes = sample.ok().map(|s| s.available_bytes);
    Ok(status)
}

fn placement(
    state: &ServerState,
    request: proto::PreviewStoragePlacement,
) -> Result<proto::StoragePlacementResult> {
    let role = super::role_from_wire(request.role).map_err(failure)?;
    let camera = state
        .camera(&request.source_id)
        .ok_or_else(|| error(proto::ErrorCode::NotFound, 404, "source is not configured"))?;
    let groups = camera.groups.iter().map(String::as_str).collect::<Vec<_>>();
    let decision = manager(state)?
        .preview_placement(
            &PlacementRequest {
                role,
                source: &camera.info.id,
                group: "",
                required_bytes: request.bytes,
            },
            &groups,
        )
        .map_err(failure)?;
    Ok(proto::StoragePlacementResult {
        selected_volume_id: decision.selected.map(|id| id.to_string()),
        rejected: decision
            .rejected
            .into_iter()
            .map(|r| proto::StoragePlacementRejection {
                volume_id: r.id.to_string(),
                reason: format!("{:?}", r.reason),
            })
            .collect(),
    })
}

fn object(value: proto::StorageObject) -> Result<locations::Object> {
    let kind = match proto::StorageObjectKind::try_from(value.kind) {
        Ok(proto::StorageObjectKind::Recording) => locations::Kind::Recording,
        Ok(proto::StorageObjectKind::Export) => locations::Kind::Export,
        Ok(proto::StorageObjectKind::Thumbnail) => locations::Kind::Thumbnail,
        _ => {
            return Err(error(
                proto::ErrorCode::InvalidRequest,
                400,
                "invalid storage object kind",
            ));
        }
    };
    if uuid::Uuid::parse_str(&value.id).is_err() {
        return Err(error(
            proto::ErrorCode::InvalidRequest,
            400,
            "invalid storage object ID",
        ));
    }
    Ok(locations::Object { kind, id: value.id })
}

fn wire_object(value: &locations::Object) -> proto::StorageObject {
    let kind = match value.kind {
        locations::Kind::Recording => proto::StorageObjectKind::Recording,
        locations::Kind::Export => proto::StorageObjectKind::Export,
        locations::Kind::Thumbnail => proto::StorageObjectKind::Thumbnail,
    };
    proto::StorageObject {
        kind: kind as i32,
        id: value.id.clone(),
    }
}

fn location(value: &locations::Location) -> proto::StorageObjectLocation {
    proto::StorageObjectLocation {
        object: Some(wire_object(&value.object)),
        volume_id: value.volume.clone(),
        bytes: value.bytes,
        revision: value.revision,
    }
}

fn objects(
    state: &ServerState,
    request: proto::ListStorageObjects,
) -> Result<proto::StorageObjectList> {
    let after = request.after.map(object).transpose()?;
    let Reply::Objects(objects) = catalog(
        state,
        Request::Objects(locations::objects::Page {
            volume: request.volume_id,
            after,
            limit: 64,
        }),
    )?
    else {
        unreachable!("object page reply")
    };
    let next_after =
        (objects.len() == 64).then(|| wire_object(&objects.last().expect("full page").object));
    Ok(proto::StorageObjectList {
        objects: objects.iter().map(location).collect(),
        next_after,
    })
}
