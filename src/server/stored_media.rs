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
    token: uuid::Uuid,
}

impl OpenCursorReservation<'_> {
    fn claim(
        &mut self,
        reservations: &mut std::collections::HashMap<(SessionId, String), uuid::Uuid>,
    ) -> Result<(SessionId, String), ControlCommandError> {
        let key = self
            .key
            .as_ref()
            .expect("cursor reservation must have a key");
        if reservations.get(key) != Some(&self.token) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "stored media cursor was closed or replaced while opening",
            ));
        }
        let key = self
            .key
            .take()
            .expect("validated cursor reservation has a key");
        reservations.remove(&key);
        Ok(key)
    }

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
        let key = self.claim(&mut reservations)?;
        if self
            .state
            .maintenance_active
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "recording maintenance is running",
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
        let mut reservations = self
            .state
            .stored_media_cursor_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if reservations.get(&key) == Some(&self.token) {
            reservations.remove(&key);
        }
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
    if state
        .maintenance_active
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "recording maintenance is running",
        ));
    }
    validate_open_cursor(
        cursors.keys().chain(reservations.keys()),
        session_id,
        cursor_id,
    )?;
    let token = uuid::Uuid::new_v4();
    let previous = reservations.insert(key.clone(), token);
    assert!(
        previous.is_none(),
        "stored media cursor reservation must be unique"
    );
    Ok(OpenCursorReservation {
        state,
        key: Some(key),
        token,
    })
}

pub(super) fn dispatch(
    state: &ServerState,
    session_id: SessionId,
    command: proto::StoredMediaCommand,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    match command.action {
        Some(stored_media_command::Action::Open(open)) => open_cursor(state, session_id, open),
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
        Some(stored_media_command::Action::Close(close)) => close_cursor(state, session_id, close),
        Some(stored_media_command::Action::QueryTimeline(query)) => {
            query_timeline(state, session_id, query)
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

fn close_cursor(
    state: &ServerState,
    session_id: SessionId,
    close: proto::CloseStoredMedia,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    let key = (session_id, close.stored_media_id);
    let removed = state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&key);
    let pending = state
        .stored_media_cursor_reservations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&key);
    if removed.is_none() && pending.is_none() {
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

fn open_cursor(
    state: &ServerState,
    session_id: SessionId,
    open: proto::OpenStoredMedia,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    validate_client_id(&open.stored_media_id, "stored media cursor ID")?;
    let reservation = super::session_lifecycle::admit(state, session_id, || {
        reserve_open_cursor(state, session_id, &open.stored_media_id)
    })?;
    let (cursor, state_message, messages) = open_stored_media(state, open)?;
    reservation.commit(cursor)?;
    let notifications = terminal_stored_media_notification(&state_message);
    Ok(StoredMediaDispatch {
        result: Some(control_ok::Result::StoredMediaState(state_message)),
        messages,
        notifications,
    })
}

fn query_timeline(
    state: &ServerState,
    session_id: SessionId,
    query: proto::QueryStoredMediaTimeline,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    let access = super::camera_access::for_session(state, session_id)?;
    let (delivery, messages) = query_stored_media_timeline(state, query, &access)?;
    Ok(StoredMediaDispatch {
        result: Some(control_ok::Result::StoredMediaQueryDelivery(delivery)),
        messages,
        notifications: Vec::new(),
    })
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
    reservations.retain(|(owner_session_id, _), _| *owner_session_id != session_id);
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

    #[test]
    fn close_then_reopen_rejects_the_old_reservation_token() {
        let state = ServerState::empty();
        let owner = SessionId::from_u64(1);
        let mut first = reserve_open_cursor(&state, owner, "playback").unwrap();
        dispatch(
            &state,
            owner,
            proto::StoredMediaCommand {
                action: Some(stored_media_command::Action::Close(
                    proto::CloseStoredMedia {
                        stored_media_id: "playback".into(),
                    },
                )),
            },
        )
        .unwrap();
        let mut second = reserve_open_cursor(&state, owner, "playback").unwrap();
        let mut reservations = state.stored_media_cursor_reservations.lock().unwrap();
        assert_eq!(
            first.claim(&mut reservations).unwrap_err().code,
            proto::ErrorCode::Rejected
        );
        let key = second.claim(&mut reservations).unwrap();
        assert_eq!(key, (owner, "playback".into()));
        assert!(reservations.is_empty());
    }

    #[test]
    fn cancelled_open_drop_cannot_remove_a_new_reservation() {
        let state = ServerState::empty();
        let owner = SessionId::from_u64(1);
        let first = reserve_open_cursor(&state, owner, "playback").unwrap();
        dispatch(
            &state,
            owner,
            proto::StoredMediaCommand {
                action: Some(stored_media_command::Action::Close(
                    proto::CloseStoredMedia {
                        stored_media_id: "playback".into(),
                    },
                )),
            },
        )
        .unwrap();
        let second = reserve_open_cursor(&state, owner, "playback").unwrap();
        drop(first);
        assert_eq!(
            state.stored_media_cursor_reservations.lock().unwrap().len(),
            1
        );
        drop(second);
        assert!(
            state
                .stored_media_cursor_reservations
                .lock()
                .unwrap()
                .is_empty()
        );
    }
}
