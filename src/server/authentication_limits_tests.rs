use super::*;
use std::sync::atomic::Ordering;

fn exhaust_address(state: &ServerState) {
    state
        .authentication
        .lock()
        .unwrap()
        .login_budget
        .addresses
        .insert(
            "203.0.113.1".parse().unwrap(),
            AttemptWindow {
                started: Instant::now(),
                attempts: LOGIN_ADDRESS_ATTEMPTS,
            },
        );
}

#[test]
fn proxy_creation_denial_precedes_disk_provisioning() {
    let (state, directory) = tests::proxy_state();
    let path = directory.join("config.toml");
    let before = std::fs::read(&path).unwrap();
    exhaust_address(&state);
    let request = Request::fake_https_from(
        "203.0.113.1:1".parse().unwrap(),
        "GET",
        "/auth/session",
        vec![
            ("Host".into(), "keeppeek.example".into()),
            ("X-Identity-Subject".into(), "synthetic-alice".into()),
            ("X-Identity-Role".into(), "users".into()),
        ],
        vec![],
    );
    let response = handle(&request, &state).unwrap();
    assert_eq!(response.status_code, 429);
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, _)| name == "Set-Cookie")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        state
            .authentication
            .lock()
            .unwrap()
            .directory
            .records
            .is_empty()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn anonymous_creation_denial_issues_no_cookie() {
    let (state, _, _) = tests::browser_state();
    exhaust_address(&state);
    let response = handle(
        &request("GET", "/auth/session", "203.0.113.1:1", None, ""),
        &state,
    )
    .unwrap();
    assert_eq!(response.status_code, 429);
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, _)| name == "Set-Cookie")
    );
}

#[test]
fn missing_browser_is_unauthorized_until_address_budget_is_exhausted() {
    let (state, _, _) = tests::browser_state();
    let request = request(
        "POST",
        "/auth/login",
        "203.0.113.1:1",
        None,
        "provider_id=company&csrf_token=synthetic&return_path=/",
    );
    assert_eq!(handle(&request, &state).unwrap().status_code, 401);
    exhaust_address(&state);
    assert_eq!(handle(&request, &state).unwrap().status_code, 429);
}

fn request(method: &str, path: &str, peer: &str, cookie: Option<&str>, body: &str) -> Request {
    let mut headers = vec![
        ("Host".into(), "keeppeek.example".into()),
        ("Origin".into(), "https://keeppeek.example".into()),
        (
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ),
    ];
    if let Some(cookie) = cookie {
        headers.push(("Cookie".into(), cookie.into()));
    }
    Request::fake_https_from(
        peer.parse().unwrap(),
        method,
        path,
        headers,
        body.as_bytes().to_vec(),
    )
}

fn anonymous(state: &ServerState) -> (String, String) {
    let response = handle(
        &request("GET", "/auth/session", "203.0.113.1:1", None, ""),
        state,
    )
    .unwrap();
    assert_eq!(response.status_code, 200);
    let cookie = response
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
    (cookie, body["csrf_token"].as_str().unwrap().to_owned())
}

#[test]
fn login_address_limit_covers_cookie_free_candidate_start_and_callback() {
    let state = ServerState::empty();
    for attempt in 0..LOGIN_ADDRESS_ATTEMPTS {
        let (method, path, body) = if attempt % 2 == 0 {
            ("POST", "/auth/login", "candidate_plan_id=invalid")
        } else {
            (
                "GET",
                "/auth/callback?state=synthetic-state&code=synthetic-code",
                "",
            )
        };
        let response = handle(&request(method, path, "203.0.113.1:1", None, body), &state).unwrap();
        assert_ne!(response.status_code, 429);
    }
    for (method, path) in [("POST", "/auth/login"), ("GET", "/auth/callback")] {
        assert_eq!(
            handle(&request(method, path, "203.0.113.1:2", None, ""), &state)
                .unwrap()
                .status_code,
            429
        );
    }
    assert_ne!(
        handle(
            &request("GET", "/auth/callback", "203.0.113.2:1", None, ""),
            &state
        )
        .unwrap()
        .status_code,
        429
    );
    assert_eq!(
        state
            .access_metrics
            .authentication_failures
            .load(Ordering::Relaxed),
        33
    );
    let audit = serde_json::to_string(&state.access_manager.list_audit(100)).unwrap();
    assert!(audit.contains("external_login_start"));
    assert!(audit.contains("external_login_callback"));
    assert!(audit.contains("rate_limited"));
    assert!(!audit.contains("synthetic-state"));
    assert!(!audit.contains("synthetic-code"));
}

#[test]
fn browser_limit_follows_cookie_across_peers_and_precedes_provider_work() {
    let (state, _, _) = tests::browser_state();
    let (cookie, csrf) = anonymous(&state);
    let _cache = state.oidc_cache.lock().unwrap();
    for attempt in 0..LOGIN_BROWSER_ATTEMPTS {
        let form = format!("provider_id=company&csrf_token={csrf}&return_path=/");
        let peer = format!("203.0.113.{}:1", attempt + 2);
        let response = handle(
            &request("POST", "/auth/login", &peer, Some(&cookie), &form),
            &state,
        )
        .unwrap();
        // A held provider cache rejects discovery immediately, without network requests.
        assert_eq!(response.status_code, 503);
    }
    let response = handle(
        &request("POST", "/auth/login", "203.0.113.99:1", Some(&cookie), ""),
        &state,
    )
    .unwrap();
    assert_eq!(response.status_code, 429);
    let hash: [u8; 32] = Sha256::digest(cookie.split_once('=').unwrap().1.as_bytes()).into();
    assert!(
        state
            .authentication
            .lock()
            .unwrap()
            .login_budget
            .browsers
            .contains_key(&hash)
    );
}

#[test]
fn invalid_return_path_is_rejected_before_provider_discovery() {
    let (state, _, _) = tests::browser_state();
    let (cookie, csrf) = anonymous(&state);
    let _cache = state.oidc_cache.lock().unwrap();
    for path in [
        "https://evil.example",
        "//evil.example",
        "/%2f%2fevil.example",
        "/auth/callback",
    ] {
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("provider_id", "company")
            .append_pair("csrf_token", &csrf)
            .append_pair("return_path", path)
            .finish();
        let response = handle(
            &request("POST", "/auth/login", "203.0.113.1:1", Some(&cookie), &form),
            &state,
        )
        .unwrap();
        assert_eq!(response.status_code, 400, "{path}");
    }
    let form = format!("provider_id=company&csrf_token={csrf}&return_path=/events");
    assert_eq!(
        handle(
            &request("POST", "/auth/login", "203.0.113.1:1", Some(&cookie), &form),
            &state
        )
        .unwrap()
        .status_code,
        503
    );
}

#[test]
fn polling_authenticated_and_existing_anonymous_sessions_does_not_charge_attempts() {
    let (state, session, _) = tests::browser_state();
    let (anonymous, _) = anonymous(&state);
    let session = format!("{SESSION_COOKIE}={session}");
    for cookie in [&session, &anonymous] {
        for _ in 0..LOGIN_ADDRESS_ATTEMPTS + 1 {
            assert_eq!(
                handle(
                    &request("GET", "/auth/session", "203.0.113.1:1", Some(cookie), ""),
                    &state
                )
                .unwrap()
                .status_code,
                200
            );
        }
    }
    let registry = state.authentication.lock().unwrap();
    assert_eq!(
        registry.login_budget.addresses[&"203.0.113.1".parse::<IpAddr>().unwrap()].attempts,
        1
    );
    assert!(registry.login_budget.browsers.is_empty());
}

#[test]
fn configuration_activation_keeps_attempts_including_those_after_preparation() {
    let state = ServerState::empty();
    let before = Config::default();
    let next = prepare_configuration(&state, &before, &before).unwrap();
    for _ in 0..LOGIN_ADDRESS_ATTEMPTS {
        handle(
            &request("GET", "/auth/callback", "203.0.113.1:1", None, ""),
            &state,
        )
        .unwrap();
    }
    activate_configuration(&state, Some(next));
    assert_eq!(
        handle(
            &request("GET", "/auth/callback", "203.0.113.1:1", None, ""),
            &state
        )
        .unwrap()
        .status_code,
        429
    );
}

#[test]
fn fixed_windows_expire_without_evicting_active_addresses_or_browser_hashes() {
    let now = Instant::now();
    let mut budget = LoginBudget::default();
    for index in 0..LOGIN_ADDRESS_LIMIT {
        let address = IpAddr::V4(std::net::Ipv4Addr::from(u32::try_from(index).unwrap()));
        budget.addresses.insert(
            address,
            AttemptWindow {
                started: now,
                attempts: 1,
            },
        );
    }
    let fresh = request("GET", "/auth/callback", "203.0.113.1:1", None, "");
    assert_eq!(budget.admit(&fresh, now).unwrap_err().status_code, 429);
    assert_eq!(budget.addresses.len(), LOGIN_ADDRESS_LIMIT);
    assert!(budget.admit(&fresh, now + LOGIN_WINDOW).is_ok());
    assert_eq!(budget.addresses.len(), 1);
    for index in 0..LOGIN_BROWSER_LIMIT {
        let hash = Sha256::digest(index.to_be_bytes()).into();
        budget.browsers.insert(
            hash,
            AttemptWindow {
                started: now + LOGIN_WINDOW,
                attempts: 1,
            },
        );
    }
    let browser = request(
        "GET",
        "/auth/callback",
        "203.0.113.1:1",
        Some("__Host-keeppeek-login=synthetic-cookie"),
        "",
    );
    assert_eq!(
        budget
            .admit(&browser, now + LOGIN_WINDOW)
            .unwrap_err()
            .status_code,
        429
    );
    assert_eq!(budget.browsers.len(), LOGIN_BROWSER_LIMIT);
    assert!(budget.admit(&browser, now + LOGIN_WINDOW * 2).is_ok());
    assert_eq!(budget.browsers.len(), 1);
}

#[test]
fn mapped_ipv4_and_forwarded_addresses_do_not_bypass_socket_peer_budget() {
    let state = ServerState::empty();
    for index in 0..LOGIN_ADDRESS_ATTEMPTS {
        let request = Request::fake_https_from(
            "203.0.113.1:1".parse().unwrap(),
            "POST",
            "/auth/login",
            vec![("X-Forwarded-For".into(), format!("198.51.100.{index}"))],
            vec![],
        );
        assert_eq!(handle(&request, &state).unwrap().status_code, 415);
    }
    assert_eq!(
        handle(
            &request("POST", "/auth/login", "[::ffff:203.0.113.1]:1", None, ""),
            &state
        )
        .unwrap()
        .status_code,
        429
    );
}
