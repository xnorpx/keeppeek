//! Verifies a candidate identity without replacing the current browser session.

use super::*;
use crate::access::{browser_sessions, external, identities, login_transactions, proxy_identity};
use rouille::{Request, Response};
use subtle::ConstantTimeEq;

pub(super) struct StartContext {
    pub id: Uuid,
    pub proof: Proof,
    pub principal: ApiPrincipal,
    pub provider: external::Provider,
    pub origin: String,
    pub return_path: String,
}

pub(in crate::server) fn prepare(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    request: proto::PrepareAdministratorVerification,
) -> Result<proto::AdministratorVerification, ControlCommandError> {
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, session, || {
        require_remote_owner(state, session, principal)?;
        let candidate =
            resolve_candidate(state, session, principal, &request.configuration_plan_id)?;
        let config = candidate
            .after
            .external_auth
            .as_ref()
            .ok_or_else(|| rejected("candidate has no external authentication"))?;
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == request.provider_id)
            .ok_or_else(|| rejected("candidate provider is unavailable"))?;
        if !config.allowed_origins.contains(&request.origin) {
            return Err(rejected("candidate origin is unavailable"));
        }
        if let external::Method::Oidc(settings) = &provider.method {
            let redirect = url::Url::parse(&settings.redirect_uri)
                .map_err(|_| rejected("candidate redirect is invalid"))?;
            if redirect.origin().ascii_serialization() != request.origin {
                return Err(rejected("candidate origin must match its redirect"));
            }
        }
        let token = browser_sessions::random_token();
        let proof = Proof::new(
            session,
            principal,
            &candidate,
            Stage::BrowserStart {
                origin: request.origin.clone(),
                provider_id: request.provider_id.clone(),
                challenge: Some(Sha256::digest(token.as_bytes()).into()),
            },
        )?;
        require_remote_owner(state, session, principal)?;
        let mut response = state
            .configuration_plans
            .proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(proof, Instant::now(), super::super::now_ms())?;
        response.browser_start = Some(proto::AdministratorBrowserStart {
            origin: request.origin,
            provider_id: request.provider_id,
            csrf_token: token,
        });
        Ok(response)
    })
}

pub(in crate::server) fn get(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    request: proto::GetAdministratorVerification,
) -> Result<proto::AdministratorVerification, ControlCommandError> {
    let id = Uuid::parse_str(&request.verification_id)
        .map_err(|_| rejected("Administrator verification is unavailable"))?;
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, session, || {
        let proof = snapshot(state, id)?;
        checked_candidate(state, session, principal, &proof)?;
        Ok(proof.response(id))
    })
}

pub(super) fn snapshot(state: &ServerState, id: Uuid) -> Result<Proof, ControlCommandError> {
    state
        .configuration_plans
        .proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .proofs
        .get(&id)
        .cloned()
        .ok_or_else(|| rejected("Administrator verification is unavailable"))
}

pub(super) fn checked_candidate(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    proof: &Proof,
) -> Result<Candidate, ControlCommandError> {
    require_remote_owner(state, session, principal)?;
    let candidate = resolve_candidate(state, session, principal, &proof.plan_id)?;
    proof.validate(
        session,
        principal,
        &candidate,
        fingerprint(&candidate.path, &candidate.before, &candidate.after)?,
        Instant::now(),
        super::super::now_ms(),
    )?;
    Ok(candidate)
}

pub(in crate::server::authentication) fn start(
    request: &Request,
    state: &ServerState,
    form: &HashMap<String, String>,
) -> Result<Response, Response> {
    if super::super::header(request, "Authorization", 4_096)?.is_some() {
        return Err(super::super::api_status(
            400,
            "verification cannot use a bearer header",
        ));
    }
    login_transactions::validate_return_path(form.get("return_path").map_or("/", String::as_str))
        .map_err(|_| super::super::api_status(400, "return path is invalid"))?;
    let id = form
        .get("candidate_plan_id")
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| super::super::api_status(400, "verification plan is unavailable"))?;
    let context = start_context(request, state, form, id).map_err(http_error)?;
    match &context.provider.method {
        external::Method::Proxy(_) => start_proxy(request, state, context).map_err(http_error),
        external::Method::Oidc(_) => {
            super::oidc_browser::start(request, state, context).map_err(http_error)
        }
    }
}

fn start_context(
    request: &Request,
    state: &ServerState,
    form: &HashMap<String, String>,
    id: Uuid,
) -> Result<StartContext, ControlCommandError> {
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let proof = snapshot(state, id)?;
    let principal = parent(state, &proof)?;
    session_lifecycle::admit(state, proof.session, || {
        let candidate = checked_candidate(state, proof.session, &principal, &proof)?;
        let config = candidate
            .after
            .external_auth
            .as_ref()
            .ok_or_else(|| rejected("candidate is unavailable"))?;
        let provider_id = check_start(request, form, config, &proof)?;
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .cloned()
            .ok_or_else(|| rejected("candidate provider is unavailable"))?;
        let origin = super::super::request_origin(request, config)
            .map_err(|_| denied("verification origin is invalid"))?;
        consume_start(state, id)?;
        Ok(StartContext {
            id,
            proof: proof.clone(),
            principal: principal.clone(),
            provider,
            origin,
            return_path: form
                .get("return_path")
                .cloned()
                .unwrap_or_else(|| "/".into()),
        })
    })
}

pub(super) fn parent(
    state: &ServerState,
    proof: &Proof,
) -> Result<ApiPrincipal, ControlCommandError> {
    state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&proof.session)
        .map(|owner| owner.principal.clone())
        .ok_or_else(|| rejected("verification parent session is unavailable"))
}

fn start_proxy(
    request: &Request,
    state: &ServerState,
    context: StartContext,
) -> Result<Response, ControlCommandError> {
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, context.proof.session, || {
        let candidate = checked_candidate(
            state,
            context.proof.session,
            &context.principal,
            &context.proof,
        )?;
        let config = candidate
            .after
            .external_auth
            .as_ref()
            .ok_or_else(|| rejected("candidate is unavailable"))?;
        let prepared = proxy_admission(request, config, &candidate.after, &context.provider.id)?;
        require_remote_owner(state, context.proof.session, &context.principal)?;
        record_identity(state, context.id, prepared)?;
        Ok(Response::empty_204())
    })
}

pub(super) fn record_identity(
    state: &ServerState,
    id: Uuid,
    prepared: identities::PreparedIdentity,
) -> Result<(), ControlCommandError> {
    let mut registry = state
        .configuration_plans
        .proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let live = registry
        .proofs
        .get_mut(&id)
        .ok_or_else(|| rejected("verification is unavailable"))?;
    live.stage = Stage::External(prepared);
    Ok(())
}

pub(super) fn http_error(error: ControlCommandError) -> Response {
    super::super::api_status(error._http_status, &error.message)
}

fn check_start(
    request: &Request,
    form: &HashMap<String, String>,
    config: &external::Config,
    proof: &Proof,
) -> Result<String, ControlCommandError> {
    let Stage::BrowserStart {
        origin,
        provider_id,
        challenge: Some(challenge),
    } = &proof.stage
    else {
        return Err(rejected("verification start is unavailable"));
    };
    let actual = super::super::request_origin(request, config)
        .map_err(|_| denied("verification origin is invalid"))?;
    let provided = super::super::header(request, "Origin", 2_048)
        .map_err(|_| denied("verification origin is invalid"))?;
    let csrf = form
        .get("csrf_token")
        .ok_or_else(|| denied("verification CSRF binding is required"))?;
    let digest: [u8; 32] = Sha256::digest(csrf.as_bytes()).into();
    if &actual != origin
        || provided != Some(origin.as_str())
        || form.get("provider_id") != Some(provider_id)
        || csrf.len() > 128
        || !bool::from(challenge.ct_eq(&digest))
    {
        return Err(denied("verification browser binding is invalid"));
    }
    Ok(provider_id.clone())
}

fn consume_start(state: &ServerState, id: Uuid) -> Result<(), ControlCommandError> {
    let mut registry = state
        .configuration_plans
        .proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let proof = registry
        .proofs
        .get_mut(&id)
        .ok_or_else(|| rejected("verification is unavailable"))?;
    let Stage::BrowserStart { challenge, .. } = &mut proof.stage else {
        return Err(rejected("verification start is unavailable"));
    };
    challenge
        .take()
        .ok_or_else(|| rejected("verification start was already consumed"))?;
    Ok(())
}

fn proxy_admission(
    request: &Request,
    config: &external::Config,
    candidate: &Config,
    provider_id: &str,
) -> Result<identities::PreparedIdentity, ControlCommandError> {
    // ponytail: Reuse the complete proxy policy to reject peers trusted by multiple providers.
    let assertion = proxy_identity::authenticate(
        config,
        request.remote_addr().ip(),
        &request.headers().take(129).collect::<Vec<_>>(),
    )
    .map_err(|_| denied("candidate proxy evidence is invalid"))?
    .ok_or_else(|| denied("candidate proxy evidence is unavailable"))?;
    if assertion.provider_id != provider_id || assertion.grant.role != AccessRole::Administrator {
        return Err(denied("candidate identity must be an Administrator"));
    }
    let directory = identities::Directory::from_root(&candidate.source)
        .map_err(|_| rejected("candidate identity directory is unavailable"))?;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| rejected("candidate provider is unavailable"))?;
    directory
        .prepare_authenticated_admission(
            identities::IdentityInput {
                provider_id: &assertion.provider_id,
                namespace: &assertion.namespace,
                subject: &assertion.subject,
                display_name: &assertion.display_name,
                grant: assertion.grant,
                now_ms: super::super::now_ms(),
            },
            provider,
        )
        .map_err(|_| denied("candidate identity cannot be admitted"))
}

fn denied(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, 403, message)
}

#[cfg(test)]
#[path = "authentication_verification_browser_tests.rs"]
mod tests;
