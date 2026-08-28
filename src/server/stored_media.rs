use super::{
    ControlCommandError, ServerState, StoredMediaDispatch, open_stored_media,
    query_stored_media_timeline, refill_stored_media, seek_stored_media, set_stored_media_playback,
    terminal_stored_media_notification,
};
use crate::{
    api::proto::{self, ok as control_ok, stored_media_command},
    webrtc::SessionId,
};

pub(super) fn dispatch(
    state: &ServerState,
    session_id: SessionId,
    command: proto::StoredMediaCommand,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    match command.action {
        Some(stored_media_command::Action::Open(open)) => {
            let (cursor, state_message, messages) = open_stored_media(state, open)?;
            let key = (session_id, state_message.stored_media_id.clone());
            let mut cursors = state
                .stored_media_cursors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cursors.contains_key(&key) {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    409,
                    "stored media cursor ID is already active on this connection",
                ));
            }
            cursors.insert(key, cursor);
            let notifications = terminal_stored_media_notification(&state_message);
            Ok(StoredMediaDispatch {
                result: Some(control_ok::Result::StoredMediaState(state_message)),
                messages,
                notifications,
            })
        }
        Some(stored_media_command::Action::Seek(seek)) => {
            let (state_message, messages) = seek_stored_media(state, session_id, seek)?;
            let notifications = terminal_stored_media_notification(&state_message);
            Ok(StoredMediaDispatch {
                result: Some(control_ok::Result::StoredMediaState(state_message)),
                messages,
                notifications,
            })
        }
        Some(stored_media_command::Action::Refill(refill)) => {
            let (state_message, messages) = refill_stored_media(state, session_id, refill)?;
            let notifications = terminal_stored_media_notification(&state_message);
            Ok(StoredMediaDispatch {
                result: Some(control_ok::Result::StoredMediaState(state_message)),
                messages,
                notifications,
            })
        }
        Some(stored_media_command::Action::SetPlayback(update)) => {
            let (state_message, messages) = set_stored_media_playback(state, session_id, update)?;
            Ok(StoredMediaDispatch {
                result: Some(control_ok::Result::StoredMediaState(state_message)),
                messages,
                notifications: Vec::new(),
            })
        }
        Some(stored_media_command::Action::Close(close)) => {
            let removed = state
                .stored_media_cursors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&(session_id, close.stored_media_id));
            if removed.is_none() {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::NotFound,
                    404,
                    "stored media cursor was not found",
                ));
            }
            Ok(StoredMediaDispatch {
                result: None,
                messages: Vec::new(),
                notifications: Vec::new(),
            })
        }
        Some(stored_media_command::Action::QueryTimeline(query)) => {
            let (delivery, messages) = query_stored_media_timeline(state, query)?;
            Ok(StoredMediaDispatch {
                result: Some(control_ok::Result::StoredMediaQueryDelivery(delivery)),
                messages,
                notifications: Vec::new(),
            })
        }
        Some(stored_media_command::Action::CancelTimelineQuery(_)) => Ok(StoredMediaDispatch {
            result: None,
            messages: Vec::new(),
            notifications: Vec::new(),
        }),
        None => Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored media command has no action",
        )),
    }
}

pub(super) fn close_session(state: &ServerState, session_id: SessionId) {
    state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(owner_session_id, _), _| *owner_session_id != session_id);
}
