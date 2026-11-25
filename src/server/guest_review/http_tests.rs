use super::*;

fn context() -> SessionContext {
    SessionContext {
        origin: "https://keeppeek.example".into(),
        policy_revision: 1,
    }
}

fn request(method: &str, path: &str, mut headers: Vec<(&str, &str)>) -> Request {
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("Host"))
    {
        headers.push(("Host", "keeppeek.example"));
    }
    Request::fake_https(
        method,
        path,
        headers
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
        Vec::new(),
    )
}

fn hardened(response: &Response) {
    for (name, expected) in [
        ("Cache-Control", "private, no-store"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Cross-Origin-Resource-Policy", "same-origin"),
    ] {
        let values = response
            .headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(values, vec![expected], "wrong {name}");
    }
    assert!(response.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("Content-Security-Policy")
            && value.contains("frame-ancestors 'none'")
    }));
}

#[test]
fn guest_boundary_never_inherits_trusted_local_or_normal_api_authority() {
    let state = ServerState::empty();
    assert!(
        boundary(
            &request("GET", "/guest/session", vec![]),
            &state,
            &context()
        )
        .is_ok()
    );
    for headers in [
        vec![("Authorization", "Bearer ordinary-api-secret")],
        vec![("Origin", "https://attacker.example")],
        vec![("Sec-Fetch-Site", "cross-site")],
        vec![("Host", "attacker.example")],
        vec![("Host", "keeppeek.example"), ("Host", "keeppeek.example")],
    ] {
        assert!(
            boundary(
                &request("GET", "/guest/session", headers),
                &state,
                &context()
            )
            .is_err()
        );
    }
    let ordinary_cookie = request(
        "GET",
        "/guest/session",
        vec![("Cookie", "__Host-keeppeek-session=ordinary")],
    );
    assert!(handle_cookie(&ordinary_cookie).is_err());
    let plain = Request::fake_http(
        "GET",
        "/guest/session",
        vec![("Host".into(), "keeppeek.example".into())],
        Vec::new(),
    );
    assert!(boundary(&plain, &state, &context()).is_err());
}

#[test]
fn mutating_guest_routes_require_exact_origin_and_canonical_empty_body_boundary() {
    let state = ServerState::empty();
    for method in ["POST", "DELETE"] {
        assert!(
            boundary(
                &request(method, "/guest/session", vec![]),
                &state,
                &context()
            )
            .is_err()
        );
        assert!(
            boundary(
                &request(
                    method,
                    "/guest/session",
                    vec![("Origin", "https://keeppeek.example")]
                ),
                &state,
                &context()
            )
            .is_ok()
        );
    }
    for (path, headers) in [
        ("/guest/%73ession", vec![]),
        ("/guest/session", vec![("Content-Length", "1")]),
        ("/guest/session", vec![("Transfer-Encoding", "chunked")]),
        (
            "/guest/session",
            vec![
                ("Origin", "https://keeppeek.example"),
                ("Origin", "https://keeppeek.example"),
            ],
        ),
    ] {
        assert!(boundary(&request("GET", path, headers), &state, &context()).is_err());
    }
}

#[test]
fn session_query_is_exact_and_never_discovers_a_different_public_session() {
    let id = Uuid::new_v4();
    let valid = request("GET", &format!("/guest/media?session={id}"), vec![]);
    assert_eq!(session_id(&valid, true).unwrap(), Some(id));
    assert_eq!(
        session_id(&request("GET", "/guest/session", vec![]), false).unwrap(),
        None
    );
    for path in [
        "/guest/media".to_owned(),
        "/guest/media?token=secret".to_owned(),
        format!("/guest/media?session={id}&extra=1"),
        format!("/guest/media?session={id}&session={id}"),
        "/guest/media?session=00000000-0000-0000-0000-000000000000".to_owned(),
        format!("/guest/media?session={}", id.simple()),
    ] {
        assert!(session_id(&request("GET", &path, vec![]), true).is_err());
    }
}

#[test]
fn wrong_guest_methods_paths_and_failed_handoffs_keep_security_headers() {
    let state = ServerState::empty();
    for (method, path, status) in [
        ("PUT", "/guest/session", 405),
        ("GET", "/guest/unknown", 404),
        ("POST", "/share", 405),
        ("GET", "/share/nested", 404),
        ("GET", "/share?token=never-accepted", 400),
    ] {
        let response = handle(&request(method, path, vec![]), &state).unwrap();
        assert_eq!(response.status_code, status);
        hardened(&response);
    }
    let response = handle(&request("POST", "/guest/session", vec![]), &state).unwrap();
    assert!(response.status_code >= 400);
    let cookies = response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("Set-Cookie"))
        .collect::<Vec<_>>();
    assert_eq!(cookies.len(), 1);
    assert!(cookies[0].1.contains("Max-Age=0"));
    hardened(&response);
    assert!(handle(&request("GET", "/api/health", vec![]), &state).is_none());
}

#[test]
fn hardening_replaces_existing_normal_page_cache_and_csp_values() {
    let response = headers(
        Response::text("page")
            .with_additional_header("Cache-Control", "public, max-age=3600")
            .with_additional_header("cache-control", "public")
            .with_additional_header("Content-Security-Policy", "default-src *"),
    );
    hardened(&response);
    let csp = response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("Content-Security-Policy"))
        .collect::<Vec<_>>();
    assert_eq!(csp.len(), 1);
    assert!(csp[0].1.contains("default-src 'none'"));
}

#[test]
fn a_shared_document_allows_external_navigation_without_allowing_cross_site_guest_api() {
    let state = ServerState::empty();
    for site in ["cross-site", "same-site"] {
        let headers = vec![("Sec-Fetch-Site", site)];
        assert!(
            boundary(
                &request("GET", "/share", headers.clone()),
                &state,
                &context()
            )
            .is_ok()
        );
        assert!(
            boundary(
                &request("GET", "/guest/session", headers),
                &state,
                &context()
            )
            .is_err()
        );
    }
}

#[test]
fn canonical_long_public_origins_fit_the_guest_host_header_budget() {
    let state = ServerState::empty();
    let host = format!(
        "{}.{}.{}.{}:8443",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    assert!(host.len() > 256);
    let origin = format!("https://{host}");
    crate::config::sharing::SharingConfig {
        enabled: true,
        public_origin: Some(origin.clone()),
        revision: 1,
    }
    .validate()
    .unwrap();
    let context = SessionContext {
        origin,
        policy_revision: 1,
    };
    assert!(
        boundary(
            &request("GET", "/guest/session", vec![("Host", &host)]),
            &state,
            &context
        )
        .is_ok()
    );
}

#[test]
fn classified_peer_isolates_trusted_proxy_clients_and_blocks_forged_headers() {
    use crate::access::NetworkAccessPolicy;
    use std::net::IpAddr;

    let ip = |value: &str| value.parse::<IpAddr>().unwrap();
    let direct = ServerState::empty();
    let spoofed = request(
        "POST",
        "/guest/session",
        vec![("X-Forwarded-For", "198.51.100.42")],
    );
    assert_eq!(
        classify_peer(&spoofed, &direct).effective_address,
        ip("127.0.0.1")
    );

    let mut proxied = ServerState::empty();
    proxied.network_access = NetworkAccessPolicy::new(
        vec!["127.0.0.1/32".parse().unwrap()],
        vec!["127.0.0.1/32".parse().unwrap()],
    );
    for (forwarded, expected) in [
        (Some("192.0.2.2"), "192.0.2.2"),
        (Some("192.0.2.3"), "192.0.2.3"),
        (Some("10.0.0.99, 192.0.2.2"), "192.0.2.2"),
        (Some("::ffff:192.0.2.2"), "192.0.2.2"),
        (Some("not-an-ip"), "127.0.0.1"),
        (None, "127.0.0.1"),
    ] {
        let headers = forwarded.map_or_else(Vec::new, |v| vec![("X-Forwarded-For", v)]);
        assert_eq!(
            classify_peer(&request("POST", "/guest/session", headers), &proxied).effective_address,
            ip(expected),
            "{forwarded:?}"
        );
    }
}

#[test]
fn trusted_proxy_rate_limit_isolates_distinct_clients_while_blocking_spoofing() {
    use super::super::limits::{Action, Budget, Limited};
    use crate::access::NetworkAccessPolicy;
    use std::time::Instant;

    let now = Instant::now();
    let direct = ServerState::empty();
    let mut direct_budget = Budget::new(now);
    for octet in 1..=20_u8 {
        let header = format!("198.51.100.{octet}");
        let req = request("POST", "/guest/session", vec![("X-Forwarded-For", &header)]);
        direct_budget
            .admit(
                classify_peer(&req, &direct).effective_address,
                Action::Exchange,
                now,
            )
            .unwrap();
    }
    let extra = request(
        "POST",
        "/guest/session",
        vec![("X-Forwarded-For", "198.51.100.99")],
    );
    assert_eq!(
        direct_budget.admit(
            classify_peer(&extra, &direct).effective_address,
            Action::Exchange,
            now
        ),
        Err(Limited)
    );

    let mut proxied = ServerState::empty();
    proxied.network_access = NetworkAccessPolicy::new(
        vec!["127.0.0.1/32".parse().unwrap()],
        vec!["127.0.0.1/32".parse().unwrap()],
    );
    let mut proxied_budget = Budget::new(now);
    let first = request(
        "POST",
        "/guest/session",
        vec![("X-Forwarded-For", "192.0.2.2")],
    );
    for _ in 0..20 {
        proxied_budget
            .admit(
                classify_peer(&first, &proxied).effective_address,
                Action::Exchange,
                now,
            )
            .unwrap();
    }
    let spoofed_first = request(
        "POST",
        "/guest/session",
        vec![("X-Forwarded-For", "10.0.0.5, 192.0.2.2")],
    );
    assert_eq!(
        proxied_budget.admit(
            classify_peer(&spoofed_first, &proxied).effective_address,
            Action::Exchange,
            now
        ),
        Err(Limited)
    );
    let second = request(
        "POST",
        "/guest/session",
        vec![("X-Forwarded-For", "192.0.2.3")],
    );
    proxied_budget
        .admit(
            classify_peer(&second, &proxied).effective_address,
            Action::Exchange,
            now,
        )
        .unwrap();
}

#[test]
fn guest_cookie_is_rejected_by_ordinary_protected_http_routes() {
    let state = ServerState::empty();
    let (_router, router_tx) = crate::runtime::Router::new().unwrap();
    let cookie = format!("{COOKIE}={}", "A".repeat(43));
    for (index, (method, path)) in [
        ("GET", "/logs"),
        ("GET", "/logs/snapshot"),
        ("GET", "/metrics"),
        ("GET", "/config/export"),
        ("GET", "/recording-coverage?start_ms=0&end_ms=1000"),
        ("POST", "/create"),
    ]
    .into_iter()
    .enumerate()
    {
        let remote =
            std::net::SocketAddr::from(([203, 0, 113, 10 + u8::try_from(index).unwrap()], 4567));
        let req = Request::fake_https_from(
            remote,
            method,
            path,
            vec![
                ("Host".into(), "keeppeek.example".into()),
                ("Origin".into(), "https://keeppeek.example".into()),
                ("Cookie".into(), cookie.clone()),
            ],
            Vec::new(),
        );
        let response = super::super::super::handle_request(&req, &router_tx, &state);
        assert_eq!(
            response.status_code, 401,
            "guest cookie must not authorize {method} {path}"
        );
    }
}
