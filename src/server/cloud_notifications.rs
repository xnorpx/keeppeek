//! Server control-plane dispatch for bounded Keeppeek Service mobile push signaling.
//!
//! Handles Administrator settings, status, and test commands alongside paired-mobile
//! resolution of opaque 16-byte `cloud_event_id` aliases into canonical still-image previews.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    access::AccessRole,
    api::proto::{self, cloud_notification_command, cloud_notification_result, ok as control_ok},
    config::{self, cloud_notifications::Settings},
    notifications::{
        cloud::CloudSignalRuntime,
        cloud_limits::{CloudFailureCode, CloudState},
    },
    webrtc::SessionId,
};

use super::{
    ApiPrincipal, ApiPrincipalIdentity, ControlCommandError, ServerState, camera_access,
    millis_timestamp, proto_event_attachment_descriptor, proto_event_image_availability,
};

/// Capability advertised only when the bounded cloud signaling runtime is available.
pub(super) const CAPABILITY_ID: &str = "keeppeek.cloud-notifications.v1";

/// Generic failure returned for all denied, disabled, unknown, expired, or wrong-NVR resolutions.
const GENERIC_PREVIEW_UNAVAILABLE: &str = "cloud notification preview is unavailable";

pub(super) const fn required_role(command: &proto::CloudNotificationCommand) -> AccessRole {
    match command.action {
        Some(cloud_notification_command::Action::Resolve(_)) => AccessRole::User,
        _ => AccessRole::Administrator,
    }
}

pub(super) const fn audit_operation(
    command: &proto::CloudNotificationCommand,
) -> Option<&'static str> {
    match command.action {
        Some(cloud_notification_command::Action::UpdateSettings(_)) => {
            Some("cloud_notification_settings_update")
        }
        Some(cloud_notification_command::Action::Test(_)) => Some("cloud_notification_test"),
        _ => None,
    }
}

pub(super) fn dispatch(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    command: proto::CloudNotificationCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    let cloud = state.cloud_notifications.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "cloud notifications runtime is unavailable",
        )
    })?;
    let now_ms = unix_time_ms();
    let result = match command.action {
        Some(cloud_notification_command::Action::GetSettings(_)) => {
            ensure_admin(principal)?;
            let settings = cloud.settings();
            cloud_notification_result::Result::Settings(proto_settings(&settings))
        }
        Some(cloud_notification_command::Action::UpdateSettings(request)) => {
            ensure_admin(principal)?;
            let updated = update_settings(state, cloud, &request, now_ms)?;
            cloud_notification_result::Result::Settings(proto_settings(&updated))
        }
        Some(cloud_notification_command::Action::GetStatus(_)) => {
            ensure_admin(principal)?;
            let status = cloud.status(now_ms);
            cloud_notification_result::Result::Status(proto_status(status))
        }
        Some(cloud_notification_command::Action::Test(_)) => {
            ensure_admin(principal)?;
            let test = cloud.send_test_signal(now_ms).map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::Internal,
                    500,
                    format!("cloud notification test failed: {error}"),
                )
            })?;
            cloud_notification_result::Result::Test(proto::CloudNotificationTestResult {
                accepted: test.accepted,
                next_eligible_at: test.next_eligible_at_ms.map(millis_timestamp),
                failure: proto_failure_code(test.failure) as i32,
            })
        }
        Some(cloud_notification_command::Action::Resolve(request)) => {
            let context = resolve_preview(state, cloud, session_id, principal, &request, now_ms)?;
            cloud_notification_result::Result::Context(context)
        }
        None => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "cloud notification command has no action",
            ));
        }
    };
    Ok(control_ok::Result::CloudNotificationResult(
        proto::CloudNotificationResult {
            result: Some(result),
        },
    ))
}

/// Checks whether a paired mobile principal is still allowed to fetch preview media bytes.
pub(super) fn enforce_mobile_media_preview_gate(
    state: &ServerState,
    principal: &ApiPrincipal,
) -> Result<(), ControlCommandError> {
    let ApiPrincipalIdentity::Credential { id, revision } = principal.identity else {
        return Ok(());
    };
    let now_ms = unix_time_ms();
    if state
        .access_manager
        .paired_mobile_nvr_binding(id, revision, now_ms)
        .is_none()
    {
        return Ok(());
    }
    let allowed = state.cloud_notifications.as_ref().is_some_and(|cloud| {
        let settings = cloud.settings();
        settings.enabled && settings.image_previews_enabled && settings.is_paired()
    });
    if !allowed {
        return Err(generic_preview_error());
    }
    Ok(())
}

fn ensure_admin(principal: &ApiPrincipal) -> Result<(), ControlCommandError> {
    if principal.role == AccessRole::Administrator {
        Ok(())
    } else {
        Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "Administrator role is required",
        ))
    }
}

fn update_settings(
    state: &ServerState,
    cloud: &CloudSignalRuntime,
    request: &proto::UpdateCloudNotificationSettings,
    now_ms: i64,
) -> Result<Settings, ControlCommandError> {
    let config_path = state.camera_config_path.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "cloud notification configuration storage is unavailable",
        )
    })?;
    let _config_update = state
        .config_update
        .lock()
        .expect("configuration update mutex must not be poisoned");
    let current = cloud.settings();
    if request.expected_revision != current.revision {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            format!(
                "cloud notification settings revision conflict (current {})",
                current.revision
            ),
        ));
    }
    let next_revision = current.revision.checked_add(1).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "cloud notification settings revision is exhausted",
        )
    })?;
    let next = Settings {
        revision: next_revision,
        enabled: request.enabled,
        image_previews_enabled: request.image_previews_enabled,
        ..current
    };
    next.validate().map_err(|error| {
        ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string())
    })?;
    persist_settings_to_config(config_path, &next)?;
    cloud
        .apply_settings(next.clone(), now_ms)
        .map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to apply cloud notification settings: {error}"),
            )
        })?;
    Ok(next)
}

fn persist_settings_to_config(
    config_path: &std::path::Path,
    next: &Settings,
) -> Result<(), ControlCommandError> {
    let mut root = if config_path.exists() {
        config::load_configuration_table(config_path).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to read configuration: {error}"),
            )
        })?
    } else {
        toml::Table::new()
    };
    let serialized = toml::Value::try_from(next).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to encode cloud notification settings: {error}"),
        )
    })?;
    root.insert("cloud_notifications".to_owned(), serialized);
    config::write_configuration_table(config_path, &root).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to persist cloud notification settings: {error}"),
        )
    })
}

fn resolve_preview(
    state: &ServerState,
    cloud: &CloudSignalRuntime,
    session_id: SessionId,
    principal: &ApiPrincipal,
    request: &proto::ResolveCloudNotification,
    now_ms: i64,
) -> Result<proto::CloudNotificationContext, ControlCommandError> {
    let cloud_event_id: [u8; 16] = request.cloud_event_id.as_slice().try_into().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "cloud_event_id must be exactly 16 bytes",
        )
    })?;
    authorize_paired_mobile_preview(state, cloud, principal, now_ms)?;
    let alias = cloud
        .resolve_alias(&cloud_event_id, now_ms)
        .map_err(|_| generic_preview_error())?
        .ok_or_else(generic_preview_error)?;
    let access =
        camera_access::for_session(state, session_id).map_err(|_| generic_preview_error())?;
    if !access.allows(&alias.source_id) {
        return Err(generic_preview_error());
    }
    let event_store = state.events.as_ref().ok_or_else(generic_preview_error)?;
    let event = event_store
        .event_by_id(&alias.event_id)
        .ok()
        .flatten()
        .ok_or_else(generic_preview_error)?;
    if event.camera_id != alias.source_id {
        return Err(generic_preview_error());
    }
    let supported_canonical = event
        .canonical_attachment()
        .filter(|attachment| is_still_image_attachment(attachment))
        .cloned();
    let image_file_present = supported_canonical.is_some()
        && event_store
            .thumbnail_path(&event.camera_id, &event.id)
            .ok()
            .flatten()
            .is_some();
    let canonical_attachment = supported_canonical.map(proto_event_attachment_descriptor);
    let image_availability =
        proto_event_image_availability(canonical_attachment.is_some(), image_file_present);
    Ok(proto::CloudNotificationContext {
        cloud_event_id: cloud_event_id.to_vec(),
        event_id: event.id,
        revision: event.revision,
        canonical_attachment,
        image_availability,
    })
}

fn authorize_paired_mobile_preview(
    state: &ServerState,
    cloud: &CloudSignalRuntime,
    principal: &ApiPrincipal,
    now_ms: i64,
) -> Result<(), ControlCommandError> {
    let ApiPrincipalIdentity::Credential { id, revision } = principal.identity else {
        return Err(generic_preview_error());
    };
    let bound_nvr_id = state
        .access_manager
        .paired_mobile_nvr_binding(id, revision, now_ms)
        .ok_or_else(generic_preview_error)?;
    let settings = cloud.settings();
    if !settings.enabled || !settings.image_previews_enabled {
        return Err(generic_preview_error());
    }
    let pairing = settings
        .pairing_reference()
        .ok()
        .flatten()
        .ok_or_else(generic_preview_error)?;
    if pairing.nvr_id_canonical != bound_nvr_id {
        return Err(generic_preview_error());
    }
    Ok(())
}

/// Accepts only static still images; rejects video, audio, and animated image formats.
fn is_still_image_attachment(attachment: &crate::storage::metadata::EventAttachment) -> bool {
    crate::storage::metadata::is_supported_event_image(attachment)
        && matches!(attachment.content_type.as_str(), "image/jpeg" | "image/png")
}

fn generic_preview_error() -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::NotFound, 404, GENERIC_PREVIEW_UNAVAILABLE)
}

const fn proto_settings(settings: &Settings) -> proto::CloudNotificationSettings {
    proto::CloudNotificationSettings {
        revision: settings.revision,
        enabled: settings.enabled,
        image_previews_enabled: settings.image_previews_enabled,
    }
}

fn proto_status(
    status: crate::notifications::cloud_state::CloudStatusSnapshot,
) -> proto::CloudNotificationStatus {
    proto::CloudNotificationStatus {
        state: proto_state(status.state) as i32,
        paired: status.paired,
        pending: status.pending,
        accepted_last_hour: status.accepted_last_hour,
        accepted_last_day: status.accepted_last_day,
        suppressed_last_hour: status.suppressed_last_hour,
        suppressed_last_day: status.suppressed_last_day,
        next_eligible_at: status.next_eligible_at_ms.map(millis_timestamp),
        last_accepted_at: status.last_accepted_at_ms.map(millis_timestamp),
        last_failure: proto_failure_code(status.last_failure) as i32,
    }
}

const fn proto_state(state: CloudState) -> proto::CloudNotificationState {
    match state {
        CloudState::Disabled => proto::CloudNotificationState::Disabled,
        CloudState::Unpaired => proto::CloudNotificationState::Unpaired,
        CloudState::Ready => proto::CloudNotificationState::Ready,
        CloudState::Throttled => proto::CloudNotificationState::Throttled,
        CloudState::BackingOff => proto::CloudNotificationState::BackingOff,
        CloudState::Unavailable => proto::CloudNotificationState::Unavailable,
    }
}

const fn proto_failure_code(code: CloudFailureCode) -> proto::CloudNotificationFailureCode {
    match code {
        CloudFailureCode::None => proto::CloudNotificationFailureCode::None,
        CloudFailureCode::LocalLimit => proto::CloudNotificationFailureCode::LocalLimit,
        CloudFailureCode::ServiceLimit => proto::CloudNotificationFailureCode::ServiceLimit,
        CloudFailureCode::Dependency => proto::CloudNotificationFailureCode::Dependency,
        CloudFailureCode::CredentialRejected => {
            proto::CloudNotificationFailureCode::CredentialRejected
        }
        CloudFailureCode::Integrity => proto::CloudNotificationFailureCode::Integrity,
        CloudFailureCode::Expired => proto::CloudNotificationFailureCode::Expired,
    }
}

fn unix_time_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "cloud_notifications_tests.rs"]
mod tests;
