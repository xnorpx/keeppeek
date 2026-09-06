use std::collections::HashMap;
use std::sync::Arc;

use super::{CameraEntry, ControlCommandError, ServerState, proto, ptz_capability, unsupported};
use crate::server::{SessionId, control_ok};

pub(in crate::server) struct Owner {
    pub session_id: SessionId,
    camera: CameraEntry,
}

pub(in crate::server) fn handle_ptz(
    state: &ServerState,
    session_id: SessionId,
    command: proto::PtzCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    let action = command.action.ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "PTZ command has no action",
        )
    })?;
    let mut owners = state.ptz_owners.try_lock().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "another PTZ command is in progress",
        )
    })?;
    let camera = if matches!(action, proto::ptz_command::Action::Stop(_)) {
        owners
            .get(&command.source_id)
            .map(|owner| owner.camera.clone())
            .or_else(|| state.camera(&command.source_id))
    } else {
        state.camera(&command.source_id)
    }
    .ok_or_else(|| ControlCommandError::new(proto::ErrorCode::NotFound, 404, "camera not found"))?;
    if !matches!(action, proto::ptz_command::Action::Stop(_)) && !ptz_capability(&camera).supported
    {
        return Err(unsupported("camera does not report usable PTZ support"));
    }
    if !super::available(&camera) {
        return Err(unavailable("PTZ transport is unavailable"));
    }
    let presets = execute(&mut owners, &camera, session_id, action)?;
    Ok(control_ok::Result::PtzResult(proto::PtzResult {
        source_id: command.source_id,
        presets,
    }))
}

fn execute(
    owners: &mut HashMap<String, Owner>,
    camera: &CameraEntry,
    session_id: SessionId,
    action: proto::ptz_command::Action,
) -> Result<Vec<proto::PtzPreset>, ControlCommandError> {
    if matches!(action, proto::ptz_command::Action::ListPresets(_)) {
        return super::presets(camera);
    }
    if owners
        .get(&camera.info.id)
        .is_some_and(|owner| owner.session_id != session_id)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "camera PTZ is owned by another connection",
        ));
    }
    match action {
        proto::ptz_command::Action::Continuous(continuous) => {
            move_camera(owners, camera, session_id, &continuous)?;
        }
        proto::ptz_command::Action::Stop(_) => {
            super::stop(camera).map_err(|_| unavailable("camera PTZ stop failed"))?;
            owners.remove(&camera.info.id);
        }
        proto::ptz_command::Action::GotoPreset(goto) => {
            if owners.contains_key(&camera.info.id) {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    409,
                    "stop continuous PTZ movement before selecting a preset",
                ));
            }
            if goto.preset_id == 0 {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "PTZ preset ID must be nonzero",
                ));
            }
            if !ptz_capability(camera).presets {
                return Err(unsupported("camera does not report preset support"));
            }
            if super::goto_preset(camera, goto.preset_id).is_err() {
                owners.insert(
                    camera.info.id.clone(),
                    Owner {
                        session_id,
                        camera: camera.clone(),
                    },
                );
                safety_stop(owners, camera);
                return Err(unavailable(
                    "camera PTZ preset failed; its outcome may be unknown",
                ));
            }
        }
        _ => {
            return Err(unsupported(
                "PTZ action is not implemented by this camera transport",
            ));
        }
    }
    Ok(Vec::new())
}

fn move_camera(
    owners: &mut HashMap<String, Owner>,
    camera: &CameraEntry,
    session_id: SessionId,
    continuous: &proto::PtzContinuous,
) -> Result<(), ControlCommandError> {
    if owners
        .get(&camera.info.id)
        .is_some_and(|owner| !Arc::ptr_eq(&owner.camera.control_revision, &camera.control_revision))
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "stop the previous camera target before moving its replacement",
        ));
    }
    let movement = super::movement(camera, continuous)?;
    owners.insert(
        camera.info.id.clone(),
        Owner {
            session_id,
            camera: camera.clone(),
        },
    );
    if movement.send(camera).is_err() {
        safety_stop(owners, camera);
        return Err(unavailable(
            "camera PTZ movement failed; its outcome may be unknown",
        ));
    }
    Ok(())
}

fn safety_stop(owners: &mut HashMap<String, Owner>, camera: &CameraEntry) {
    if super::stop(camera).is_ok() {
        owners.remove(&camera.info.id);
    } else {
        tracing::warn!(source_id = %camera.info.id, "camera PTZ movement outcome and safety stop are unconfirmed");
    }
}

pub(in crate::server) fn close_session(state: &ServerState, session_id: SessionId) {
    let mut owners = state
        .ptz_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    owners.retain(|source_id, owner| {
        if owner.session_id != session_id {
            return true;
        }
        if super::stop(&owner.camera).is_ok() {
            return false;
        }
        tracing::warn!(%source_id, "unable to confirm stop of session-owned PTZ movement");
        true
    });
}

fn unavailable(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Unavailable, 502, message)
}
