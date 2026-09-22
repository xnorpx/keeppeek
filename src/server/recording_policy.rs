//! Administrator recording controls backed by the storage admission authority.

use super::{ApiPrincipal, ControlCommandError, ServerState};
use crate::{
    api::proto,
    storage::recording_control::{Override, Reason, Revision, Snapshot, Source},
};

pub(super) const CAPABILITY: &str = "keeppeek.recording-control.v1";

pub(super) fn dispatch_control(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::request::Command,
) -> Result<proto::ok::Result, ControlCommandError> {
    match command {
        proto::request::Command::RecordingPolicyCommand(command) => {
            dispatch(state, principal, command)
        }
        proto::request::Command::PreservationCommand(command) => {
            super::preservation::dispatch(state, principal, command)
        }
        _ => unreachable!("only recording policy commands reach this dispatcher"),
    }
}

pub(super) const fn sensitive_operation(command: &proto::request::Command) -> Option<&'static str> {
    match command {
        proto::request::Command::RecordingPolicyCommand(command) => match command.action {
            Some(proto::recording_policy_command::Action::SetOverride(_)) => {
                Some("recording_override_set")
            }
            Some(proto::recording_policy_command::Action::ClearOverride(_)) => {
                Some("recording_override_clear")
            }
            _ => None,
        },
        proto::request::Command::PreservationCommand(command) => match command.action {
            Some(proto::preservation_command::Action::SaveForever(_)) => Some("preservation_save"),
            Some(proto::preservation_command::Action::Release(_)) => Some("preservation_release"),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn configure(state: &ServerState, camera: &crate::cameras::CameraConfig) {
    if let Some(storage) = &state.recording_control {
        storage.configure_camera_recording(
            &camera.ip.to_string(),
            camera.recording_mode,
            std::time::Duration::from_secs(camera.event_recording_duration_secs),
        );
    }
}

pub(super) fn dispatch(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::RecordingPolicyCommand,
) -> Result<proto::ok::Result, ControlCommandError> {
    if principal.role != crate::access::AccessRole::Administrator {
        return Err(error(403, "Administrator access is required"));
    }
    let storage = state
        .recording_control
        .as_ref()
        .ok_or_else(|| error(503, "recording controls are unavailable"))?;
    if command.source_id.is_empty() || command.source_id.len() > 128 {
        return Err(error(400, "invalid recording source"));
    }
    let snapshot = match command.action {
        Some(proto::recording_policy_command::Action::Get(_)) => {
            storage.recording_control(&command.source_id)
        }
        Some(proto::recording_policy_command::Action::SetOverride(request)) => {
            let revision = revision(&request.expected_revision)?;
            let source = match proto::RecordingOverrideSource::try_from(request.source) {
                Ok(proto::RecordingOverrideSource::Manual) => Source::Manual,
                Ok(proto::RecordingOverrideSource::External) => Source::External,
                _ => return Err(error(400, "recording override source is required")),
            };
            if request.reason.trim().is_empty()
                || request.reason.len() > 256
                || !(1..=86_400_000).contains(&request.ttl_ms)
            {
                return Err(error(
                    400,
                    "recording override requires a reason and TTL of 1..86400000 ms",
                ));
            }
            storage.set_recording_override(
                &command.source_id,
                revision,
                Override {
                    enabled: request.enabled,
                    source,
                    actor: principal.id(),
                    reason: request.reason,
                    ttl_ms: request.ttl_ms,
                },
            )
        }
        Some(proto::recording_policy_command::Action::ClearOverride(request)) => storage
            .clear_recording_override(&command.source_id, revision(&request.expected_revision)?),
        None => return Err(error(400, "recording control action is required")),
    }
    .map_err(|_| {
        error(
            409,
            "recording control changed or is bounded/unavailable; reload current state",
        )
    })?;
    Ok(proto::ok::Result::RecordingControlState(map_snapshot(
        command.source_id,
        snapshot,
    )))
}

fn revision(value: &str) -> Result<Revision, ControlCommandError> {
    let (epoch, sequence) = value
        .split_once(':')
        .ok_or_else(|| error(400, "invalid recording revision"))?;
    if epoch.len() != 32
        || sequence.is_empty()
        || sequence.len() > 20
        || !epoch.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !sequence.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(error(400, "invalid recording revision"));
    }
    Ok(Revision {
        epoch: u128::from_str_radix(epoch, 16)
            .map_err(|_| error(400, "invalid recording revision"))?,
        sequence: sequence
            .parse()
            .map_err(|_| error(400, "invalid recording revision"))?,
    })
}

fn map_snapshot(source_id: String, value: Snapshot) -> proto::RecordingControlState {
    let reason = match value.reason {
        Reason::Configuration => proto::RecordingControlReason::Configuration,
        Reason::ConfiguredDisabled => proto::RecordingControlReason::ConfiguredDisabled,
        Reason::Privacy => proto::RecordingControlReason::Privacy,
        Reason::PrivacyUnavailable => proto::RecordingControlReason::PrivacyUnavailable,
        Reason::Override => proto::RecordingControlReason::Override,
        Reason::Expired => proto::RecordingControlReason::Expired,
        Reason::ClockUnavailable => proto::RecordingControlReason::ClockUnavailable,
    };
    let override_state = value
        .request
        .zip(value.expires_at_ms)
        .map(|(request, expires_at_ms)| proto::RecordingOverrideState {
            enabled: request.enabled,
            source: match request.source {
                Source::Manual => proto::RecordingOverrideSource::Manual as i32,
                Source::External => proto::RecordingOverrideSource::External as i32,
            },
            actor: request.actor,
            reason: request.reason,
            expires_at_ms,
        });
    proto::RecordingControlState {
        source_id,
        revision: format!("{:032x}:{}", value.revision.epoch, value.revision.sequence),
        configured_mode: mode(value.configured_mode) as i32,
        effective_mode: mode(value.mode) as i32,
        reason: reason as i32,
        override_state,
    }
}

const fn mode(value: crate::cameras::CameraRecordingMode) -> proto::CameraRecordingMode {
    use crate::cameras::CameraRecordingMode;
    match value {
        CameraRecordingMode::Off => proto::CameraRecordingMode::Off,
        CameraRecordingMode::Sub => proto::CameraRecordingMode::Sub,
        CameraRecordingMode::Main => proto::CameraRecordingMode::Main,
        CameraRecordingMode::Both => proto::CameraRecordingMode::Both,
        CameraRecordingMode::EventBoost => proto::CameraRecordingMode::EventBoost,
    }
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
#[path = "../../tests/server/recording_policy.rs"]
mod tests;
