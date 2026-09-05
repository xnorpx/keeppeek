use super::{
    ApiPrincipal, CameraEntry, ControlCommandError, ServerControlHandler, ServerState,
    proto_camera_source_session, unix_time_ms,
};
use crate::{
    access::{AccessRole, CameraAccess, MAX_CAMERA_ACCESS_IDS},
    api::proto::{
        self, camera_control_command, event_search_command, request as control_request,
        stored_media_command,
    },
    webrtc::SessionId,
};

pub(super) fn for_principal(
    state: &ServerState,
    principal: &ApiPrincipal,
) -> Result<CameraAccess, ControlCommandError> {
    let policy = match principal.credential_binding() {
        Some((id, revision)) => state
            .access_manager
            .camera_access(
                id,
                revision,
                i64::try_from(unix_time_ms()).unwrap_or(i64::MAX),
            )
            .ok_or_else(denied),
        None if principal.is_local() && principal.role == AccessRole::Administrator => {
            Ok(CameraAccess::unrestricted())
        }
        None => Err(denied()),
    }?;
    Ok(resolve_groups(state, policy))
}

fn resolve_groups(state: &ServerState, mut policy: CameraAccess) -> CameraAccess {
    if policy.all_cameras || policy.group_ids.is_empty() {
        return policy;
    }
    let mut allowed = policy
        .camera_ids
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    for camera in state.camera_entries() {
        if camera
            .groups
            .iter()
            .any(|group| policy.group_ids.contains(group))
        {
            allowed.insert(camera.info.id);
        }
    }
    policy.group_ids.clear();
    policy.camera_ids = allowed.into_iter().collect();
    policy
}

pub(super) fn invalidate_group_sessions(state: &ServerState, groups: &[String]) {
    let sessions = state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|(session_id, session)| (*session_id, session.principal.clone()))
        .collect::<Vec<_>>();
    let now = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    for (session_id, principal) in sessions {
        let Some((id, revision)) = principal.credential_binding() else {
            continue;
        };
        let Some(policy) = state.access_manager.camera_access(id, revision, now) else {
            continue;
        };
        if policy.group_ids.iter().any(|group| groups.contains(group)) {
            super::close_api_session(state, session_id);
            state.webrtc.request_api_session_close(session_id);
        }
    }
}

pub(super) fn visible_cameras(
    handler: &ServerControlHandler,
    session_id: SessionId,
) -> Option<Vec<CameraEntry>> {
    let principal = handler
        .authorize_api_session(session_id, AccessRole::User, "camera_capabilities")
        .ok()?;
    let policy = for_principal(&handler.state, &principal).ok()?;
    query_cameras(&handler.state, &policy, &[]).ok()
}

pub(super) fn query_cameras(
    state: &ServerState,
    policy: &CameraAccess,
    requested: &[String],
) -> Result<Vec<CameraEntry>, ControlCommandError> {
    if !policy.all_cameras {
        require_cameras(policy, requested)?;
    }
    Ok(state
        .camera_entries()
        .into_iter()
        .filter(|camera| {
            policy.allows(&camera.info.id)
                && (requested.is_empty() || requested.contains(&camera.info.id))
        })
        .collect())
}

pub(super) fn for_session(
    state: &ServerState,
    session_id: SessionId,
) -> Result<CameraAccess, ControlCommandError> {
    if session_id.as_u64() == 0 {
        return Ok(CameraAccess::unrestricted());
    }
    let principal = {
        let owners = state.api_session_owners.lock().unwrap();
        let session = owners.get(&session_id).ok_or_else(denied)?;
        let now = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        if now >= session.absolute_expires_at_ms
            || session.last_activity.elapsed() >= state.api_session_policy.idle_timeout
        {
            return Err(denied());
        }
        session.principal.clone()
    };
    for_principal(state, &principal)
}

pub(super) fn authorize_command(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: &proto::Request,
) -> Result<(), ControlCommandError> {
    let policy = for_principal(state, principal)?;
    if policy.all_cameras {
        return Ok(());
    }
    match &request.command {
        Some(control_request::Command::SubscribeMedia(subscribe)) => {
            authorize_subscription(state, &policy, subscribe)
        }
        Some(control_request::Command::SubscribeEvents(subscribe)) => {
            require_cameras(&policy, &subscribe.source_ids)
        }
        Some(control_request::Command::CameraControlCommand(command)) => {
            authorize_camera_control(&policy, command)
        }
        Some(control_request::Command::StoredMediaCommand(command)) => {
            authorize_stored_media(&policy, command)
        }
        Some(control_request::Command::EventSearchCommand(command)) => {
            authorize_event_search(&policy, command)
        }
        _ => Ok(()),
    }
}

fn authorize_subscription(
    state: &ServerState,
    policy: &CameraAccess,
    request: &proto::SubscribeMedia,
) -> Result<(), ControlCommandError> {
    state
        .camera_entries()
        .iter()
        .any(|camera| {
            policy.allows(&camera.info.id)
                && proto_camera_source_session(&camera.info, &state.webrtc)
                    .is_some_and(|source| source.source_session_id == request.source_session_id)
        })
        .then_some(())
        .ok_or_else(denied)
}

pub(super) fn require_camera(
    policy: &CameraAccess,
    source_id: &str,
) -> Result<(), ControlCommandError> {
    policy.allows(source_id).then_some(()).ok_or_else(denied)
}

pub(super) fn require_cameras(
    policy: &CameraAccess,
    source_ids: &[String],
) -> Result<(), ControlCommandError> {
    if source_ids.len() > MAX_CAMERA_ACCESS_IDS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "too many camera source IDs",
        ));
    }
    source_ids
        .iter()
        .all(|source_id| policy.allows(source_id))
        .then_some(())
        .ok_or_else(denied)
}

fn authorize_camera_control(
    policy: &CameraAccess,
    command: &proto::CameraControlCommand,
) -> Result<(), ControlCommandError> {
    match &command.action {
        Some(camera_control_command::Action::Ptz(request)) => {
            require_camera(policy, &request.source_id)
        }
        Some(camera_control_command::Action::GetMotionDetection(request)) => {
            require_camera(policy, &request.source_id)
        }
        Some(camera_control_command::Action::SetMotionDetection(request)) => {
            require_camera(policy, &request.source_id)
        }
        Some(camera_control_command::Action::SetManufacturer(request)) => {
            require_camera(policy, &request.source_id)
        }
        None => Ok(()),
    }
}

fn authorize_stored_media(
    policy: &CameraAccess,
    command: &proto::StoredMediaCommand,
) -> Result<(), ControlCommandError> {
    match &command.action {
        Some(stored_media_command::Action::Open(request)) => {
            require_camera(policy, &request.source_id)
        }
        Some(stored_media_command::Action::QueryTimeline(request)) => {
            require_cameras(policy, &request.source_ids)
        }
        _ => Ok(()),
    }
}

fn authorize_event_search(
    policy: &CameraAccess,
    command: &proto::EventSearchCommand,
) -> Result<(), ControlCommandError> {
    match &command.action {
        Some(event_search_command::Action::Query(request)) => {
            authorize_event_query(policy, request)
        }
        Some(event_search_command::Action::FetchMedia(request)) => {
            if request.objects.len() > super::MAX_EVENT_SEARCH_MEDIA_OBJECTS {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "too many event media objects",
                ));
            }
            for object in &request.objects {
                require_camera(policy, &object.source_id)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(super) fn authorize_event_query(
    policy: &CameraAccess,
    request: &proto::QueryEvents,
) -> Result<(), ControlCommandError> {
    if let Some(source_id) = &request.source_id {
        require_camera(policy, source_id)?;
    }
    match &request.search {
        Some(proto::query_events::Search::Metadata(metadata)) => {
            require_cameras(policy, &metadata.source_ids)
        }
        Some(proto::query_events::Search::Text(_) | proto::query_events::Search::Semantic(_))
            if !policy.all_cameras && request.source_id.is_none() =>
        {
            Err(denied())
        }
        _ => Ok(()),
    }
}

pub(super) fn denied() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        403,
        "camera access is not permitted",
    )
}
