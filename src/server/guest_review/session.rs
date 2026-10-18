use super::super::{ServerState, authentication, sharing};
use super::{
    http::{
        COOKIE, audit, clear_cookie, control_failure, failure, handle_cookie, io_failure,
        reject_unread, session_id,
    },
    registry::RequestGuard,
};
use crate::shares::{BoundSession, SessionContext};
use rouille::{Request, Response};
use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exchange {
    token: String,
}

#[derive(Serialize)]
struct Ready {
    session_id: String,
    status: &'static str,
    display_name: String,
    start_ms: i64,
    end_ms: i64,
    expires_at_ms: i64,
    can_stream: bool,
    can_download: bool,
    bytes: u64,
    has_gaps: bool,
}

pub(super) fn exchange(
    request: &Request,
    state: &ServerState,
    context: &SessionContext,
    guard: RequestGuard,
) -> Response {
    exchange_inner(request, state, context, &guard).unwrap_or_else(|response| response)
}

fn exchange_inner(
    request: &Request,
    state: &ServerState,
    context: &SessionContext,
    guard: &RequestGuard,
) -> Result<Response, Response> {
    if request.raw_url().contains('?') {
        return Err(reject_unread(request, failure(400)));
    }
    let token = exchange_body(request, guard)?;
    let issued = sharing::with_guest_store(state, context, |store, clock| {
        store.exchange(&token, context, clock)
    })
    .map_err(control_failure)?;
    let id = issued.id;
    let handle = issued.handle.into_string();
    let result = (|| {
        let bound = sharing::with_guest_store(state, context, |store, clock| {
            store.inspect_session(&handle, Some(id), context, clock)
        })
        .map_err(control_failure)?;
        sharing::validate_guest(state, &bound.grant).map_err(|_| failure(401))?;
        let response = status_payload(state, bound);
        guard.check().map_err(io_failure)?;
        Ok(response.with_additional_header(
            "Set-Cookie",
            format!("{COOKIE}={handle}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=300"),
        ))
    })();
    if result.is_err() {
        retire(state, &handle, id, context);
    } else {
        audit(request, state, "guest_handoff", "success");
    }
    result
}

fn exchange_length(request: &Request) -> Result<usize, Response> {
    if authentication::header(request, "Content-Encoding", 64)?.is_some()
        || authentication::header(request, "Transfer-Encoding", 128)?.is_some()
    {
        return Err(failure(400));
    }
    let length = authentication::header(request, "Content-Length", 20)?
        .filter(|length| !length.is_empty() && length.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|length| length.parse::<usize>().ok())
        .ok_or_else(|| failure(400))?;
    if length == 0 || length > 256 {
        return Err(failure(if length > 256 { 413 } else { 400 }));
    }
    let content_type = authentication::header(request, "Content-Type", 128)?;
    if content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        != Some("application/json")
    {
        return Err(failure(415));
    }
    Ok(length)
}

fn exchange_body(request: &Request, guard: &RequestGuard) -> Result<String, Response> {
    let length = exchange_length(request).map_err(|response| reject_unread(request, response))?;
    let mut body = Vec::with_capacity(length);
    request
        .data()
        .ok_or_else(|| failure(400))?
        .take(257)
        .read_to_end(&mut body)
        .map_err(|_| reject_unread(request, failure(400)))?;
    guard.check().map_err(io_failure)?;
    if body.len() != length {
        return Err(reject_unread(request, failure(400)));
    }
    let exchange: Exchange = serde_json::from_slice(&body).map_err(|_| failure(400))?;
    Ok(exchange.token)
}

pub(super) fn status(
    request: &Request,
    state: &ServerState,
    context: &SessionContext,
    guard: RequestGuard,
) -> Response {
    let result = (|| {
        let id = session_id(request, false)?;
        let handle = handle_cookie(request)?;
        let bound = sharing::with_guest_store(state, context, |store, clock| {
            store.inspect_session(handle, id, context, clock)
        })
        .map_err(control_failure)?;
        guard.check().map_err(io_failure)?;
        Ok(status_payload(state, bound))
    })();
    result.unwrap_or_else(|response| response)
}

pub(super) fn end(
    request: &Request,
    state: &ServerState,
    context: &SessionContext,
    guard: RequestGuard,
) -> Response {
    let result = (|| {
        let id = session_id(request, true)?.ok_or_else(|| failure(400))?;
        let handle = handle_cookie(request)?;
        guard.check().map_err(io_failure)?;
        sharing::with_guest_store(state, context, |store, clock| {
            store.end_session(handle, id, context, clock)
        })
        .map_err(control_failure)?;
        state.guest_review.abort_session(id);
        audit(request, state, "guest_end", "success");
        Ok(clear_cookie(Response::empty_204()))
    })();
    result.unwrap_or_else(|response| response)
}

fn status_payload(state: &ServerState, bound: BoundSession) -> Response {
    let grant = &bound.grant;
    let terminal = if grant.revoked_at_ms.is_some() {
        Some("revoked")
    } else if !bound.media_active {
        Some("expired")
    } else {
        None
    };
    if let Some(status) = terminal {
        return terminal_status(bound.session_id, status);
    }
    let checked = match sharing::validate_guest(state, grant) {
        Ok(checked) => checked,
        Err(_) => return terminal_status(bound.session_id, "unavailable"),
    };
    if sharing::open_guest(state, &checked.binding).is_err() {
        return terminal_status(bound.session_id, "unavailable");
    }
    let can_stream = grant.intent.allow_stream
        && (bound.stream_committed
            || grant
                .intent
                .max_views
                .is_none_or(|limit| grant.views < limit));
    let can_download = grant.intent.allow_download
        && grant
            .intent
            .max_downloads
            .is_none_or(|limit| grant.downloads < limit);
    if !can_stream && !can_download {
        return terminal_status(bound.session_id, "limit_reached");
    }
    Response::json(&Ready {
        session_id: bound.session_id.to_string(),
        status: "ready",
        display_name: grant.intent.label.clone(),
        start_ms: grant.intent.binding.start_ms,
        end_ms: grant.intent.binding.end_ms,
        expires_at_ms: grant.intent.expires_at_ms,
        can_stream,
        can_download,
        bytes: grant.intent.binding.bytes,
        has_gaps: checked.has_gaps,
    })
}

fn terminal_status(id: uuid::Uuid, status: &'static str) -> Response {
    Response::json(&serde_json::json!({"session_id": id.to_string(), "status": status}))
}

fn retire(state: &ServerState, handle: &str, id: uuid::Uuid, context: &SessionContext) {
    if sharing::with_guest_store(state, context, |store, clock| {
        store.end_session(handle, id, context, clock)
    })
    .is_err_and(|error| error._http_status == 503)
    {
        tracing::warn!("guest failed handoff cleanup was unavailable");
    }
    state.guest_review.abort_session(id);
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
