use super::{
    ControlCommandError, ServerState, StoredMediaDispatch, open_stored_media,
    query_stored_media_timeline, refill_stored_media, seek_stored_media, set_stored_media_playback,
    terminal_stored_media_notification, validate_client_id,
};
use crate::{
    api::proto::{self, ok as control_ok, stored_media_command},
    webrtc::SessionId,
};

const MAX_CURSORS_PER_SESSION: usize = 16;
const MAX_CURSORS: usize = 1_024;

fn validate_open_cursor<'a>(
    keys: impl IntoIterator<Item = &'a (SessionId, String)>,
    session_id: SessionId,
    cursor_id: &str,
) -> Result<(), ControlCommandError> {
    let mut total = 0usize;
    let mut session_total = 0usize;
    for (owner, existing_id) in keys {
        total += 1;
        if *owner == session_id {
            if existing_id == cursor_id {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    409,
                    "stored media cursor ID is already active or opening on this connection",
                ));
            }
            session_total += 1;
        }
    }
    if session_total >= MAX_CURSORS_PER_SESSION || total >= MAX_CURSORS {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "too many stored media cursors are active",
        ));
    }
    Ok(())
}

struct OpenCursorReservation<'a> {
    state: &'a ServerState,
    key: Option<(SessionId, String)>,
}

impl OpenCursorReservation<'_> {
    fn commit(mut self, cursor: super::StoredMediaCursor) -> Result<(), ControlCommandError> {
        let mut cursors = self
            .state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut reservations = self
            .state
            .stored_media_cursor_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = self
            .key
            .take()
            .expect("stored media cursor reservation must have a key");
        if !reservations.remove(&key) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "stored media session closed while the cursor was opening",
            ));
        }
        cursors.insert(key, cursor);
        Ok(())
    }
}

impl Drop for OpenCursorReservation<'_> {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        self.state
            .stored_media_cursor_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
    }
}

fn reserve_open_cursor<'a>(
    state: &'a ServerState,
    session_id: SessionId,
    cursor_id: &str,
) -> Result<OpenCursorReservation<'a>, ControlCommandError> {
    let key = (session_id, cursor_id.to_owned());
    let cursors = state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut reservations = state
        .stored_media_cursor_reservations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    validate_open_cursor(
        cursors.keys().chain(reservations.iter()),
        session_id,
        cursor_id,
    )?;
    let inserted = reservations.insert(key.clone());
    assert!(inserted, "stored media cursor reservation must be unique");
    Ok(OpenCursorReservation {
        state,
        key: Some(key),
    })
}

pub(super) fn dispatch(
    state: &ServerState,
    session_id: SessionId,
    command: proto::StoredMediaCommand,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    match command.action {
        Some(stored_media_command::Action::Open(open)) => {
            validate_client_id(&open.stored_media_id, "stored media cursor ID")?;
            let reservation = reserve_open_cursor(state, session_id, &open.stored_media_id)?;
            let (cursor, state_message, messages) = open_stored_media(state, open)?;
            reservation.commit(cursor)?;
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
    let mut cursors = state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut reservations = state
        .stored_media_cursor_reservations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cursors.retain(|(owner_session_id, _), _| *owner_session_id != session_id);
    reservations.retain(|(owner_session_id, _)| *owner_session_id != session_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn cursor_admission_rejects_duplicates_and_per_session_overflow() {
        let owner = SessionId::from_u64(1);
        let other = SessionId::from_u64(2);
        let mut keys = HashSet::new();
        keys.insert((owner, "duplicate".to_owned()));

        let duplicate = validate_open_cursor(keys.iter(), owner, "duplicate").unwrap_err();
        assert_eq!(duplicate.code, proto::ErrorCode::Rejected);

        for index in 1..MAX_CURSORS_PER_SESSION {
            keys.insert((owner, format!("cursor-{index}")));
        }
        keys.insert((other, "other-session".to_owned()));

        let overflow = validate_open_cursor(keys.iter(), owner, "overflow").unwrap_err();
        assert_eq!(overflow.code, proto::ErrorCode::Rejected);
        assert!(validate_open_cursor(keys.iter(), other, "second").is_ok());
    }

    #[test]
    fn cursor_reservations_block_duplicate_and_excess_open_work() {
        let state = ServerState::empty();
        let owner = SessionId::from_u64(1);
        let mut reservations = vec![reserve_open_cursor(&state, owner, "duplicate").unwrap()];

        let Err(duplicate) = reserve_open_cursor(&state, owner, "duplicate") else {
            panic!("duplicate cursor reservation unexpectedly succeeded");
        };
        assert_eq!(duplicate.code, proto::ErrorCode::Rejected);

        for index in 1..MAX_CURSORS_PER_SESSION {
            reservations
                .push(reserve_open_cursor(&state, owner, &format!("cursor-{index}")).unwrap());
        }
        let Err(overflow) = reserve_open_cursor(&state, owner, "overflow") else {
            panic!("excess cursor reservation unexpectedly succeeded");
        };
        assert_eq!(overflow.code, proto::ErrorCode::Rejected);

        drop(reservations);
        assert!(reserve_open_cursor(&state, owner, "duplicate").is_ok());
    }

    #[test]
    fn closing_a_session_releases_its_pending_cursor_reservations() {
        let state = ServerState::empty();
        let owner = SessionId::from_u64(1);
        let reservation = reserve_open_cursor(&state, owner, "opening").unwrap();

        close_session(&state, owner);

        assert!(
            state
                .stored_media_cursor_reservations
                .lock()
                .unwrap()
                .is_empty()
        );
        drop(reservation);
    }
}
