//! Runs the browser code flow without retaining provider tokens in application sessions.

use super::*;
use crate::access::{
    browser_sessions::Session,
    login_transactions::{Start, Transaction},
    oidc,
    oidc_transport::Transport,
};
use std::io::Read;

struct Intent {
    browser: Session,
    origin: String,
    revision: u64,
    provider: external::Provider,
}

struct Completed {
    browser: Uuid,
    origin: String,
    revision: u64,
    return_path: String,
    provider: external::Provider,
    verified: oidc::Verified,
}

pub(super) fn login(request: &Request, state: &ServerState) -> Result<Response, Response> {
    secure_transport(request, state)?;
    let form = login_form(request)?;
    crate::access::login_transactions::validate_return_path(
        form.get("return_path").map_or("/", String::as_str),
    )
    .map_err(|_| api_status(400, "return path is invalid"))?;
    if form.contains_key("candidate_plan_id") {
        return verification::browser::start(request, state, &form);
    }
    let intent = login_intent(request, state, &form)?;
    let settings = oidc_settings(&intent.provider)?;
    let provider = cached_provider(state, &intent.provider.id, intent.revision, settings)?;
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ensure_pending_browser(
        request,
        &mut registry,
        intent.browser.id,
        &intent.origin,
        intent.revision,
    )?;
    let target = registry
        .transactions
        .start(
            Start {
                browser: intent.browser.id,
                origin: &intent.origin,
                provider_id: &intent.provider.id,
                revision: intent.revision,
                return_path: form.get("return_path").map_or("/", String::as_str),
                candidate_plan: None,
            },
            settings,
            provider.authorization_endpoint(),
            Instant::now(),
        )
        .map_err(|_| api_status(400, "login request is invalid or at capacity"))?;
    Ok(Response::redirect_303(target.to_string()))
}

fn login_intent(
    request: &Request,
    state: &ServerState,
    form: &HashMap<String, String>,
) -> Result<Intent, Response> {
    if cookie(request, SESSION_COOKIE)?.is_some()
        || header(request, "Authorization", 4_096)?.is_some()
    {
        return Err(api_status(
            400,
            "sign out before changing authentication methods",
        ));
    }
    if form.contains_key("candidate_plan_id") {
        return Err(api_status(400, "verification plan is unavailable"));
    }
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let config = registry
        .config
        .clone()
        .ok_or_else(|| api_status(400, "external authentication is disabled"))?;
    let origin = request_origin(request, &config)?;
    let browser = pending_browser(request, &mut registry, &origin)?;
    let csrf = form
        .get("csrf_token")
        .ok_or_else(|| api_status(403, "login requires a CSRF binding"))?;
    if csrf.len() > 128
        || header(request, "Origin", 2_048)? != Some(origin.as_str())
        || !browser.csrf_matches(csrf)
    {
        return Err(api_status(403, "Origin or CSRF binding is invalid"));
    }
    let id = form
        .get("provider_id")
        .ok_or_else(|| api_status(400, "provider ID is required"))?;
    let provider = config
        .providers
        .iter()
        .find(|provider| &provider.id == id)
        .cloned()
        .ok_or_else(|| api_status(400, "identity provider is unavailable"))?;
    Ok(Intent {
        browser,
        origin,
        revision: registry.revision,
        provider,
    })
}

pub(super) fn callback(request: &Request, state: &ServerState) -> Result<Response, Response> {
    // Cross-site top-level navigation is expected here; one-use state replaces ordinary CSRF.
    secure_channel(request, state)?;
    let query = parameters(
        request.raw_query_string().as_bytes(),
        &[
            "code",
            "state",
            "error",
            "error_description",
            "error_uri",
            "iss",
            "session_state",
        ],
    )?;
    if let Some(result) = verification::oidc_browser::callback(request, state, &query) {
        return result;
    }
    let (transaction, provider, origin) = consume_callback(request, state, &query)?;
    if query.contains_key("error") {
        return Err(api_status(401, "identity provider denied sign-in"));
    }
    let settings = oidc_settings(&provider)?;
    if query
        .get("iss")
        .is_some_and(|issuer| issuer != &settings.issuer)
    {
        return Err(api_status(
            401,
            "callback issuer does not match the provider",
        ));
    }
    let code = query
        .get("code")
        .ok_or_else(|| api_status(400, "authorization code is required"))?;
    let client = cached_provider(state, &provider.id, transaction.revision, settings)?;
    let verified = client
        .exchange(
            oidc::Exchange {
                code,
                nonce: &transaction.nonce,
                verifier: transaction.verifier,
            },
            now_ms,
        )
        .map_err(|error| {
            if error.downcast_ref::<oidc::ProviderUnavailable>().is_some() {
                api_status(503, "OIDC provider is unavailable")
            } else {
                api_status(401, "OIDC code or identity verification failed")
            }
        })?;
    complete(
        request,
        state,
        Completed {
            browser: transaction.browser,
            origin,
            revision: transaction.revision,
            return_path: transaction.return_path,
            provider,
            verified,
        },
    )
}

fn consume_callback(
    request: &Request,
    state: &ServerState,
    query: &HashMap<String, String>,
) -> Result<(Transaction, external::Provider, String), Response> {
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let config = registry
        .config
        .clone()
        .ok_or_else(|| api_status(401, "external authentication is unavailable"))?;
    let origin = request_origin(request, &config)?;
    let browser = pending_browser(request, &mut registry, &origin)?;
    let value = query
        .get("state")
        .ok_or_else(|| api_status(400, "login state is required"))?;
    let revision = registry.revision;
    let transaction = registry
        .transactions
        .consume(value, browser.id, &origin, revision, Instant::now())
        .map_err(|_| api_status(401, "login state is invalid or expired"))?;
    if transaction.candidate_plan.is_some() {
        return Err(api_status(401, "verification plan is unavailable"));
    }
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == transaction.provider_id)
        .cloned()
        .ok_or_else(|| api_status(401, "identity provider is unavailable"))?;
    Ok((transaction, provider, origin))
}

fn complete(
    request: &Request,
    state: &ServerState,
    completed: Completed,
) -> Result<Response, Response> {
    let settings = oidc_settings(&completed.provider)?;
    let display_name = match completed.verified.claims.get(&settings.display_name_claim) {
        Some(serde_json::Value::String(name)) => name.as_str(),
        None => &completed.provider.name,
        _ => return Err(api_status(403, "identity display claim is invalid")),
    };
    let grant = external::map_claims(&completed.provider.mappings, &completed.verified.claims)
        .map_err(|_| api_status(403, "identity has no unambiguous access mapping"))?;
    let update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ensure_pending_browser(
        request,
        &mut registry,
        completed.browser,
        &completed.origin,
        completed.revision,
    )?;
    let identity = persist_identity(
        state,
        &mut registry,
        IdentityInput {
            provider_id: &completed.provider.id,
            namespace: &settings.issuer,
            subject: &completed.verified.subject,
            display_name,
            grant,
            now_ms: now_ms(),
        },
    )?;
    let issued = registry
        .sessions
        .rotate(
            completed.browser,
            Binding {
                identity_id: identity.id,
                revision: identity.revision,
            },
            Instant::now(),
            now_ms(),
        )
        .map_err(|_| api_status(429, "browser session capacity exceeded"))?;
    registry.transactions.revoke_browser(completed.browser);
    drop(registry);
    drop(update);
    audit(state, request, Some(&identity), "oidc_login", "success");
    Ok(Response::redirect_303(completed.return_path)
        .with_additional_header(
            "Set-Cookie",
            session_cookie(
                &issued.cookie,
                state.api_session_policy.absolute_timeout.as_secs(),
            ),
        )
        .with_additional_header(
            "Set-Cookie",
            format!("{LOGIN_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0"),
        ))
}

fn pending_browser(
    request: &Request,
    registry: &mut Registry,
    origin: &str,
) -> Result<Session, Response> {
    cookie(request, LOGIN_COOKIE)?
        .and_then(|handle| registry.sessions.lookup(handle, origin, Instant::now()))
        .filter(|browser| browser.binding.is_none())
        .ok_or_else(|| api_status(401, "browser login binding is unavailable"))
}

fn ensure_pending_browser(
    request: &Request,
    registry: &mut Registry,
    browser: Uuid,
    origin: &str,
    revision: u64,
) -> Result<(), Response> {
    if registry.revision != revision || pending_browser(request, registry, origin)?.id != browser {
        return Err(api_status(401, "login binding changed during verification"));
    }
    Ok(())
}

fn oidc_settings(provider: &external::Provider) -> Result<&external::Oidc, Response> {
    match &provider.method {
        external::Method::Oidc(settings) => Ok(settings),
        external::Method::Proxy(_) => Err(api_status(
            400,
            "proxy sign-in requires its trusted connection",
        )),
    }
}

fn cached_provider(
    state: &ServerState,
    id: &str,
    revision: u64,
    settings: &external::Oidc,
) -> Result<oidc::Provider, Response> {
    let mut cache = state
        .oidc_cache
        .try_lock()
        .map_err(|_| api_status(503, "identity provider discovery is busy"))?;
    let now = Instant::now();
    cache
        .get(revision, id, now, || {
            oidc::Provider::discover_with_budget(
                settings,
                Transport::new(settings)?,
                &state.oidc_issuer_budgets,
                now,
            )
        })
        .map_err(|_| api_status(503, "identity provider is temporarily unavailable"))
}

fn login_form(request: &Request) -> Result<HashMap<String, String>, Response> {
    if header(request, "Content-Type", 128)?.and_then(|value| value.split(';').next())
        != Some("application/x-www-form-urlencoded")
    {
        return Err(api_status(415, "login requires a URL-encoded form"));
    }
    let mut bytes = Vec::new();
    request
        .data()
        .ok_or_else(|| api_status(400, "login form is required"))?
        .take(8_193)
        .read_to_end(&mut bytes)
        .map_err(|_| api_status(400, "login form could not be read"))?;
    parameters(
        &bytes,
        &[
            "provider_id",
            "csrf_token",
            "return_path",
            "candidate_plan_id",
        ],
    )
}

fn parameters(bytes: &[u8], allowed: &[&str]) -> Result<HashMap<String, String>, Response> {
    if bytes.len() > 8_192 || std::str::from_utf8(bytes).is_err() {
        return Err(api_status(
            400,
            "authentication parameters exceed the limit",
        ));
    }
    let mut values = HashMap::new();
    for (name, value) in url::form_urlencoded::parse(bytes) {
        if !allowed.contains(&name.as_ref())
            || value.len() > 4_096
            || values
                .insert(name.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(api_status(
                400,
                "authentication parameters are invalid or duplicated",
            ));
        }
    }
    Ok(values)
}
