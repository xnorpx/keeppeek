//! Runs candidate OIDC exchanges without granting an application session.

use super::*;
use crate::access::{external, identities, login_transactions, oidc, oidc_transport::Transport};
use rouille::{Request, Response};

#[derive(Clone)]
pub(super) struct Pending {
    client: oidc::Provider,
    provider: external::Provider,
    origin: String,
    pub(super) browser: Uuid,
    revision: u64,
}

struct Callback {
    id: Uuid,
    proof: Proof,
    principal: ApiPrincipal,
    pending: Box<Pending>,
    return_path: String,
}

pub(super) fn start(
    request: &Request,
    state: &ServerState,
    context: browser::StartContext,
) -> Result<Response, ControlCommandError> {
    let settings = settings(&context.provider)?;
    let client = candidate_provider(state, settings)?;
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, context.proof.session, || {
        browser::checked_candidate(
            state,
            context.proof.session,
            &context.principal,
            &context.proof,
        )?;
        require_remote_owner(state, context.proof.session, &context.principal)?;
        issue_transaction(request, state, &context, client)
    })
}

fn candidate_provider(
    state: &ServerState,
    settings: &external::Oidc,
) -> Result<oidc::Provider, ControlCommandError> {
    let cache = state
        .configuration_plans
        .proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .providers
        .clone();
    let mut cache = cache
        .try_lock()
        .map_err(|_| unavailable("candidate discovery is busy"))?;
    let now = Instant::now();
    cache
        .get(settings, now, || {
            oidc::Provider::discover_with_budget(
                settings,
                Transport::new(settings)?,
                &state.oidc_issuer_budgets,
                now,
            )
        })
        .map_err(|_| unavailable("candidate identity provider is unavailable"))
}

fn issue_transaction(
    request: &Request,
    state: &ServerState,
    context: &browser::StartContext,
    client: oidc::Provider,
) -> Result<Response, ControlCommandError> {
    let mut authentication = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let issued = authentication
        .sessions
        .issue(
            None,
            &context.origin,
            request.remote_addr().ip(),
            Instant::now(),
            super::super::now_ms(),
        )
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::Rejected,
                429,
                "browser verification capacity exceeded",
            )
        })?;
    let mut registry = state
        .configuration_plans
        .proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let target = registry.transactions.start(
        login_transactions::Start {
            browser: issued.session_id,
            origin: &context.origin,
            provider_id: &context.provider.id,
            revision: authentication.revision,
            return_path: &context.return_path,
            candidate_plan: Some(context.id),
        },
        settings(&context.provider)?,
        client.authorization_endpoint(),
        Instant::now(),
    );
    let target = match target {
        Ok(target) => target,
        Err(_) => {
            authentication.sessions.revoke(issued.session_id);
            return Err(rejected("candidate login transaction could not be started"));
        }
    };
    let Some(proof) = registry.proofs.get_mut(&context.id) else {
        authentication.sessions.revoke(issued.session_id);
        registry.transactions.revoke_browser(issued.session_id);
        return Err(rejected("verification is unavailable"));
    };
    proof.stage = Stage::OidcPending(Box::new(Pending {
        client,
        provider: context.provider.clone(),
        origin: context.origin.clone(),
        browser: issued.session_id,
        revision: authentication.revision,
    }));
    Ok(Response::redirect_303(target.to_string())
        .with_additional_header("Set-Cookie", super::super::login_cookie(&issued.cookie)))
}

pub(in crate::server::authentication) fn callback(
    request: &Request,
    state: &ServerState,
    query: &HashMap<String, String>,
) -> Option<Result<Response, Response>> {
    let value = query.get("state")?;
    let id = state
        .configuration_plans
        .proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .transactions
        .candidate(value)?;
    Some(finish_callback(request, state, query, id).map_err(browser::http_error))
}

fn finish_callback(
    request: &Request,
    state: &ServerState,
    query: &HashMap<String, String>,
    id: Uuid,
) -> Result<Response, ControlCommandError> {
    let (callback, transaction) = consume_callback(request, state, query, id)?;
    let settings = settings(&callback.pending.provider)?;
    if query.contains_key("error")
        || query
            .get("iss")
            .is_some_and(|issuer| issuer != &settings.issuer)
    {
        return Err(denied("candidate identity provider rejected verification"));
    }
    let code = query
        .get("code")
        .ok_or_else(|| denied("candidate authorization code is required"))?;
    let verified = callback
        .pending
        .client
        .exchange(
            oidc::Exchange {
                code,
                nonce: &transaction.nonce,
                verifier: transaction.verifier,
            },
            super::super::now_ms,
        )
        .map_err(|error| {
            if error.downcast_ref::<oidc::ProviderUnavailable>().is_some() {
                unavailable("candidate identity provider is unavailable")
            } else {
                denied("candidate identity verification failed")
            }
        })?;
    complete(request, state, callback, verified)
}

fn consume_callback(
    request: &Request,
    state: &ServerState,
    query: &HashMap<String, String>,
    id: Uuid,
) -> Result<(Callback, login_transactions::Transaction), ControlCommandError> {
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let proof = browser::snapshot(state, id)?;
    let principal = browser::parent(state, &proof)?;
    session_lifecycle::admit(state, proof.session, || {
        let candidate = browser::checked_candidate(state, proof.session, &principal, &proof)?;
        let Stage::OidcPending(pending) = &proof.stage else {
            return Err(denied("candidate login is unavailable"));
        };
        let config = candidate
            .after
            .external_auth
            .as_ref()
            .ok_or_else(|| denied("candidate is unavailable"))?;
        let origin = super::super::request_origin(request, config)
            .map_err(|_| denied("candidate origin is invalid"))?;
        let browser = pending_browser(request, state, pending).unwrap_or_else(|_| Uuid::nil());
        let mut registry = state
            .configuration_plans
            .proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let transaction = registry
            .transactions
            .consume(
                query.get("state").map_or("", String::as_str),
                browser,
                &origin,
                pending.revision,
                Instant::now(),
            )
            .map_err(|_| denied("candidate login state is invalid or expired"))?;
        Ok((
            Callback {
                id,
                proof: proof.clone(),
                principal: principal.clone(),
                pending: pending.clone(),
                return_path: transaction.return_path.clone(),
            },
            transaction,
        ))
    })
}

fn pending_browser(
    request: &Request,
    state: &ServerState,
    pending: &Pending,
) -> Result<Uuid, ControlCommandError> {
    let cookie = super::super::cookie(request, super::super::LOGIN_COOKIE)
        .map_err(|_| denied("candidate browser binding is invalid"))?
        .ok_or_else(|| denied("candidate browser binding is unavailable"))?;
    let mut authentication = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if authentication.revision != pending.revision {
        return Err(denied("authentication configuration changed"));
    }
    authentication
        .sessions
        .lookup(cookie, &pending.origin, Instant::now())
        .filter(|browser| browser.id == pending.browser && browser.binding.is_none())
        .map(|browser| browser.id)
        .ok_or_else(|| denied("candidate browser binding is unavailable"))
}

fn complete(
    request: &Request,
    state: &ServerState,
    callback: Callback,
    verified: oidc::Verified,
) -> Result<Response, ControlCommandError> {
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, callback.proof.session, || {
        let candidate = browser::checked_candidate(
            state,
            callback.proof.session,
            &callback.principal,
            &callback.proof,
        )?;
        pending_browser(request, state, &callback.pending)?;
        let prepared = admit_identity(&candidate.after, &callback.pending.provider, verified)?;
        require_remote_owner(state, callback.proof.session, &callback.principal)?;
        browser::record_identity(state, callback.id, prepared)?;
        state
            .authentication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .revoke(callback.pending.browser);
        Ok(
            Response::redirect_303(callback.return_path).with_additional_header(
                "Set-Cookie",
                format!(
                    "{}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0",
                    super::super::LOGIN_COOKIE,
                ),
            ),
        )
    })
}

fn admit_identity(
    candidate: &Config,
    provider: &external::Provider,
    verified: oidc::Verified,
) -> Result<identities::PreparedIdentity, ControlCommandError> {
    let settings = settings(provider)?;
    let grant = external::map_claims(&provider.mappings, &verified.claims)
        .map_err(|_| denied("candidate identity has no unambiguous mapping"))?;
    if grant.role != AccessRole::Administrator {
        return Err(denied("candidate identity must be an Administrator"));
    }
    let name = match verified.claims.get(&settings.display_name_claim) {
        Some(serde_json::Value::String(name)) => name.as_str(),
        None => &provider.name,
        _ => return Err(denied("candidate display name is invalid")),
    };
    let directory = identities::Directory::from_root(&candidate.source)
        .map_err(|_| rejected("candidate identity directory is unavailable"))?;
    directory
        .prepare_authenticated_admission(
            identities::IdentityInput {
                provider_id: &provider.id,
                namespace: &settings.issuer,
                subject: &verified.subject,
                display_name: name,
                grant,
                now_ms: super::super::now_ms(),
            },
            provider,
        )
        .map_err(|_| denied("candidate identity cannot be admitted"))
}

fn settings(provider: &external::Provider) -> Result<&external::Oidc, ControlCommandError> {
    match &provider.method {
        external::Method::Oidc(settings) => Ok(settings),
        external::Method::Proxy(_) => Err(rejected("candidate is not an OIDC provider")),
    }
}

fn denied(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, 401, message)
}

fn unavailable(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Unavailable, 503, message)
}

#[cfg(test)]
#[path = "authentication_verification_oidc_tests.rs"]
mod tests;
