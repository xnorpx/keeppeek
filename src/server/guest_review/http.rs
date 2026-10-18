use super::super::{ControlCommandError, ServerState, api_status, authentication, sharing};
use super::{limits::Action, registry::RequestGuard};
use crate::shares::SessionContext;
use rouille::{Request, Response};
use std::io;
use uuid::Uuid;

pub(super) const COOKIE: &str = "__Host-keeppeek-guest";

pub(in crate::server) fn handle(request: &Request, state: &ServerState) -> Option<Response> {
    let path = request.url();
    if path == "/share" || path.starts_with("/share/") {
        return Some(page(request, state));
    }
    if path != "/guest" && !path.starts_with("/guest/") {
        return None;
    }
    let response = if request
        .response_abort_handle()
        .is_some_and(|abort| abort.enable_bounded_writes().is_err())
    {
        reject_unread(request, failure(503))
    } else {
        dispatch(request, state)
    };
    if response.status_code >= 400 {
        audit(
            request,
            state,
            "guest_request",
            outcome(response.status_code),
        );
    }
    let response =
        if request.method() == "POST" && path == "/guest/session" && response.status_code >= 400 {
            clear_cookie(response)
        } else {
            response
        };
    Some(headers(response))
}

fn dispatch(request: &Request, state: &ServerState) -> Response {
    let path = request.url();
    let action = match (request.method(), path.as_str()) {
        ("POST", "/guest/session") => Action::Exchange,
        ("GET" | "DELETE", "/guest/session") => Action::Status,
        ("GET", "/guest/media") => Action::Media,
        ("GET", "/guest/download") => Action::Download,
        (_, "/guest/session" | "/guest/media" | "/guest/download") => {
            return reject_unread(request, failure(405));
        }
        _ => return reject_unread(request, failure(404)),
    };
    let (context, guard) = match admit(request, state, action) {
        Ok(value) => value,
        Err(response) => return reject_unread(request, response),
    };
    match (request.method(), path.as_str()) {
        ("POST", "/guest/session") => super::session::exchange(request, state, &context, guard),
        ("GET", "/guest/session") => super::session::status(request, state, &context, guard),
        ("DELETE", "/guest/session") => super::session::end(request, state, &context, guard),
        ("GET", "/guest/media") => {
            super::media::response(request, state, &context, guard, crate::shares::Mode::Stream)
        }
        ("GET", "/guest/download") => super::media::response(
            request,
            state,
            &context,
            guard,
            crate::shares::Mode::Download,
        ),
        _ => unreachable!("guest route was classified before admission"),
    }
}

fn admit(
    request: &Request,
    state: &ServerState,
    action: Action,
) -> Result<(SessionContext, RequestGuard), Response> {
    let context = sharing::context(state).map_err(control_failure)?;
    let guard = admit_context(request, state, &context, action)?;
    Ok((context, guard))
}

fn admit_context(
    request: &Request,
    state: &ServerState,
    context: &SessionContext,
    action: Action,
) -> Result<RequestGuard, Response> {
    boundary(request, state, context)?;
    let abort = request
        .response_abort_handle()
        .ok_or_else(|| failure(503))?;
    state
        .guest_review
        .admit_request(request.remote_addr().ip(), action, abort)
        .map_err(io_failure)
}

fn boundary(
    request: &Request,
    state: &ServerState,
    context: &SessionContext,
) -> Result<(), Response> {
    if request.raw_url().len() > 512
        || request.raw_url().split('?').next() != Some(request.url().as_str())
    {
        return Err(failure(400));
    }
    let length = authentication::header(request, "Content-Length", 20)?;
    let transfer = authentication::header(request, "Transfer-Encoding", 128)?;
    if request.method() != "POST"
        && (transfer.is_some() || length.is_some_and(|value| value != "0"))
    {
        return Err(failure(400));
    }
    if request.method() == "GET" && request.raw_url() == "/share" {
        authentication::secure_channel(request, state)?;
    } else {
        authentication::secure_transport(request, state)?;
    }
    if authentication::header(request, "Authorization", 4_096)?.is_some() {
        return Err(failure(403));
    }
    let host = authentication::header(request, "Host", 512)?.ok_or_else(|| failure(400))?;
    if format!("https://{host}") != context.origin {
        return Err(failure(403));
    }
    let origin = authentication::header(request, "Origin", 2_048)?;
    if origin.is_some_and(|origin| origin != context.origin)
        || (matches!(request.method(), "POST" | "DELETE")
            && origin != Some(context.origin.as_str()))
    {
        return Err(failure(403));
    }
    Ok(())
}

pub(super) fn session_id(request: &Request, required: bool) -> Result<Option<Uuid>, Response> {
    let Some((_, query)) = request.raw_url().split_once('?') else {
        return if required {
            Err(failure(400))
        } else {
            Ok(None)
        };
    };
    let value = query.strip_prefix("session=").ok_or_else(|| failure(400))?;
    if value.len() != 36 {
        return Err(failure(400));
    }
    let id = Uuid::parse_str(value).map_err(|_| failure(400))?;
    if id.is_nil() {
        return Err(failure(400));
    }
    Ok(Some(id))
}

pub(super) fn handle_cookie(request: &Request) -> Result<&str, Response> {
    authentication::cookie(request, COOKIE)?.ok_or_else(|| failure(401))
}

pub(super) fn headers(response: Response) -> Response {
    response
        .with_unique_header("Cache-Control", "private, no-store")
        .with_unique_header("Pragma", "no-cache")
        .with_unique_header("Referrer-Policy", "no-referrer")
        .with_unique_header("X-Content-Type-Options", "nosniff")
        .with_unique_header("X-Frame-Options", "DENY")
        .with_unique_header(
            "Content-Security-Policy",
            "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        )
        .with_unique_header("Cross-Origin-Resource-Policy", "same-origin")
}

pub(super) fn clear_cookie(response: Response) -> Response {
    response.with_additional_header(
        "Set-Cookie",
        format!("{COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0"),
    )
}

pub(super) fn failure(status: u16) -> Response {
    api_status(
        status,
        match status {
            400 => "invalid guest request",
            403 => "guest origin is not allowed",
            404 => "guest route not found",
            405 => "guest method is not allowed",
            413 => "guest body is too large",
            415 => "guest body type is not supported",
            416 => "guest byte range is invalid",
            429 => "guest capacity is temporarily limited",
            503 => "guest review is unavailable",
            _ => "guest access is unavailable",
        },
    )
}

pub(super) fn io_failure(error: io::Error) -> Response {
    failure(if error.kind() == io::ErrorKind::WouldBlock {
        429
    } else {
        503
    })
}

pub(super) fn control_failure(error: ControlCommandError) -> Response {
    failure(match error._http_status {
        400 => 400,
        429 => 429,
        503 => 503,
        _ => 401,
    })
}

pub(super) fn reject_unread(request: &Request, response: Response) -> Response {
    let body = request.headers().any(|(name, value)| {
        (name.eq_ignore_ascii_case("Content-Length") && value != "0")
            || name.eq_ignore_ascii_case("Transfer-Encoding")
    });
    if body
        && let Some(abort) = request.response_abort_handle()
        && abort.abort().is_err()
    {
        tracing::warn!("guest malformed-body connection cancellation failed");
    }
    response
}

pub(super) fn audit(request: &Request, state: &ServerState, action: &str, result: &str) {
    let classification = state
        .network_access
        .classify(request.remote_addr().ip(), request.headers());
    super::super::record_access_audit(
        state,
        i64::try_from(super::super::unix_time_ms()).unwrap_or(i64::MAX),
        None,
        None,
        action,
        None,
        result,
        classification.reason,
    );
}

const fn outcome(status: u16) -> &'static str {
    match status {
        400 | 404 | 405 | 413 | 415 | 416 => "malformed_request",
        401 => "invalid_guest",
        403 | 426 => "origin_or_transport_denied",
        429 => "rate_limited",
        _ => "service_unavailable",
    }
}
fn page(request: &Request, state: &ServerState) -> Response {
    let result = (|| {
        if request.url() != "/share" {
            return Err(failure(404));
        }
        if request.method() != "GET" {
            return Err(failure(405));
        }
        if request.raw_url() != "/share" {
            return Err(failure(400));
        }
        let context = sharing::page_context(state).map_err(control_failure)?;
        let guard = admit_context(request, state, &context, Action::Status)?;
        let csp = sharing::page_csp(state).map_err(control_failure)?;
        guard.check().map_err(io_failure)?;
        Ok(headers(super::super::serve_ui(request))
            .with_unique_header("Content-Security-Policy", csp.to_string()))
    })();
    result.unwrap_or_else(|response| reject_unread(request, headers(response)))
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
