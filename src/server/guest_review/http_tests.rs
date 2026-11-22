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
