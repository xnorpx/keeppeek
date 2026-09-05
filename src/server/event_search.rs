use super::{
    ControlCommandError, ServerState, cancel_event_search_task, fetch_event_search_media,
    query_events, replace_event_search_terms, set_event_search_embedding, start_event_search_media,
    start_event_search_query, validate_client_id,
};
use crate::{
    api::proto::{self, event_search_command, ok as control_ok},
    webrtc::{OutboundDataMessage, SessionId},
};
use std::sync::atomic::Ordering;

pub(super) fn dispatch(
    state: &ServerState,
    session_id: SessionId,
    command: proto::EventSearchCommand,
) -> Result<(Option<control_ok::Result>, Vec<OutboundDataMessage>), ControlCommandError> {
    match command.action {
        Some(event_search_command::Action::ReplaceTerms(request)) => {
            replace_event_search_terms(state, &request)?;
            Ok((
                Some(control_ok::Result::EventSearchMutation(
                    proto::EventSearchMutationResult {
                        event_id: request.event_id,
                    },
                )),
                Vec::new(),
            ))
        }
        Some(event_search_command::Action::SetEmbedding(request)) => {
            set_event_search_embedding(state, &request)?;
            Ok((
                Some(control_ok::Result::EventSearchMutation(
                    proto::EventSearchMutationResult {
                        event_id: request.event_id,
                    },
                )),
                Vec::new(),
            ))
        }
        Some(event_search_command::Action::Query(request)) => query(state, session_id, request),
        Some(event_search_command::Action::FetchMedia(request)) => {
            if state.webrtc.has_api_session(session_id) {
                let delivery = start_event_search_media(state, session_id, request)?;
                return Ok((
                    Some(control_ok::Result::EventSearchMediaDelivery(delivery)),
                    Vec::new(),
                ));
            }
            let (delivery, messages) = fetch_event_search_media(state, request)?;
            Ok((
                Some(control_ok::Result::EventSearchMediaDelivery(delivery)),
                messages,
            ))
        }
        Some(event_search_command::Action::CancelQuery(request)) => {
            validate_client_id(&request.query_id, "event search query ID")?;
            cancel_event_search_task(
                state,
                session_id,
                &format!("event-search-query:{}", request.query_id),
            );
            Ok((None, Vec::new()))
        }
        Some(event_search_command::Action::CancelMedia(request)) => {
            validate_client_id(&request.transfer_id, "event search transfer ID")?;
            cancel_event_search_task(
                state,
                session_id,
                &format!("event-search-media:{}", request.transfer_id),
            );
            Ok((None, Vec::new()))
        }
        None => Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event search command has no action",
        )),
    }
}

fn query(
    state: &ServerState,
    session_id: SessionId,
    request: proto::QueryEvents,
) -> Result<(Option<control_ok::Result>, Vec<OutboundDataMessage>), ControlCommandError> {
    let access = super::camera_access::for_session(state, session_id)?;
    if state.webrtc.has_api_session(session_id) {
        let delivery = start_event_search_query(state, session_id, request, access)?;
        return Ok((
            Some(control_ok::Result::EventSearchDelivery(delivery)),
            Vec::new(),
        ));
    }
    let (delivery, messages) = query_events(state, request, &access)?;
    Ok((
        Some(control_ok::Result::EventSearchDelivery(delivery)),
        messages,
    ))
}

pub(super) fn close_session(state: &ServerState, session_id: SessionId) {
    let cancelled = state
        .event_search_tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|((owner_session_id, _), _)| *owner_session_id == session_id)
        .map(|(_, cancelled)| cancelled.clone())
        .collect::<Vec<_>>();
    for token in cancelled {
        token.store(true, Ordering::Release);
    }
}
