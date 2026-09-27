//! Owns external browser authentication and its live principal bindings.

#[cfg(test)]
#[path = "authentication_oidc_tests.rs"]
mod oidc_tests;

#[cfg(test)]
#[path = "authentication_limits_tests.rs"]
mod limits_tests;

#[cfg(test)]
#[path = "authentication_cookie_recovery_tests.rs"]
mod cookie_recovery_tests;

#[cfg(test)]
#[path = "authentication_role_matrix_tests.rs"]
mod role_matrix_tests;

#[path = "authentication_oidc.rs"]
mod oidc_browser;

#[path = "authentication_admin.rs"]
pub(super) mod admin;

#[path = "authentication_verification.rs"]
pub(super) mod verification;

use super::{ApiPrincipal, ApiPrincipalIdentity, ApiSessionPolicy, ServerState, api_status};
use crate::{
    access::{
        CameraAccess,
        browser_sessions::{Binding, Limits, Sessions},
        external,
        identities::{Directory, Identity, IdentityInput},
        login_transactions::Transactions,
    },
    config::Config,
};
use rouille::{Request, Response};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

const SESSION_COOKIE: &str = "__Host-keeppeek-session";
const LOGIN_COOKIE: &str = "__Host-keeppeek-login";

// Shared start/callback budgets permit retries while bounding unauthenticated work.
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const LOGIN_ADDRESS_ATTEMPTS: u32 = 30;
const LOGIN_BROWSER_ATTEMPTS: u32 = 10;
// Active windows are never evicted to admit a flood of new keys.
const LOGIN_ADDRESS_LIMIT: usize = 1_024;
const LOGIN_BROWSER_LIMIT: usize = 4_096;

#[derive(Default)]
struct LoginBudget {
    addresses: HashMap<IpAddr, AttemptWindow>,
    browsers: HashMap<[u8; 32], AttemptWindow>,
}

struct AttemptWindow {
    started: Instant,
    attempts: u32,
}

fn charge_login<K: Eq + std::hash::Hash>(
    windows: &mut HashMap<K, AttemptWindow>,
    key: K,
    capacity: usize,
    limit: u32,
    now: Instant,
) -> Result<(), Response> {
    // ponytail: bounded tables keep expiration scans simple; index expiry only if profiling requires it.
    windows.retain(|_, window| now.saturating_duration_since(window.started) < LOGIN_WINDOW);
    if !windows.contains_key(&key) && windows.len() >= capacity {
        return Err(api_status(429, "login attempt capacity exceeded"));
    }
    let window = windows.entry(key).or_insert(AttemptWindow {
        started: now,
        attempts: 0,
    });
    if window.attempts >= limit {
        return Err(api_status(429, "login attempts are temporarily limited"));
    }
    window.attempts += 1;
    Ok(())
}

impl LoginBudget {
    fn admit(&mut self, request: &Request, now: Instant) -> Result<(), Response> {
        let address = match request.remote_addr().ip() {
            IpAddr::V6(address) => address
                .to_ipv4_mapped()
                .map_or(IpAddr::V6(address), IpAddr::V4),
            address => address,
        };
        charge_login(
            &mut self.addresses,
            address,
            LOGIN_ADDRESS_LIMIT,
            LOGIN_ADDRESS_ATTEMPTS,
            now,
        )?;
        for name in [LOGIN_COOKIE, SESSION_COOKIE] {
            if let Some(value) = cookie(request, name)? {
                charge_login(
                    &mut self.browsers,
                    Sha256::digest(value.as_bytes()).into(),
                    LOGIN_BROWSER_LIMIT,
                    LOGIN_BROWSER_ATTEMPTS,
                    now,
                )?;
            }
        }
        Ok(())
    }
}

pub(super) struct Registry {
    config: Option<external::Config>,
    revision: u64,
    directory: Directory,
    sessions: Sessions,
    transactions: Transactions,
    login_budget: LoginBudget,
}

impl Registry {
    pub(super) fn new(config: &Config, policy: ApiSessionPolicy) -> Self {
        Self {
            config: config.external_auth.clone(),
            revision: 1,
            directory: Directory::from_root(&config.source)
                .expect("external identities must be validated before server startup"),
            sessions: Sessions::new(Limits {
                idle: policy.idle_timeout,
                absolute: policy.absolute_timeout,
                per_identity: policy.max_per_principal,
                per_address: policy.max_per_address,
            }),
            transactions: Transactions::default(),
            login_budget: LoginBudget::default(),
        }
    }
}

pub(super) fn prepare_configuration(
    state: &ServerState,
    before: &Config,
    after: &Config,
) -> anyhow::Result<Registry> {
    let registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    anyhow::ensure!(
        registry.config == before.external_auth,
        "authentication settings changed on disk"
    );
    anyhow::ensure!(
        registry.directory == Directory::from_root(&before.source)?,
        "identity directory changed on disk"
    );
    let mut next = Registry::new(after, state.api_session_policy);
    next.revision = registry
        .revision
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("authentication revision exhausted"))?;
    Ok(next)
}

#[cfg(test)]
pub(super) fn activate_configuration(state: &ServerState, next: Option<Registry>) {
    if let Some(next) = next {
        let mut current = state
            .authentication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        activate_configuration_locked(&mut current, next);
    }
}

fn activate_configuration_locked(current: &mut Registry, mut next: Registry) {
    next.login_budget = std::mem::take(&mut current.login_budget);
    *current = next;
}

/// The caller must hold config_update; the persistence closure must not lock authentication.
pub(super) fn commit_configuration(
    state: &ServerState,
    principal: Option<&ApiPrincipal>,
    next: Option<Registry>,
    persist: impl FnOnce() -> Result<(), super::ControlCommandError>,
) -> Result<(), super::ControlCommandError> {
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if principal.is_some_and(|principal| {
        principal.role != crate::access::AccessRole::Administrator
            || !active_locked(state, &registry, principal, Instant::now(), now_ms())
    }) {
        return Err(super::ControlCommandError::new(
            crate::api::proto::ErrorCode::Rejected,
            403,
            "configuration Administrator is no longer active",
        ));
    }
    persist()?;
    if let Some(next) = next {
        activate_configuration_locked(&mut registry, next);
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    pub(super) static BEFORE_CONFIGURATION_COMMIT: std::cell::Cell<Option<fn(&ServerState)>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn before_configuration_commit(state: &ServerState) {
    BEFORE_CONFIGURATION_COMMIT.with(|hook| {
        if let Some(hook) = hook.take() {
            hook(state);
        }
    });
}

fn principal(identity: &Identity, browser: Uuid, expires_at_ms: i64) -> ApiPrincipal {
    ApiPrincipal {
        identity: ApiPrincipalIdentity::External {
            id: identity.id,
            revision: identity.revision,
            browser,
        },
        display_name: identity.display_name.clone(),
        role: identity.role,
        credential_expires_at_ms: Some(expires_at_ms),
    }
}

pub(super) fn session_metadata(
    state: &ServerState,
    principal: &ApiPrincipal,
) -> Option<crate::api::proto::AccessAuthentication> {
    use crate::api::proto::{AccessAuthentication, AccessAuthenticationMethod as Method};
    let method = match principal.identity {
        ApiPrincipalIdentity::Local(_) => Method::TrustedLocal,
        ApiPrincipalIdentity::Credential { .. } => Method::Bearer,
        ApiPrincipalIdentity::External {
            id,
            revision,
            browser,
        } => {
            let registry = state
                .authentication
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !registry.sessions.active(
                browser,
                Binding {
                    identity_id: id,
                    revision,
                },
                Instant::now(),
            ) {
                return None;
            }
            let identity = registry.directory.active(id, revision)?;
            let provider = registry
                .config
                .as_ref()?
                .providers
                .iter()
                .find(|provider| provider.id == identity.provider_id)?;
            let method = match &provider.method {
                external::Method::Oidc(_) => Method::Oidc,
                external::Method::Proxy(_) => Method::Proxy,
            };
            return Some(AccessAuthentication {
                method: method as i32,
                provider_id: Some(provider.id.clone()),
                identity_id: Some(id.to_string()),
            });
        }
    };
    Some(AccessAuthentication {
        method: method as i32,
        provider_id: None,
        identity_id: None,
    })
}

pub(super) fn active(
    state: &ServerState,
    principal: &ApiPrincipal,
    now: Instant,
    now_ms: i64,
) -> bool {
    let registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    active_locked(state, &registry, principal, now, now_ms)
}

fn active_locked(
    state: &ServerState,
    registry: &Registry,
    principal: &ApiPrincipal,
    now: Instant,
    now_ms: i64,
) -> bool {
    match principal.identity {
        ApiPrincipalIdentity::Local(_) => true,
        ApiPrincipalIdentity::Credential { id, revision } => {
            registry.config.as_ref().is_none_or(|config| {
                config.bearer_enabled
                    && config
                        .bearer_transition_until_ms
                        .is_some_and(|until| now_ms < until)
            }) && state
                .access_manager
                .credential_is_active(id, revision, now_ms)
        }
        ApiPrincipalIdentity::External {
            id,
            revision,
            browser,
        } => {
            registry.directory.active(id, revision).is_some()
                && registry.sessions.active(
                    browser,
                    Binding {
                        identity_id: id,
                        revision,
                    },
                    now,
                )
        }
    }
}

pub(super) fn bearer_allowed(state: &ServerState, now_ms: i64) -> bool {
    let registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.config.as_ref().is_none_or(|config| {
        config.bearer_enabled
            && config
                .bearer_transition_until_ms
                .is_some_and(|until| now_ms < until)
    })
}

pub(super) fn touch(state: &ServerState, principal: &ApiPrincipal, now: Instant) -> bool {
    let ApiPrincipalIdentity::External {
        id,
        revision,
        browser,
    } = principal.identity
    else {
        return true;
    };
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.directory.active(id, revision).is_some()
        && registry.sessions.touch(
            browser,
            Binding {
                identity_id: id,
                revision,
            },
            now,
        )
}

pub(super) fn camera_policy(state: &ServerState, principal: &ApiPrincipal) -> Option<CameraAccess> {
    let ApiPrincipalIdentity::External {
        id,
        revision,
        browser,
    } = principal.identity
    else {
        return None;
    };
    let registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !registry.sessions.active(
        browser,
        Binding {
            identity_id: id,
            revision,
        },
        Instant::now(),
    ) {
        return None;
    }
    registry
        .directory
        .active(id, revision)
        .map(|identity| identity.camera_access.clone())
}

pub(super) fn identity_ids(state: &ServerState) -> Vec<String> {
    state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .directory
        .records
        .iter()
        .map(|identity| identity.id.to_string())
        .collect()
}

fn header<'a>(request: &'a Request, name: &str, limit: usize) -> Result<Option<&'a str>, Response> {
    let mut matching = request
        .headers()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let Some((_, value)) = matching.next() else {
        return Ok(None);
    };
    if matching.next().is_some() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(api_status(400, "invalid authentication header"));
    }
    Ok(Some(value))
}

fn cookie<'a>(request: &'a Request, name: &str) -> Result<Option<&'a str>, Response> {
    let Some(value) = header(request, "Cookie", 16_384)? else {
        return Ok(None);
    };
    let mut selected = None;
    for part in value.split(';') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        if key == name {
            if selected.is_some() || value.is_empty() || value.len() > 128 {
                return Err(api_status(400, "invalid authentication cookie"));
            }
            selected = Some(value);
        }
    }
    Ok(selected)
}

fn validate_csrf(request: &Request, origin: &str, expected: &str) -> Result<(), Response> {
    let provided = header(request, "X-KeepPeek-CSRF", 128)?;
    let origin_matches = header(request, "Origin", 2_048)? == Some(origin);
    let expected: [u8; 32] = Sha256::digest(expected.as_bytes()).into();
    let actual: [u8; 32] = Sha256::digest(provided.unwrap_or_default().as_bytes()).into();
    if !origin_matches || provided.is_none() || !bool::from(expected.ct_eq(&actual)) {
        return Err(api_status(403, "Origin or CSRF binding is invalid"));
    }
    Ok(())
}

fn session_cookie(value: &str, lifetime_secs: u64) -> String {
    format!(
        "{SESSION_COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={lifetime_secs}"
    )
}

fn login_cookie(value: &str) -> String {
    format!("{LOGIN_COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=300")
}

pub(super) fn request_origin(
    request: &Request,
    config: &external::Config,
) -> Result<String, Response> {
    let host = header(request, "Host", 256)?.ok_or_else(|| api_status(400, "Host is required"))?;
    let origin = format!("https://{host}");
    if !config.allowed_origins.contains(&origin) {
        return Err(api_status(403, "authentication origin is not allowed"));
    }
    if header(request, "Origin", 2_048)?.is_some_and(|provided| provided != origin) {
        return Err(api_status(
            403,
            "authentication origin does not match the request",
        ));
    }
    Ok(origin)
}

pub(super) fn http_principal(
    request: &Request,
    state: &ServerState,
    now_ms: i64,
) -> Result<Option<ApiPrincipal>, Response> {
    let handle = cookie(request, SESSION_COOKIE)?;
    let authorization = header(request, "Authorization", 4_096)?;
    if handle.is_some() && authorization.is_some() {
        return Err(api_status(400, "authentication methods cannot be combined"));
    }
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(config) = registry.config.clone() else {
        return if handle.is_some() {
            Err(api_status(401, "browser session is unavailable"))
        } else {
            Ok(None)
        };
    };
    let Some(handle) = handle else {
        let assertion = proxy_assertion(request, &config)?;
        if authorization.is_some() && assertion.is_some() {
            return Err(api_status(400, "authentication methods cannot be combined"));
        }
        if assertion.is_some()
            || !config.bearer_enabled
            || !config
                .bearer_transition_until_ms
                .is_some_and(|until| now_ms < until)
        {
            return Err(api_status(401, "browser sign-in is required"));
        }
        return Ok(None);
    };
    secure_transport(request, state)?;
    let origin = request_origin(request, &config)?;
    let browser = registry
        .sessions
        .lookup(handle, &origin, Instant::now())
        .ok_or_else(|| api_status(401, "browser session expired or was revoked"))?;
    let binding = browser
        .binding
        .ok_or_else(|| api_status(401, "browser sign-in is required"))?;
    let identity = registry
        .directory
        .active(binding.identity_id, binding.revision)
        .cloned()
        .ok_or_else(|| api_status(401, "external identity was revoked"))?;
    if let Err(response) = proxy_assertion(request, &config)
        .and_then(|assertion| corroborate_proxy(&config, &identity, assertion.as_ref()))
    {
        registry.sessions.revoke(browser.id);
        registry.transactions.revoke_browser(browser.id);
        return Err(response);
    }
    if !matches!(request.method(), "GET" | "HEAD" | "OPTIONS") {
        validate_csrf(request, &origin, browser.csrf())?;
    }
    renew_browser(&mut registry, &browser, Instant::now())?;
    Ok(Some(principal(
        &identity,
        browser.id,
        browser.absolute_expires_at_ms,
    )))
}

fn proxy_assertion(
    request: &Request,
    config: &external::Config,
) -> Result<Option<crate::access::proxy_identity::Assertion>, Response> {
    let headers = request.headers().take(129).collect::<Vec<_>>();
    crate::access::proxy_identity::authenticate(config, request.remote_addr().ip(), &headers)
        .map_err(|_| api_status(401, "identity proxy assertions are invalid"))
}

fn renew_browser(
    registry: &mut Registry,
    browser: &crate::access::browser_sessions::Session,
    now: Instant,
) -> Result<(), Response> {
    if browser
        .binding
        .is_some_and(|binding| registry.sessions.touch(browser.id, binding, now))
    {
        return Ok(());
    }
    registry.sessions.revoke(browser.id);
    registry.transactions.revoke_browser(browser.id);
    Err(api_status(401, "browser session expired or was revoked"))
}

fn corroborate_proxy(
    config: &external::Config,
    identity: &Identity,
    assertion: Option<&crate::access::proxy_identity::Assertion>,
) -> Result<(), Response> {
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == identity.provider_id)
        .ok_or_else(|| api_status(401, "identity provider is unavailable"))?;
    let valid = match (&provider.method, assertion) {
        (external::Method::Oidc(_), None) => true,
        (external::Method::Proxy(_), Some(assertion)) => {
            assertion.provider_id == identity.provider_id
                && external::subject_fingerprint(&assertion.namespace, &assertion.subject)
                    == identity.subject_fingerprint
                && assertion.grant.role == identity.role
                && assertion.grant.camera_access == identity.camera_access
        }
        _ => false,
    };
    if !valid {
        return Err(api_status(401, "browser and proxy identities do not match"));
    }
    Ok(())
}

pub(super) fn allows_origin(request: &Request, state: &ServerState, origin: &str) -> bool {
    let registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.config.as_ref().is_some_and(|config| {
        request_origin(request, config).is_ok_and(|expected| expected == origin)
    })
}

pub(super) fn handle(request: &Request, state: &ServerState) -> Option<Response> {
    let result = match (request.method(), request.url().as_str()) {
        ("GET", "/auth/session") => bootstrap(request, state),
        ("POST", "/auth/logout") => logout(request, state),
        ("POST", "/auth/login") => {
            admit_login_attempt(request, state).and_then(|()| oidc_browser::login(request, state))
        }
        ("GET", "/auth/callback") => admit_login_attempt(request, state)
            .and_then(|()| oidc_browser::callback(request, state)),
        (_, "/auth/session" | "/auth/logout" | "/auth/login" | "/auth/callback") => {
            Err(api_status(405, "authentication method is not allowed"))
        }
        (_, path) if path.starts_with("/auth/") => {
            Err(api_status(404, "authentication route not found"))
        }
        _ => return None,
    };
    let response = result.unwrap_or_else(|response| response);
    if response.status_code >= 400 {
        state
            .access_metrics
            .authentication_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let action = match (request.method(), request.url().as_str()) {
        ("POST", "/auth/login") => Some("external_login_start"),
        ("GET", "/auth/callback") => Some("external_login_callback"),
        _ if response.status_code >= 400 => Some("external_authentication_failure"),
        _ => None,
    };
    if let Some(action) = action {
        audit(
            state,
            request,
            None,
            action,
            authentication_outcome(response.status_code),
        );
    }
    Some(
        response
            .with_additional_header("Cache-Control", "no-store")
            .with_additional_header("Referrer-Policy", "no-referrer"),
    )
}

fn admit_login_attempt(request: &Request, state: &ServerState) -> Result<(), Response> {
    state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .login_budget
        .admit(request, Instant::now())
}

const fn authentication_outcome(status: u16) -> &'static str {
    match status {
        200..=399 => "success",
        400 | 404 | 405 | 415 => "malformed_request",
        401 => "invalid_identity",
        403 => "origin_or_csrf_denied",
        429 => "rate_limited",
        _ => "service_unavailable",
    }
}

fn now_ms() -> i64 {
    i64::try_from(super::unix_time_ms()).unwrap_or(i64::MAX)
}

fn audit(
    state: &ServerState,
    request: &Request,
    identity: Option<&Identity>,
    action: &str,
    result: &str,
) {
    let classification = state
        .network_access
        .classify(request.remote_addr().ip(), request.headers());
    let id = identity.map(|identity| identity.id.to_string());
    super::record_access_audit(
        state,
        now_ms(),
        id.as_deref(),
        identity.map(|identity| identity.role),
        action,
        None,
        result,
        classification.reason,
    );
}

fn secure_transport(request: &Request, state: &ServerState) -> Result<(), Response> {
    secure_channel(request, state)?;
    if header(request, "Sec-Fetch-Site", 32)?
        .is_some_and(|site| !matches!(site, "same-origin" | "none"))
    {
        return Err(api_status(
            403,
            "cross-site authentication requests are not allowed",
        ));
    }
    Ok(())
}

fn secure_channel(request: &Request, state: &ServerState) -> Result<(), Response> {
    use crate::access::ClientClassificationReason;
    let classification = state
        .network_access
        .classify(request.remote_addr().ip(), request.headers());
    if !request.is_secure()
        && !matches!(
            classification.reason,
            ClientClassificationReason::TrustedProxyLocal
                | ClientClassificationReason::TrustedProxyRemote
        )
    {
        return Err(api_status(
            426,
            "browser authentication requires HTTPS or a trusted proxy",
        ));
    }
    Ok(())
}

fn bootstrap(request: &Request, state: &ServerState) -> Result<Response, Response> {
    let classification = state
        .network_access
        .classify(request.remote_addr().ip(), request.headers());
    if classification.local {
        return Ok(Response::json(
            &serde_json::json!({"local": true, "bearer_enabled": false,
            "methods": [], "identity": {"id": "local-administrator", "display_name": "Local Administrator", "role": "administrator"}, "csrf_token": null}),
        ));
    }
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    bootstrap_transport(request, state, registry.config.is_some())?;
    let Some(config) = registry.config.clone() else {
        // ponytail: These cookies cannot authorize access after external authentication is disabled.
        return Ok(clear_browser_cookies(Response::json(
            &serde_json::json!({"local": false, "bearer_enabled": true,
            "methods": [], "identity": null, "csrf_token": null}),
        )));
    };
    let origin = request_origin(request, &config)?;
    let handle = cookie(request, SESSION_COOKIE)?;
    let authorization = header(request, "Authorization", 4_096)?;
    if authorization.is_some() && handle.is_some() {
        return Err(api_status(400, "authentication methods cannot be combined"));
    }
    if let Some(response) = bootstrap_existing(request, &mut registry, &config, &origin, handle) {
        return Ok(response);
    }
    if handle.is_none() {
        let assertion = proxy_assertion(request, &config)?;
        if assertion.is_some() && authorization.is_some() {
            return Err(api_status(400, "authentication methods cannot be combined"));
        }
        if let Some(assertion) = assertion {
            let (identity, response) = bootstrap_proxy(
                request,
                state,
                &mut registry,
                &config,
                &origin,
                classification.effective_address,
                assertion,
            )?;
            drop(registry);
            drop(_update);
            audit(state, request, Some(&identity), "proxy_login", "success");
            return Ok(response);
        }
    }
    let response = bootstrap_anonymous(
        request,
        &mut registry,
        &config,
        &origin,
        classification.effective_address,
    )?;
    Ok(if handle.is_some() {
        response.with_additional_header("Set-Cookie", session_cookie("", 0))
    } else {
        response
    })
}

fn bootstrap_transport(
    request: &Request,
    state: &ServerState,
    external_authentication: bool,
) -> Result<(), Response> {
    if external_authentication || state.require_secure_remote {
        secure_transport(request, state)?;
    }
    Ok(())
}

fn bootstrap_existing(
    request: &Request,
    registry: &mut Registry,
    config: &external::Config,
    origin: &str,
    handle: Option<&str>,
) -> Option<Response> {
    if let Some(browser) =
        handle.and_then(|handle| registry.sessions.lookup(handle, origin, Instant::now()))
    {
        if let Some(identity) = browser.binding.and_then(|binding| {
            registry
                .directory
                .active(binding.identity_id, binding.revision)
                .cloned()
        }) && proxy_assertion(request, config)
            .and_then(|assertion| corroborate_proxy(config, &identity, assertion.as_ref()))
            .is_ok()
            && renew_browser(registry, &browser, Instant::now()).is_ok()
        {
            return Some(bootstrap_response(config, Some(&identity), browser.csrf()));
        }
        registry.sessions.revoke(browser.id);
        registry.transactions.revoke_browser(browser.id);
    }
    None
}

fn bootstrap_proxy(
    request: &Request,
    state: &ServerState,
    registry: &mut Registry,
    config: &external::Config,
    origin: &str,
    address: IpAddr,
    assertion: crate::access::proxy_identity::Assertion,
) -> Result<(Identity, Response), Response> {
    registry.login_budget.admit(request, Instant::now())?;
    let identity = persist_identity(
        state,
        registry,
        IdentityInput {
            provider_id: &assertion.provider_id,
            namespace: &assertion.namespace,
            subject: &assertion.subject,
            display_name: &assertion.display_name,
            grant: assertion.grant,
            now_ms: now_ms(),
        },
    )?;
    let issued = issue_identity(registry, &identity, origin, address)?;
    let response = bootstrap_response(config, Some(&identity), &issued.csrf)
        .with_additional_header(
            "Set-Cookie",
            session_cookie(
                &issued.cookie,
                state.api_session_policy.absolute_timeout.as_secs(),
            ),
        );
    Ok((identity, response))
}

fn bootstrap_response(
    config: &external::Config,
    identity: Option<&Identity>,
    csrf: &str,
) -> Response {
    let methods = config.providers.iter().map(|provider| serde_json::json!({
        "id": provider.id, "name": provider.name, "kind": match provider.method { external::Method::Oidc(_) => "oidc", external::Method::Proxy(_) => "proxy" }
    })).collect::<Vec<_>>();
    let identity = identity.map(|identity| serde_json::json!({"id": identity.id,
        "display_name": identity.display_name, "provider_id": identity.provider_id, "role": identity.role}));
    Response::json(
        &serde_json::json!({"local": false, "methods": methods, "identity": identity,
        "csrf_token": csrf, "bearer_enabled": config.bearer_enabled && config.bearer_transition_until_ms.is_some_and(|until| now_ms() < until)}),
    )
}

fn bootstrap_anonymous(
    request: &Request,
    registry: &mut Registry,
    config: &external::Config,
    origin: &str,
    address: std::net::IpAddr,
) -> Result<Response, Response> {
    if let Some(browser) = cookie(request, LOGIN_COOKIE)?.and_then(|handle| {
        registry
            .sessions
            .authenticate(handle, origin, Instant::now())
    }) && browser.binding.is_none()
    {
        return Ok(bootstrap_response(config, None, browser.csrf()));
    }
    registry.login_budget.admit(request, Instant::now())?;
    let issued = registry
        .sessions
        .issue(None, origin, address, Instant::now(), now_ms())
        .map_err(|_| api_status(429, "browser bootstrap capacity exceeded"))?;
    Ok(bootstrap_response(config, None, &issued.csrf)
        .with_additional_header("Set-Cookie", login_cookie(&issued.cookie)))
}

fn persist_identity(
    state: &ServerState,
    registry: &mut Registry,
    input: IdentityInput<'_>,
) -> Result<Identity, Response> {
    let mut candidate = registry.directory.clone();
    let provider = registry
        .config
        .as_ref()
        .and_then(|config| {
            config
                .providers
                .iter()
                .find(|provider| provider.id == input.provider_id)
        })
        .ok_or_else(|| api_status(403, "external identity provider is unavailable"))?;
    let identity = candidate
        .provision_authenticated(input, provider)
        .map_err(|_| api_status(403, "external identity is denied or at capacity"))?;
    if candidate == registry.directory {
        return Ok(identity);
    }
    save_directory(state, registry, candidate)
        .map_err(|_| api_status(503, "external identity could not be saved"))?;
    Ok(identity)
}

fn save_directory(
    state: &ServerState,
    registry: &mut Registry,
    candidate: Directory,
) -> anyhow::Result<()> {
    let path = state
        .camera_config_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("identity storage is unavailable"))?;
    let config = crate::config::load_config(path)?;
    anyhow::ensure!(
        config.external_auth == registry.config,
        "external authentication changed on disk"
    );
    let mut root = config.source;
    anyhow::ensure!(
        Directory::from_root(&root)? == registry.directory,
        "identity directory changed on disk"
    );
    root.insert(
        crate::access::identities::SECTION.into(),
        toml::Value::try_from(&candidate)?,
    );
    crate::config::write_configuration_table(path, &root)?;
    registry.directory = candidate;
    Ok(())
}

fn issue_identity(
    registry: &mut Registry,
    identity: &Identity,
    origin: &str,
    address: std::net::IpAddr,
) -> Result<crate::access::browser_sessions::Issued, Response> {
    registry
        .sessions
        .issue(
            Some(Binding {
                identity_id: identity.id,
                revision: identity.revision,
            }),
            origin,
            address,
            Instant::now(),
            now_ms(),
        )
        .map_err(|_| api_status(429, "browser session capacity exceeded"))
}

fn logout(request: &Request, state: &ServerState) -> Result<Response, Response> {
    secure_transport(request, state)?;
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let config = registry
        .config
        .clone()
        .ok_or_else(|| api_status(400, "external authentication is disabled"))?;
    let origin = request_origin(request, &config)?;
    let session = cookie(request, SESSION_COOKIE)?
        .and_then(|handle| registry.sessions.lookup(handle, &origin, Instant::now()));
    let bootstrap = cookie(request, LOGIN_COOKIE)?
        .and_then(|handle| registry.sessions.lookup(handle, &origin, Instant::now()));
    let binding = session
        .as_ref()
        .filter(|browser| {
            browser.binding.is_some_and(|binding| {
                registry
                    .directory
                    .active(binding.identity_id, binding.revision)
                    .is_some()
            })
        })
        .or(bootstrap.as_ref())
        .ok_or_else(|| api_status(403, "logout requires a CSRF binding"))?;
    validate_csrf(request, &origin, binding.csrf())?;
    let logout_uri = binding
        .binding
        .and_then(|binding| {
            registry
                .directory
                .active(binding.identity_id, binding.revision)
        })
        .and_then(|identity| {
            config
                .providers
                .iter()
                .find(|provider| provider.id == identity.provider_id)
        })
        .and_then(|provider| match &provider.method {
            external::Method::Oidc(settings) => settings.logout_uri.clone(),
            external::Method::Proxy(_) => None,
        });
    for browser in [session, bootstrap].into_iter().flatten() {
        registry.sessions.revoke(browser.id);
        registry.transactions.revoke_browser(browser.id);
    }
    drop(registry);
    super::expire_api_sessions(state);
    audit(state, request, None, "external_logout", "success");
    Ok(clear_browser_cookies(
        logout_uri.map_or_else(Response::empty_204, Response::redirect_303),
    ))
}

fn clear_browser_cookies(response: Response) -> Response {
    response
        .with_additional_header("Set-Cookie", session_cookie("", 0))
        .with_additional_header(
            "Set-Cookie",
            format!("{LOGIN_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{AccessRole, external::Grant};

    #[test]
    fn bearer_transition_cutoff_invalidates_existing_principals_and_media() {
        let (state, _, _) = browser_state();
        let credential = state
            .access_manager
            .create_credential(
                "Migration administrator",
                None,
                AccessRole::Administrator,
                None,
                1,
            )
            .unwrap()
            .metadata;
        let principal = ApiPrincipal {
            identity: ApiPrincipalIdentity::Credential {
                id: credential.id,
                revision: credential.revision,
            },
            display_name: credential.name,
            role: credential.role,
            credential_expires_at_ms: None,
        };
        let deadline = now_ms();
        {
            let mut registry = state.authentication.lock().unwrap();
            let config = registry.config.as_mut().unwrap();
            config.bearer_enabled = true;
            config.bearer_transition_until_ms = Some(deadline);
        }
        assert!(active(&state, &principal, Instant::now(), deadline - 1));
        assert!(!active(&state, &principal, Instant::now(), deadline));
        assert!(super::super::camera_access::for_principal(&state, &principal).is_err());
        state.authentication.lock().unwrap().config = None;
        assert!(active(&state, &principal, Instant::now(), deadline));
    }

    #[test]
    fn authentication_routes_reject_unsupported_methods_and_unknown_paths() {
        let state = ServerState::empty();
        for (method, path, status) in [
            ("GET", "/auth/logout", 405),
            ("POST", "/auth/session", 405),
            ("GET", "/auth/unknown", 404),
        ] {
            let request = Request::fake_https(method, path, vec![], vec![]);
            assert_eq!(handle(&request, &state).unwrap().status_code, status);
        }
    }

    pub(super) fn browser_state() -> (ServerState, String, String) {
        let state = ServerState::empty();
        let mut auth = state.authentication.lock().unwrap();
        auth.config = Some(
            toml::from_str(
                r#"
allowed_origins = ["https://keeppeek.example"]
[[providers]]
id = "company"
name = "Company"
[[providers.mappings]]
claim = "groups"
value = "users"
role = "user"
[providers.mappings.camera_access]
all_cameras = false
camera_ids = ["front"]
[providers.method]
kind = "oidc"
issuer = "https://identity.example"
client_id = "keeppeek"
redirect_uri = "https://keeppeek.example/auth/callback"
"#,
            )
            .unwrap(),
        );
        let identity = auth
            .directory
            .provision(IdentityInput {
                provider_id: "company",
                namespace: "https://identity.example",
                subject: "alice",
                display_name: "Alice",
                grant: Grant {
                    role: AccessRole::User,
                    camera_access: CameraAccess {
                        all_cameras: false,
                        group_ids: vec![],
                        camera_ids: vec!["front".into()],
                    },
                },
                now_ms: 1,
            })
            .unwrap();
        let issued = auth
            .sessions
            .issue(
                Some(Binding {
                    identity_id: identity.id,
                    revision: identity.revision,
                }),
                "https://keeppeek.example",
                "203.0.113.1".parse().unwrap(),
                Instant::now(),
                1,
            )
            .unwrap();
        drop(auth);
        (state, issued.cookie, issued.csrf)
    }

    pub(super) fn remote_request(method: &str, cookie: &str, csrf: Option<&str>) -> Request {
        let mut headers = vec![
            ("Host".into(), "keeppeek.example".into()),
            ("Cookie".into(), format!("{SESSION_COOKIE}={cookie}")),
            ("Origin".into(), "https://keeppeek.example".into()),
        ];
        if let Some(csrf) = csrf {
            headers.push(("X-KeepPeek-CSRF".into(), csrf.into()));
        }
        Request::fake_https_from(
            "203.0.113.1:4567".parse().unwrap(),
            method,
            "/create",
            headers,
            vec![],
        )
    }

    #[test]
    fn session_metadata_identifies_methods_without_provider_claims() {
        use crate::api::proto::AccessAuthenticationMethod as Method;
        let (state, cookie, _) = browser_state();
        let local = ApiPrincipal::local("127.0.0.1".parse().unwrap());
        let metadata = session_metadata(&state, &local).unwrap();
        assert_eq!(metadata.method, Method::TrustedLocal as i32);
        assert!(metadata.provider_id.is_none() && metadata.identity_id.is_none());
        let mut owner = local;
        owner.identity = ApiPrincipalIdentity::Credential {
            id: Uuid::new_v4(),
            revision: 1,
        };
        let metadata = session_metadata(&state, &owner).unwrap();
        assert_eq!(metadata.method, Method::Bearer as i32);
        assert!(metadata.provider_id.is_none() && metadata.identity_id.is_none());
        let identity = state.authentication.lock().unwrap().directory.records[0].clone();
        let browser = state
            .authentication
            .lock()
            .unwrap()
            .sessions
            .lookup(&cookie, "https://keeppeek.example", Instant::now())
            .unwrap();
        owner = principal(&identity, browser.id, i64::MAX);
        let metadata = session_metadata(&state, &owner).unwrap();
        assert_eq!(metadata.method, Method::Oidc as i32);
        assert_eq!(metadata.provider_id.as_deref(), Some("company"));
        assert_eq!(metadata.identity_id, Some(identity.id.to_string()));
        state
            .authentication
            .lock()
            .unwrap()
            .directory
            .revoke(identity.id)
            .unwrap();
        assert!(session_metadata(&state, &owner).is_none());
    }

    #[test]
    fn session_metadata_does_not_relabel_a_browser_after_configuration_replacement() {
        let (state, cookie, csrf) = browser_state();
        let owner =
            super::super::api_principal(&remote_request("POST", &cookie, Some(&csrf)), &state)
                .unwrap()
                .principal;
        let mut replacement = Registry::new(&Config::default(), state.api_session_policy);
        {
            let registry = state.authentication.lock().unwrap();
            replacement.config = registry.config.clone();
            replacement.directory = registry.directory.clone();
        }
        replacement.config.as_mut().unwrap().providers[0].method =
            external::Method::Proxy(external::Proxy {
                trusted_peers: vec!["203.0.113.1/32".parse().unwrap()],
                subject_header: "X-Identity-Subject".into(),
                role_header: "X-Identity-Role".into(),
                name_header: None,
                secret_header: None,
                shared_secret: None,
            });
        activate_configuration(&state, Some(replacement));
        assert!(session_metadata(&state, &owner).is_none());
        let proxy_owner = {
            let mut registry = state.authentication.lock().unwrap();
            let identity = registry.directory.records[0].clone();
            let issued = registry
                .sessions
                .issue(
                    Some(Binding {
                        identity_id: identity.id,
                        revision: identity.revision,
                    }),
                    "https://keeppeek.example",
                    "203.0.113.1".parse().unwrap(),
                    Instant::now(),
                    now_ms(),
                )
                .unwrap();
            let browser = registry
                .sessions
                .lookup(&issued.cookie, "https://keeppeek.example", Instant::now())
                .unwrap();
            principal(&identity, browser.id, browser.absolute_expires_at_ms)
        };
        assert_eq!(
            session_metadata(&state, &proxy_owner).unwrap().method,
            crate::api::proto::AccessAuthenticationMethod::Proxy as i32
        );
        assert!(session_metadata(&state, &owner).is_none());
    }

    pub(super) fn proxy_state() -> (ServerState, std::path::PathBuf) {
        let (mut state, _, _) = browser_state();
        let directory = std::env::temp_dir().join(format!("keeppeek-proxy-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        let mut auth = state.authentication.lock().unwrap();
        auth.directory = Directory::default();
        let config = auth.config.as_mut().unwrap();
        config.providers[0].method = external::Method::Proxy(
            toml::from_str(
                r#"
trusted_peers = ["203.0.113.1/32"]
subject_header = "X-Identity-Subject"
role_header = "X-Identity-Role"
"#,
            )
            .unwrap(),
        );
        config.providers[0].mappings[0].claim = "role".into();
        let mut root = toml::Table::new();
        root.insert(
            "external_auth".into(),
            toml::Value::try_from(config.clone()).unwrap(),
        );
        crate::config::write_configuration_table(&path, &root).unwrap();
        drop(auth);
        state.camera_config_path = Some(path);
        (state, directory)
    }

    #[test]
    fn proxy_bootstrap_persists_identity_and_logout_revokes_its_cookie() {
        let (state, directory) = proxy_state();
        let path = directory.join("config.toml");
        let headers = vec![
            ("Host".into(), "keeppeek.example".into()),
            ("X-Identity-Subject".into(), "synthetic-alice".into()),
            ("X-Identity-Role".into(), "users".into()),
        ];
        let request = Request::fake_https_from(
            "203.0.113.1:4567".parse().unwrap(),
            "GET",
            "/auth/session",
            headers.clone(),
            vec![],
        );
        let response = handle(&request, &state).unwrap();
        assert_eq!(response.status_code, 200);
        let cookie = response
            .headers
            .iter()
            .find(|(name, value)| name == "Set-Cookie" && value.starts_with(SESSION_COOKIE))
            .unwrap()
            .1
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1
            .to_owned();
        let body: serde_json::Value =
            serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
        let csrf = body["csrf_token"].as_str().unwrap();
        assert_eq!(body["identity"]["role"], "user");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("synthetic-alice"));
        assert_eq!(
            Directory::from_root(&crate::config::load_configuration_table(&path).unwrap())
                .unwrap()
                .records
                .len(),
            1
        );
        let mut headers = headers;
        headers.extend([
            ("Cookie".into(), format!("{SESSION_COOKIE}={cookie}")),
            ("Origin".into(), "https://keeppeek.example".into()),
            ("X-KeepPeek-CSRF".into(), csrf.into()),
        ]);
        let request = Request::fake_https_from(
            "203.0.113.1:4567".parse().unwrap(),
            "POST",
            "/auth/logout",
            headers,
            vec![],
        );
        let principal = super::super::api_principal(&request, &state)
            .unwrap()
            .principal;
        assert_eq!(handle(&request, &state).unwrap().status_code, 204);
        assert!(!active(&state, &principal, Instant::now(), 2));
        assert!(super::super::api_principal(&request, &state).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn remote_http_cookie_principal_enforces_csrf_ambiguity_and_live_revocation() {
        let (state, cookie, csrf) = browser_state();
        let valid = remote_request("POST", &cookie, Some(&csrf));
        let identity = super::super::api_principal(&valid, &state).unwrap();
        assert_eq!(identity.principal.role, AccessRole::User);
        assert!(
            super::super::api_principal(&remote_request("POST", &cookie, None), &state).is_err()
        );
        let ambiguous = Request::fake_https_from(
            "203.0.113.1:4567".parse().unwrap(),
            "GET",
            "/metrics",
            vec![
                ("Host".into(), "keeppeek.example".into()),
                ("Cookie".into(), format!("{SESSION_COOKIE}={cookie}")),
                ("Authorization".into(), "Bearer synthetic".into()),
            ],
            vec![],
        );
        assert!(super::super::api_principal(&ambiguous, &state).is_err());
        let ApiPrincipalIdentity::External { id, .. } = identity.principal.identity else {
            panic!("wrong identity kind");
        };
        state
            .authentication
            .lock()
            .unwrap()
            .directory
            .revoke(id)
            .unwrap();
        assert!(super::super::api_principal(&valid, &state).is_err());
    }

    #[test]
    fn proxy_downgrade_revokes_the_browser_binding_not_just_one_http_request() {
        for bootstrap_request in [false, true] {
            let (state, cookie, csrf) = browser_state();
            let mut registry = state.authentication.lock().unwrap();
            let identity = &mut registry.directory.records[0];
            identity.role = AccessRole::Administrator;
            identity.camera_access = CameraAccess::unrestricted();
            identity.subject_fingerprint = external::subject_fingerprint("proxy:company", "alice");
            let identity = identity.clone();
            let browser = registry
                .sessions
                .authenticate(&cookie, "https://keeppeek.example", Instant::now())
                .unwrap();
            let owner = principal(&identity, browser.id, browser.absolute_expires_at_ms);
            let config = registry.config.as_mut().unwrap();
            config.providers[0].method = external::Method::Proxy(
                toml::from_str(
                    r#"
trusted_peers = ["203.0.113.1/32"]
subject_header = "X-Identity-Subject"
role_header = "X-Identity-Role"
"#,
                )
                .unwrap(),
            );
            config.providers[0].mappings[0].claim = "role".into();
            drop(registry);
            let request = Request::fake_https_from(
                "203.0.113.1:4567".parse().unwrap(),
                "POST",
                "/create",
                vec![
                    ("Host".into(), "keeppeek.example".into()),
                    ("Cookie".into(), format!("{SESSION_COOKIE}={cookie}")),
                    ("Origin".into(), "https://keeppeek.example".into()),
                    ("X-KeepPeek-CSRF".into(), csrf),
                    ("X-Identity-Subject".into(), "alice".into()),
                    ("X-Identity-Role".into(), "users".into()),
                ],
                vec![],
            );
            if bootstrap_request {
                let _ = bootstrap(&request, &state);
            } else {
                assert!(super::super::api_principal(&request, &state).is_err());
            }
            assert!(!active(&state, &owner, Instant::now(), now_ms()));
        }
    }

    #[test]
    fn revoked_browser_cannot_finish_a_previously_authenticated_signaling_request() {
        use std::io::Write;
        let (state, cookie, csrf) = browser_state();
        let identity =
            super::super::api_principal(&remote_request("POST", &cookie, Some(&csrf)), &state)
                .unwrap();
        logout(&remote_request("POST", &cookie, Some(&csrf)), &state).unwrap();
        let offer = serde_json::json!({"offer": {"type": "offer", "sdp": crate::webrtc::test_api_offer().to_sdp_string()}});
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&serde_json::to_vec(&offer).unwrap())
            .unwrap();
        let request = Request::fake_https_from(
            "203.0.113.1:4567".parse().unwrap(),
            "POST",
            "/create",
            vec![("Content-Encoding".into(), "gzip".into())],
            gzip.finish().unwrap(),
        );
        let response = super::super::create_api_session(&request, &state, identity);
        assert_eq!(response.status_code, 401);
        assert!(state.api_session_owners.lock().unwrap().is_empty());
        state.webrtc.shutdown();
    }

    #[test]
    fn disabled_external_authentication_bootstrap_clears_unusable_cookies() {
        let (state, cookie, _) = browser_state();
        state.authentication.lock().unwrap().config = None;
        let response = bootstrap(&remote_request("GET", &cookie, None), &state).unwrap();
        assert_eq!(response.status_code, 200);
        assert_eq!(
            response
                .headers
                .iter()
                .filter(|(name, value)| name == "Set-Cookie" && value.contains("Max-Age=0"))
                .count(),
            2
        );
        let body: serde_json::Value =
            serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
        assert_eq!(body["bearer_enabled"], true);
        assert!(body["identity"].is_null());
    }

    #[test]
    fn rejected_logout_does_not_renew_the_browser_idle_timeout() {
        let (state, _, _) = browser_state();
        let issued = {
            let mut registry = state.authentication.lock().unwrap();
            let identity = registry.directory.records[0].clone();
            registry
                .sessions
                .issue(
                    Some(Binding {
                        identity_id: identity.id,
                        revision: identity.revision,
                    }),
                    "https://keeppeek.example",
                    "203.0.113.1".parse().unwrap(),
                    Instant::now() - std::time::Duration::from_secs(3),
                    1,
                )
                .unwrap()
        };
        let request = remote_request("POST", &issued.cookie, Some("wrong"));
        assert_eq!(logout(&request, &state).unwrap_err().status_code, 403);
        let browser = state
            .authentication
            .lock()
            .unwrap()
            .sessions
            .lookup(&issued.cookie, "https://keeppeek.example", Instant::now())
            .unwrap();
        assert_eq!(browser.last_activity_at_ms(), 1);
    }

    #[test]
    fn oidc_logout_redirects_only_after_revoking_local_access() {
        let (state, cookie, csrf) = browser_state();
        let owner = super::super::api_principal(&remote_request("GET", &cookie, None), &state)
            .unwrap()
            .principal;
        {
            let mut registry = state.authentication.lock().unwrap();
            let external::Method::Oidc(settings) =
                &mut registry.config.as_mut().unwrap().providers[0].method
            else {
                panic!()
            };
            settings.logout_uri = Some("https://identity.example/logout".into());
        }
        let response = logout(&remote_request("POST", &cookie, Some(&csrf)), &state).unwrap();
        assert_eq!(response.status_code, 303);
        assert!(
            response
                .headers
                .iter()
                .any(|(name, value)| name == "Location"
                    && value == "https://identity.example/logout")
        );
        assert!(!active(&state, &owner, Instant::now(), now_ms()));
        assert_eq!(
            response
                .headers
                .iter()
                .filter(|(name, value)| name == "Set-Cookie" && value.contains("Max-Age=0"))
                .count(),
            2
        );
    }

    #[test]
    fn stale_identity_can_logout_using_the_returned_anonymous_csrf_binding() {
        let (state, cookie, _) = browser_state();
        {
            let mut registry = state.authentication.lock().unwrap();
            let id = registry.directory.records[0].id;
            registry.directory.revoke(id).unwrap();
        }
        let mut request = remote_request("GET", &cookie, None);
        let response = bootstrap(&request, &state).unwrap();
        let login_cookie = response
            .headers
            .iter()
            .find(|(name, value)| name == "Set-Cookie" && value.starts_with(LOGIN_COOKIE))
            .unwrap()
            .1
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let body: serde_json::Value =
            serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
        request = Request::fake_https_from(
            "203.0.113.1:4567".parse().unwrap(),
            "POST",
            "/auth/logout",
            vec![
                ("Host".into(), "keeppeek.example".into()),
                (
                    "Cookie".into(),
                    format!("{SESSION_COOKIE}={cookie}; {login_cookie}"),
                ),
                ("Origin".into(), "https://keeppeek.example".into()),
                (
                    "X-KeepPeek-CSRF".into(),
                    body["csrf_token"].as_str().unwrap().into(),
                ),
            ],
            vec![],
        );
        assert_eq!(logout(&request, &state).unwrap().status_code, 204);
    }

    #[test]
    fn principal_capacity_uses_stable_identity_but_ownership_keeps_browser_binding() {
        let (state, cookie, _) = browser_state();
        let owner = super::super::api_principal(&remote_request("GET", &cookie, None), &state)
            .unwrap()
            .principal;
        let mut other = owner.clone();
        let ApiPrincipalIdentity::External { browser, .. } = &mut other.identity else {
            panic!();
        };
        *browser = Uuid::new_v4();
        assert!(owner.same_identity(&other));
        assert!(!owner.owns_session(&other));
        other = owner.clone();
        other.display_name = "Changed metadata".into();
        assert!(owner.owns_session(&other));
    }

    #[test]
    fn expiry_between_lookup_and_renewal_cannot_return_an_authenticated_identity() {
        let (state, cookie, _) = browser_state();
        let mut registry = state.authentication.lock().unwrap();
        let now = Instant::now();
        let browser = registry
            .sessions
            .lookup(&cookie, "https://keeppeek.example", now)
            .unwrap();
        let after_expiry = now + state.api_session_policy.absolute_timeout;
        assert!(renew_browser(&mut registry, &browser, after_expiry).is_err());
        assert!(
            !registry
                .sessions
                .active(browser.id, browser.binding.unwrap(), after_expiry)
        );
    }

    #[test]
    fn external_principal_requires_both_live_identity_and_browser_session() {
        let state = super::super::ServerState::empty();
        let mut authentication = state.authentication.lock().unwrap();
        let identity = authentication
            .directory
            .provision(IdentityInput {
                provider_id: "company",
                namespace: "proxy:company",
                subject: "alice",
                display_name: "Alice",
                grant: Grant {
                    role: AccessRole::User,
                    camera_access: CameraAccess {
                        all_cameras: false,
                        group_ids: vec![],
                        camera_ids: vec!["front".into()],
                    },
                },
                now_ms: 1,
            })
            .unwrap();
        let now = std::time::Instant::now();
        let issued = authentication
            .sessions
            .issue(
                Some(Binding {
                    identity_id: identity.id,
                    revision: identity.revision,
                }),
                "https://keeppeek.example",
                "203.0.113.1".parse().unwrap(),
                now,
                1,
            )
            .unwrap();
        let principal = principal(&identity, issued.session_id, 1_000_000);
        drop(authentication);
        assert!(active(&state, &principal, now, 2));
        assert_eq!(
            camera_policy(&state, &principal).unwrap().camera_ids,
            ["front"]
        );
        state
            .authentication
            .lock()
            .unwrap()
            .sessions
            .revoke(issued.session_id);
        assert!(!active(&state, &principal, now, 2));
        assert!(camera_policy(&state, &principal).is_none());
    }

    #[test]
    fn cookies_and_origin_csrf_reject_duplicate_and_cross_site_inputs() {
        let request = rouille::Request::fake_http(
            "POST",
            "https://keeppeek.example/create",
            vec![(
                "Cookie".into(),
                "__Host-keeppeek-session=one; __Host-keeppeek-session=two".into(),
            )],
            vec![],
        );
        assert!(cookie(&request, SESSION_COOKIE).is_err());
        let request = rouille::Request::fake_http(
            "POST",
            "https://keeppeek.example/create",
            vec![
                ("Origin".into(), "https://evil.example".into()),
                ("X-KeepPeek-CSRF".into(), "correct".into()),
            ],
            vec![],
        );
        assert!(validate_csrf(&request, "https://keeppeek.example", "correct").is_err());
        let request = rouille::Request::fake_http(
            "POST",
            "https://keeppeek.example/create",
            vec![
                ("Origin".into(), "https://keeppeek.example".into()),
                ("X-KeepPeek-CSRF".into(), "correct".into()),
            ],
            vec![],
        );
        assert!(validate_csrf(&request, "https://keeppeek.example", "correct").is_ok());
        assert!(validate_csrf(&request, "https://keeppeek.example", "wrong").is_err());
        let serialized = session_cookie("opaque", 300);
        for attribute in ["Secure", "HttpOnly", "SameSite=Lax", "Path=/"] {
            assert!(serialized.contains(attribute));
        }
        assert!(!serialized.contains("Domain="));
    }
}
