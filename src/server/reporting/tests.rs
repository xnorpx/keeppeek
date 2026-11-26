use super::*;
use crate::{
    access::{AccessKey, AccessManager, AccessRole},
    logging::{LogHub, LogLevel},
    runtime::Router,
    server::handle_request,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Barrier, Mutex},
};

type RecordedCall = (String, Option<String>, Vec<u8>);
type StubHandler = Box<
    dyn Fn(&str, Option<&str>, &[u8]) -> Result<UpstreamResponse, TransportError> + Send + Sync,
>;

struct StubTransport {
    calls: Mutex<Vec<RecordedCall>>,
    handler: StubHandler,
}

impl StubTransport {
    fn new(
        handler: impl Fn(&str, Option<&str>, &[u8]) -> Result<UpstreamResponse, TransportError>
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            handler: Box::new(handler),
        })
    }
}

impl GrantTransport for StubTransport {
    fn create_grant(
        &self,
        origin: &str,
        bearer: Option<&str>,
        body: &[u8],
    ) -> Result<UpstreamResponse, TransportError> {
        self.calls.lock().unwrap().push((
            origin.to_owned(),
            bearer.map(str::to_owned),
            body.to_vec(),
        ));
        (self.handler)(origin, bearer, body)
    }
}

fn response_text(response: Response) -> String {
    let mut text = String::new();
    let (mut reader, _) = response.data.into_reader_and_size();
    reader.read_to_string(&mut text).unwrap();
    text
}

fn response_json(response: Response) -> Value {
    serde_json::from_str(&response_text(response)).unwrap()
}

fn has_no_store(response: &Response) -> bool {
    response.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("Cache-Control") && value.eq_ignore_ascii_case("no-store")
    })
}

fn valid_grant_body(title: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "idempotencyKey": "report-key-001",
        "manifest": {
            "version": 1,
            "title": title,
            "description": "canary-issue-desc playback stalled",
            "reproduction": "Open clip and press play.",
            "expected": "Video advances.",
            "actual": "Video freezes.",
            "evidenceWarnings": ["server-log-filter-active"],
            "artifacts": {
                "diagnostics": { "type": "application/gzip", "bytes": 4096 },
                "screen": { "type": "video/webm", "bytes": 65536 }
            }
        }
    }))
    .unwrap()
}

fn configured_state(
    transport: Arc<dyn GrantTransport>,
    credential: Option<&str>,
) -> (ServerState, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("keeppeek-report-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, "host = \"127.0.0.1\"\n").unwrap();
    std::fs::write(
        dir.join("secrets.toml"),
        "REPORT_KEY = \"canary-service-secret-value\"\n",
    )
    .unwrap();
    let mut state = ServerState::empty().with_camera_config_path(config_path);
    state.reporting = Runtime::with_transport(
        Some(Settings {
            origin: "https://reports.example".to_owned(),
            credential: credential.map(str::to_owned),
            retention_days: 14,
        }),
        transport,
    );
    (state, dir)
}

fn local_request(method: &str, path: &str, body: Vec<u8>) -> Request {
    let mut headers = vec![("Host".to_owned(), "127.0.0.1:8081".to_owned())];
    if !body.is_empty() {
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
    }
    Request::fake_http(method, path, headers, body)
}

fn remote_request(
    secure: bool,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Vec<u8>,
) -> Request {
    let mut headers = vec![("Host".to_owned(), "keeppeek.example".to_owned())];
    if let Some(token) = bearer {
        headers.push(("Authorization".to_owned(), format!("Bearer {token}")));
    }
    if !body.is_empty() {
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
    }
    let addr = "203.0.113.10:44321".parse().unwrap();
    if secure {
        Request::fake_https_from(addr, method, path, headers, body)
    } else {
        Request::fake_http_from(addr, method, path, headers, body)
    }
}

#[test]
fn unconfigured_reporting_keeps_local_default_and_rejects_grant_requests() {
    let (_router, router_tx) = Router::new().unwrap();
    let state = ServerState::empty();

    let status = handle_request(
        &local_request("GET", "/reporting/status", vec![]),
        &router_tx,
        &state,
    );
    assert_eq!(status.status_code, 200);
    assert!(has_no_store(&status));
    assert_eq!(response_json(status), json!({ "configured": false }));

    let grant = handle_request(
        &local_request("POST", "/reporting/grants", valid_grant_body("Local only")),
        &router_tx,
        &state,
    );
    assert_eq!(grant.status_code, 503);
    assert!(has_no_store(&grant));

    let audit = state.access_manager.list_audit(10);
    assert!(
        audit
            .iter()
            .any(|event| event.action == "bug_report_submit" && event.result == "unconfigured")
    );
}

#[test]
fn enforces_administrator_authorization_and_secure_remote_classification() {
    let (_router, router_tx) = Router::new().unwrap();
    let transport = StubTransport::new(|_, _, _| {
        Ok(UpstreamResponse {
            status: 200,
            body: br#"{"id":"rep-1","grant":"tok-1","expires":1900000000000}"#.to_vec(),
        })
    });
    let (mut state, dir) = configured_state(transport, None);
    let manager = AccessManager::ephemeral(AccessKey::unset());
    let admin = manager
        .create_credential("admin", None, AccessRole::Administrator, None, 1_000)
        .unwrap();
    let user = manager
        .create_credential("viewer", None, AccessRole::User, None, 1_000)
        .unwrap();
    state = state.with_access_manager(manager);
    state.require_secure_remote = true;

    let admin_key = admin.access_key.canonical();
    let user_key = user.access_key.canonical();
    let audit_body = br#"{"event":"start","mode":"logs_only"}"#.to_vec();

    for (method, path, body) in [
        ("GET", "/reporting/status", vec![]),
        ("POST", "/reporting/grants", valid_grant_body("Auth test")),
        ("POST", "/reporting/audit", audit_body),
    ] {
        let plain = remote_request(false, method, path, Some(&admin_key), body.clone());
        assert_eq!(handle_request(&plain, &router_tx, &state).status_code, 426);

        let anon = remote_request(true, method, path, None, body.clone());
        assert_eq!(handle_request(&anon, &router_tx, &state).status_code, 401);

        let viewer = remote_request(true, method, path, Some(&user_key), body.clone());
        assert_eq!(handle_request(&viewer, &router_tx, &state).status_code, 403);

        let allowed = remote_request(true, method, path, Some(&admin_key), body);
        let response = handle_request(&allowed, &router_tx, &state);
        assert!(matches!(response.status_code, 200 | 204));
    }

    let bad_origin = Request::fake_https_from(
        "203.0.113.10:44321".parse().unwrap(),
        "POST",
        "/reporting/grants",
        vec![
            ("Host".into(), "keeppeek.example".into()),
            ("Origin".into(), "https://evil.example".into()),
            ("Authorization".into(), format!("Bearer {admin_key}")),
            ("Content-Type".into(), "application/json".into()),
        ],
        valid_grant_body("Bad origin"),
    );
    assert_eq!(
        handle_request(&bad_origin, &router_tx, &state).status_code,
        403
    );

    std::fs::remove_dir_all(dir).unwrap();
}

fn assert_audit_and_logs_free_of_canaries(state: &ServerState, hub: &LogHub) {
    let entry = hub.record(
        LogLevel::Info,
        "keeppeek::test",
        "leaked canary-upload-grant-token and canary-service-secret-value".to_owned(),
        BTreeMap::from([("grant".to_owned(), Value::String("direct-grant".to_owned()))]),
        None,
        None,
    );
    assert!(!entry.message.contains("canary-upload-grant-token"));
    assert!(!entry.message.contains("canary-service-secret-value"));
    assert_eq!(entry.fields["grant"], "[REDACTED]");

    let audit_serialized = serde_json::to_string(&state.access_manager.list_audit(20)).unwrap();
    assert!(audit_serialized.contains("bug_report_submit"));
    assert!(audit_serialized.contains("report-opaque-1"));
    for canary in [
        "canary-service-secret-value",
        "canary-upload-grant-token",
        "canary-issue-title",
        "canary-issue-desc",
    ] {
        assert!(!audit_serialized.contains(canary));
    }
}

#[test]
fn resolves_server_secret_redacts_grant_and_keeps_audit_free_of_canaries() {
    let (_router, router_tx) = Router::new().unwrap();
    let transport = StubTransport::new(|origin, bearer, body| {
        assert_eq!(origin, "https://reports.example");
        assert_eq!(bearer, Some("canary-service-secret-value"));
        let sent: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(sent["retentionDays"], 14);
        assert_eq!(sent["idempotencyKey"], "report-key-001");
        Ok(UpstreamResponse {
            status: 200,
            body: br#"{"id":"report-opaque-1","grant":"canary-upload-grant-token","expires":1900000000000}"#
                .to_vec(),
        })
    });
    let (mut state, dir) = configured_state(transport.clone(), Some("{secret:REPORT_KEY}"));
    let filter_file = crate::logging::LogFilterFile::beside_config(&dir.join("config.toml"));
    let (logging, _dispatch) = crate::logging::LoggingService::for_test(filter_file, "info");
    let hub = logging.dashboard_redactor();
    state.logging = Some(logging);

    let status_res = handle_request(
        &local_request("GET", "/reporting/status", vec![]),
        &router_tx,
        &state,
    );
    assert_eq!(status_res.status_code, 200);
    let status_text = response_text(status_res);
    assert!(!status_text.contains("canary-service-secret-value"));
    assert!(!status_text.contains("REPORT_KEY"));
    assert_eq!(
        serde_json::from_str::<Value>(&status_text).unwrap(),
        json!({
            "configured": true,
            "origin": "https://reports.example",
            "retention_days": 14
        })
    );

    let grant_res = handle_request(
        &local_request(
            "POST",
            "/reporting/grants",
            valid_grant_body("canary-issue-title"),
        ),
        &router_tx,
        &state,
    );
    assert_eq!(grant_res.status_code, 200);
    assert!(has_no_store(&grant_res));
    let grant_json = response_json(grant_res);
    assert_eq!(grant_json["id"], "report-opaque-1");
    assert_eq!(grant_json["grant"], "canary-upload-grant-token");
    assert_eq!(transport.calls.lock().unwrap().len(), 1);
    assert_audit_and_logs_free_of_canaries(&state, &hub);

    std::fs::remove_dir_all(dir).unwrap();
}

fn invalid_manifest_cases() -> Vec<Value> {
    vec![
        json!({ "idempotencyKey": "../bad", "manifest": {} }),
        json!({
            "idempotencyKey": "k1",
            "manifest": {
                "version": 1, "title": "   ", "description": "d", "reproduction": "r",
                "expected": "e", "actual": "a", "evidenceWarnings": [],
                "artifacts": { "diagnostics": { "type": "application/gzip", "bytes": 10 } }
            }
        }),
        json!({
            "idempotencyKey": "k1",
            "manifest": {
                "version": 1, "title": "t", "description": "d", "reproduction": "r",
                "expected": "e", "actual": "a", "evidenceWarnings": ["unknown-warning"],
                "artifacts": { "diagnostics": { "type": "application/gzip", "bytes": 10 } }
            }
        }),
        json!({
            "idempotencyKey": "k1",
            "manifest": {
                "version": 1, "title": "t", "description": "d", "reproduction": "r",
                "expected": "e", "actual": "a", "evidenceWarnings": [],
                "artifacts": {
                    "diagnostics": { "type": "application/gzip", "bytes": MAX_DIAGNOSTICS_BYTES + 1 }
                }
            }
        }),
        json!({
            "idempotencyKey": "k1",
            "manifest": {
                "version": 1, "title": "t", "description": "d", "reproduction": "r",
                "expected": "e", "actual": "a", "evidenceWarnings": [],
                "artifacts": {
                    "diagnostics": { "type": "application/gzip", "bytes": 10 },
                    "screen": { "type": "video/avi", "bytes": 10 }
                }
            }
        }),
    ]
}

#[test]
fn validates_manifest_bounds_and_request_limits() {
    let (_router, router_tx) = Router::new().unwrap();
    let transport = StubTransport::new(|_, _, _| {
        panic!("invalid manifests must be rejected before contacting the service")
    });
    let (state, dir) = configured_state(transport, None);

    let wrong_type = Request::fake_http(
        "POST",
        "/reporting/grants",
        vec![
            ("Host".into(), "127.0.0.1:8081".into()),
            ("Content-Type".into(), "text/plain".into()),
        ],
        valid_grant_body("Wrong mime"),
    );
    assert_eq!(
        handle_request(&wrong_type, &router_tx, &state).status_code,
        415
    );

    let oversized_body = local_request(
        "POST",
        "/reporting/grants",
        vec![b' '; MAX_GRANT_REQUEST_BYTES + 1],
    );
    assert_eq!(
        handle_request(&oversized_body, &router_tx, &state).status_code,
        413
    );

    for invalid_json in invalid_manifest_cases() {
        let res = handle_request(
            &local_request(
                "POST",
                "/reporting/grants",
                serde_json::to_vec(&invalid_json).unwrap(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(res.status_code, 400);
    }

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn maps_upstream_failures_without_echoing_bodies() {
    let (_router, router_tx) = Router::new().unwrap();
    let next = Arc::new(Mutex::new(Ok(UpstreamResponse {
        status: 500,
        body: b"upstream-private-detail-canary".to_vec(),
    })));
    let shared = Arc::clone(&next);
    let transport = StubTransport::new(move |_, _, _| match &*shared.lock().unwrap() {
        Ok(response) => Ok(UpstreamResponse {
            status: response.status,
            body: response.body.clone(),
        }),
        Err(TransportError::Timeout) => Err(TransportError::Timeout),
        Err(TransportError::Unavailable) => Err(TransportError::Unavailable),
        Err(TransportError::InvalidResponse) => Err(TransportError::InvalidResponse),
    });
    let (state, dir) = configured_state(transport, None);

    for (upstream, expected_status) in [
        (Ok((409, b"conflict-canary".as_slice())), 409),
        (Ok((400, b"bad-manifest-canary".as_slice())), 400),
        (Ok((401, b"bad-auth-canary".as_slice())), 502),
        (Ok((500, b"internal-canary".as_slice())), 502),
        (
            Ok((
                200,
                b"{\"id\":\"../bad\",\"grant\":\"ok\",\"expires\":1}".as_slice(),
            )),
            502,
        ),
        (Err(TransportError::Timeout), 504),
        (Err(TransportError::InvalidResponse), 502),
    ] {
        *next.lock().unwrap() = upstream.map(|(status, body)| UpstreamResponse {
            status,
            body: body.to_vec(),
        });
        let res = handle_request(
            &local_request(
                "POST",
                "/reporting/grants",
                valid_grant_body("Failure mapping"),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(res.status_code, expected_status);
        assert!(!response_text(res).contains("canary"));
    }

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn records_closed_audit_events_and_rejects_arbitrary_fields() {
    let (_router, router_tx) = Router::new().unwrap();
    let state = ServerState::empty();

    for payload in [
        json!({ "event": "start", "mode": "logs_only" }),
        json!({ "event": "start", "mode": "screen" }),
        json!({ "event": "cancel" }),
        json!({ "event": "local_download" }),
        json!({ "event": "complete", "report_id": "report-abc_123" }),
        json!({ "event": "failed", "report_id": "report-abc_123" }),
    ] {
        let res = handle_request(
            &local_request(
                "POST",
                "/reporting/audit",
                serde_json::to_vec(&payload).unwrap(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(res.status_code, 204);
        assert!(has_no_store(&res));
    }

    for invalid in [
        json!({ "event": "start", "issue_text": "private" }),
        json!({ "event": "complete", "report_id": "../escape" }),
        json!({ "event": "unknown" }),
    ] {
        let res = handle_request(
            &local_request(
                "POST",
                "/reporting/audit",
                serde_json::to_vec(&invalid).unwrap(),
            ),
            &router_tx,
            &state,
        );
        assert_eq!(res.status_code, 400);
    }

    let oversized = local_request(
        "POST",
        "/reporting/audit",
        vec![b' '; MAX_AUDIT_REQUEST_BYTES + 1],
    );
    assert_eq!(
        handle_request(&oversized, &router_tx, &state).status_code,
        413
    );

    let actions = state
        .access_manager
        .list_audit(20)
        .into_iter()
        .map(|event| event.action)
        .collect::<Vec<_>>();
    for expected in [
        "bug_report_start",
        "bug_report_cancel",
        "bug_report_local_download",
        "bug_report_complete",
        "bug_report_failed",
    ] {
        assert!(actions.iter().any(|action| action == expected));
    }
}

#[test]
fn bounds_concurrent_in_flight_grant_requests() {
    let (_router, router_tx) = Router::new().unwrap();
    let barrier = Arc::new(Barrier::new(MAX_IN_FLIGHT_GRANTS + 1));
    let release = Arc::new(Barrier::new(MAX_IN_FLIGHT_GRANTS + 1));
    let enter = Arc::clone(&barrier);
    let exit = Arc::clone(&release);
    let transport = StubTransport::new(move |_, _, _| {
        enter.wait();
        exit.wait();
        Ok(UpstreamResponse {
            status: 200,
            body: br#"{"id":"rep-ok","grant":"tok-ok","expires":1900000000000}"#.to_vec(),
        })
    });
    let (state, dir) = configured_state(transport, None);
    let state = Arc::new(state);
    let mut workers = Vec::new();
    for _ in 0..MAX_IN_FLIGHT_GRANTS {
        let worker_state = Arc::clone(&state);
        let worker_tx = router_tx.clone();
        workers.push(std::thread::spawn(move || {
            handle_request(
                &local_request("POST", "/reporting/grants", valid_grant_body("Concurrent")),
                &worker_tx,
                &worker_state,
            )
            .status_code
        }));
    }
    barrier.wait();
    let rejected = handle_request(
        &local_request("POST", "/reporting/grants", valid_grant_body("Overflow")),
        &router_tx,
        &state,
    );
    assert_eq!(rejected.status_code, 429);
    release.wait();
    for worker in workers {
        assert_eq!(worker.join().unwrap(), 200);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn log_hub_snapshots_report_stable_unique_generation_across_restarts() {
    let first = LogHub::default();
    let second = LogHub::default();
    let snap_a = first.snapshot(None, 10);
    let snap_b = first.snapshot(None, 10);
    let snap_restarted = second.snapshot(None, 10);
    assert!(
        snap_a
            .generation
            .as_deref()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(snap_a.generation, snap_b.generation);
    assert_ne!(snap_a.generation, snap_restarted.generation);
}
