use super::*;

#[test]
fn bootstrap_transport_preserves_bearer_opt_out_but_requires_tls_for_external_auth() {
    assert!(ServerState::empty().require_secure_remote);
    for (external, require_secure, status) in [
        (false, false, 200),
        (false, true, 426),
        (true, false, 426),
        (true, true, 426),
    ] {
        for spoof_proxy in [false, true] {
            let (mut state, _, _) = tests::browser_state();
            state.require_secure_remote = require_secure;
            if !external {
                state.authentication.lock().unwrap().config = None;
            }
            let mut headers = vec![("Host".into(), "keeppeek.example".into())];
            if spoof_proxy {
                headers.extend([
                    ("X-Forwarded-Proto".into(), "https".into()),
                    ("X-Forwarded-For".into(), "127.0.0.1".into()),
                ]);
            }
            let request = Request::fake_http_from(
                "203.0.113.1:4567".parse().unwrap(),
                "GET",
                "/auth/session",
                headers,
                vec![],
            );
            let response = handle(&request, &state).unwrap();
            assert_eq!(
                response.status_code, status,
                "external={external} require_secure={require_secure} spoof={spoof_proxy}"
            );
            if status == 200 {
                let body: serde_json::Value =
                    serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
                assert_eq!(body["bearer_enabled"], true);
                assert_eq!(body["local"], false);
                assert!(body["identity"].is_null());
            }
        }
    }
}

fn discover(state: &ServerState, cookies: &str) -> Response {
    handle(
        &Request::fake_https_from(
            "203.0.113.1:4567".parse().unwrap(),
            "GET",
            "/auth/session",
            vec![
                ("Host".into(), "keeppeek.example".into()),
                ("Origin".into(), "https://keeppeek.example".into()),
                ("Cookie".into(), cookies.into()),
            ],
            vec![],
        ),
        state,
    )
    .unwrap()
}

#[test]
fn anonymous_bootstrap_without_session_cookie_does_not_expire_one() {
    let (state, _, _) = tests::browser_state();
    let response = discover(&state, "");
    assert_eq!(response.status_code, 200);
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, value)| name == "Set-Cookie" && value.starts_with(SESSION_COOKIE))
    );
    let login = response
        .headers
        .iter()
        .find_map(|(name, value)| {
            (name == "Set-Cookie" && value.starts_with(LOGIN_COOKIE))
                .then(|| value.split(';').next().unwrap().to_owned())
        })
        .unwrap();
    let reused = discover(&state, &login);
    assert_eq!(reused.status_code, 200);
    assert!(
        !reused
            .headers
            .iter()
            .any(|(name, value)| name == "Set-Cookie" && value.starts_with(SESSION_COOKIE))
    );
}

#[test]
fn stale_session_bootstrap_clears_cookie_with_new_and_existing_login_state() {
    for fault in ["invalid", "revoked", "expired"] {
        let (state, handle) = stale_state(fault);
        let stale = format!("{SESSION_COOKIE}={handle}");
        let response = discover(&state, &stale);
        assert_eq!(response.status_code, 200);
        assert!(
            response
                .headers
                .iter()
                .any(|(name, value)| name == "Set-Cookie" && value == &session_cookie("", 0)),
            "{fault}"
        );
        let login = response
            .headers
            .iter()
            .find_map(|(name, value)| {
                (name == "Set-Cookie" && value.starts_with(LOGIN_COOKIE))
                    .then(|| value.split(';').next().unwrap().to_owned())
            })
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
        assert!(body["identity"].is_null());
        let reused = discover(&state, &format!("{stale}; {login}"));
        assert!(
            reused
                .headers
                .iter()
                .any(|(name, value)| name == "Set-Cookie" && value == &session_cookie("", 0))
        );
        assert!(
            super::super::api_principal(&tests::remote_request("GET", &handle, None), &state,)
                .is_err()
        );
    }
}

fn stale_state(fault: &str) -> (ServerState, String) {
    let (state, mut handle, _) = tests::browser_state();
    let mut registry = state.authentication.lock().unwrap();
    match fault {
        "invalid" => handle = "invalid-handle".into(),
        "revoked" => {
            let browser = registry
                .sessions
                .lookup(&handle, "https://keeppeek.example", Instant::now())
                .unwrap();
            registry.sessions.revoke(browser.id);
        }
        "expired" => registry
            .sessions
            .expire(Instant::now() + state.api_session_policy.absolute_timeout),
        _ => unreachable!(),
    }
    drop(registry);
    (state, handle)
}

#[test]
fn stale_session_bootstrap_does_not_fall_back_to_proxy_identity() {
    let (state, directory) = tests::proxy_state();
    let path = directory.join("config.toml");
    let before = std::fs::read(&path).unwrap();
    let request = Request::fake_https_from(
        "203.0.113.1:4567".parse().unwrap(),
        "GET",
        "/auth/session",
        vec![
            ("Host".into(), "keeppeek.example".into()),
            ("Cookie".into(), format!("{SESSION_COOKIE}=invalid-handle")),
            ("X-Identity-Subject".into(), "synthetic-alice".into()),
            ("X-Identity-Role".into(), "users".into()),
        ],
        vec![],
    );
    let response = handle(&request, &state).unwrap();
    assert_eq!(response.status_code, 200);
    let body: serde_json::Value =
        serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
    assert!(body["identity"].is_null());
    assert!(
        state
            .authentication
            .lock()
            .unwrap()
            .directory
            .records
            .is_empty()
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert!(super::super::api_principal(&request, &state).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn valid_session_bootstrap_preserves_cookie_and_identity() {
    let (state, handle, _) = tests::browser_state();
    let response = discover(&state, &format!("{SESSION_COOKIE}={handle}"));
    assert_eq!(response.status_code, 200);
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, _)| name == "Set-Cookie")
    );
    let body: serde_json::Value =
        serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
    assert!(!body["identity"].is_null());
    assert!(
        super::super::api_principal(&tests::remote_request("GET", &handle, None), &state,).is_ok()
    );
}

#[test]
fn recovered_cookie_can_start_oidc_but_stale_cookie_cannot() {
    let fixture = crate::access::oidc::FixtureProvider::new();
    for fault in ["invalid", "revoked", "expired"] {
        let (state, handle) = stale_state(fault);
        state
            .authentication
            .lock()
            .unwrap()
            .config
            .as_mut()
            .unwrap()
            .providers[0]
            .method = external::Method::Oidc(fixture.config.clone());
        state
            .oidc_cache
            .lock()
            .unwrap()
            .get(1, "company", Instant::now(), || {
                crate::access::oidc::Provider::discover(&fixture.config, fixture.transport.clone())
            })
            .unwrap();
        let stale = format!("{SESSION_COOKIE}={handle}");
        let response = discover(&state, &stale);
        let login = response
            .headers
            .iter()
            .find_map(|(name, value)| {
                (name == "Set-Cookie" && value.starts_with(LOGIN_COOKIE))
                    .then(|| value.split(';').next().unwrap().to_owned())
            })
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
        let csrf = body["csrf_token"].as_str().unwrap();
        assert_eq!(
            start_login(&state, &format!("{stale}; {login}"), csrf).status_code,
            400
        );
        assert_eq!(start_login(&state, &login, csrf).status_code, 303);
    }
}

fn start_login(state: &ServerState, cookies: &str, csrf: &str) -> Response {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("provider_id", "company")
        .append_pair("csrf_token", csrf)
        .finish();
    handle(
        &Request::fake_https_from(
            "203.0.113.1:4567".parse().unwrap(),
            "POST",
            "/auth/login",
            vec![
                ("Host".into(), "keeppeek.example".into()),
                ("Origin".into(), "https://keeppeek.example".into()),
                ("Cookie".into(), cookies.into()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
            ],
            form.into_bytes(),
        ),
        state,
    )
    .unwrap()
}
