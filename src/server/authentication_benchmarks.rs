//! Opt-in release workload, not a browser or network-ingress benchmark.
//!
//! Run with `cargo test --release --lib issue123_authentication_benchmark -- --ignored --nocapture --test-threads=1`.
//! Compare bearer rows from baseline and feature builds on the same idle machine.
//! The same-build cookie/bearer delta does not measure the historical bearer regression.

use super::*;
use crate::access::{browser_sessions, external, oidc};
use serde_json::{Value, json};
use std::hint::black_box;

const ORIGIN: &str = "https://keeppeek.example";
const SESSION_COOKIE: &str = "__Host-keeppeek-session";
const LOGIN_COOKIE: &str = "__Host-keeppeek-login";
const LOGIN_RUNS: usize = 30;
const AUTH_RUNS: usize = 10_000;
const FLOOD_RUNS: usize = 10_000;
const WARMUP_RUNS: usize = 1_000;
const WATCHES_PER_OWNER: usize = 8;

struct Fixture {
    state: ServerState,
    provider: oidc::FixtureProvider,
    client: oidc::Provider,
    directory: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let provider = oidc::FixtureProvider::new();
        let directory =
            std::env::temp_dir().join(format!("keeppeek-auth-bench-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let settings: external::Config = toml::from_str(&format!(
            r#"allowed_origins = ['{ORIGIN}']
[[providers]]
id = 'company'
name = 'Benchmark'
mappings = [{{claim = 'groups', value = 'viewers', role = 'administrator'}}]
[providers.method]
kind = 'oidc'
issuer = '{}'
client_id = 'keeppeek'
redirect_uri = '{ORIGIN}/auth/callback'
private_networks = ['127.0.0.1/32']
"#,
            provider.config.issuer
        ))
        .unwrap();
        let path = directory.join("config.toml");
        let mut root = toml::Table::new();
        root.insert(
            "external_auth".into(),
            toml::Value::try_from(settings).unwrap(),
        );
        config::write_configuration_table(&path, &root).unwrap();
        let config = config::load_config(&path).unwrap();
        let mut state = ServerState::empty();
        state.camera_config_path = Some(path);
        state.allowed_origins = Arc::new(HashSet::from([ORIGIN.to_owned()]));
        state.api_session_policy.max_per_principal = 64;
        state.api_session_policy.max_per_address = 64;
        state.authentication = Arc::new(Mutex::new(authentication::Registry::new(
            &config,
            state.api_session_policy,
        )));
        let now = Instant::now();
        let started = Instant::now();
        let client = state
            .oidc_cache
            .lock()
            .unwrap()
            .get(1, "company", now, || {
                oidc::Provider::discover_with_budget(
                    &provider.config,
                    provider.transport.clone(),
                    &state.oidc_issuer_budgets,
                    now,
                )
            })
            .unwrap();
        emit(json!({"workload":"cold_discovery_jwks_https", "samples":1,
            "wall_ns":nanos(started.elapsed()), "network":"real localhost TLS"}));
        Self {
            state,
            provider,
            client,
            directory,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.webrtc.shutdown();
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

struct Browser {
    cookie: String,
    csrf: String,
    address: u8,
}

fn request(
    address: u8,
    method: &str,
    path: &str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Request {
    let mut all = vec![
        ("Host".into(), "keeppeek.example".into()),
        ("Origin".into(), ORIGIN.into()),
        ("Sec-Fetch-Site".into(), "same-origin".into()),
    ];
    all.extend(headers);
    Request::fake_https_from(
        (Ipv4Addr::new(203, 0, 113, address), 4567).into(),
        method,
        path,
        all,
        body,
    )
}

fn cookie_header(browser: &Browser) -> Vec<(String, String)> {
    vec![(
        "Cookie".into(),
        format!("{SESSION_COOKIE}={}", browser.cookie),
    )]
}

fn cookie(response: &Response, name: &str) -> String {
    response
        .headers
        .iter()
        .find_map(|(header, value)| {
            (header == "Set-Cookie")
                .then(|| value.split(';').next().unwrap().split_once('='))
                .flatten()
                .filter(|(key, _)| *key == name)
                .map(|(_, value)| value.to_owned())
        })
        .expect("expected cookie")
}

fn json_response(response: Response) -> Value {
    assert_eq!(response.status_code, 200);
    serde_json::from_reader(response.data.into_reader_and_size().0).unwrap()
}

fn start_login(fixture: &Fixture, address: u8) -> (String, Url) {
    let bootstrap = authentication::handle(
        &request(address, "GET", "/auth/session", vec![], vec![]),
        &fixture.state,
    )
    .unwrap();
    let pending = cookie(&bootstrap, LOGIN_COOKIE);
    let csrf = json_response(bootstrap)["csrf_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("provider_id", "company")
        .append_pair("csrf_token", &csrf)
        .finish();
    let response = authentication::handle(
        &request(
            address,
            "POST",
            "/auth/login",
            vec![
                ("Cookie".into(), format!("{LOGIN_COOKIE}={pending}")),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
            ],
            form.into_bytes(),
        ),
        &fixture.state,
    )
    .unwrap();
    assert_eq!(response.status_code, 303);
    let authorization = Url::parse(
        &response
            .headers
            .iter()
            .find(|(name, _)| name == "Location")
            .unwrap()
            .1,
    )
    .unwrap();
    (pending, authorization)
}

fn login(fixture: &Fixture, address: u8) -> Browser {
    let (pending, authorization) = start_login(fixture, address);
    fixture.provider.prepare_login(&authorization, now_ms());
    let state = authorization
        .query_pairs()
        .find(|(name, _)| name == "state")
        .unwrap()
        .1
        .into_owned();
    let response = authentication::handle(
        &request(
            address,
            "GET",
            &format!("/auth/callback?code=synthetic-code&state={state}"),
            vec![("Cookie".into(), format!("{LOGIN_COOKIE}={pending}"))],
            vec![],
        ),
        &fixture.state,
    )
    .unwrap();
    assert_eq!(response.status_code, 303);
    let mut browser = Browser {
        cookie: cookie(&response, SESSION_COOKIE),
        csrf: String::new(),
        address,
    };
    let response = authentication::handle(
        &request(
            address,
            "GET",
            "/auth/session",
            cookie_header(&browser),
            vec![],
        ),
        &fixture.state,
    )
    .unwrap();
    browser.csrf = json_response(response)["csrf_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        api_principal(&authorization_request(&browser), &fixture.state)
            .unwrap()
            .principal
            .role,
        AccessRole::Administrator
    );
    browser
}

fn authorization_request(browser: &Browser) -> Request {
    request(
        browser.address,
        "GET",
        "/api/session",
        cookie_header(browser),
        vec![],
    )
}

fn provider_exchange(fixture: &Fixture) {
    use crate::access::login_transactions::{Start, Transactions};
    let mut transactions = Transactions::default();
    let mut samples = Vec::with_capacity(LOGIN_RUNS);
    for _ in 0..LOGIN_RUNS {
        let browser = Uuid::new_v4();
        let url = transactions
            .start(
                Start {
                    browser,
                    origin: ORIGIN,
                    provider_id: "company",
                    revision: 1,
                    return_path: "/",
                    candidate_plan: None,
                },
                &fixture.provider.config,
                fixture.client.authorization_endpoint(),
                Instant::now(),
            )
            .unwrap();
        fixture.provider.prepare_login(&url, now_ms());
        let state = url
            .query_pairs()
            .find(|(name, _)| name == "state")
            .unwrap()
            .1
            .into_owned();
        let transaction = transactions
            .consume(&state, browser, ORIGIN, 1, Instant::now())
            .unwrap();
        let started = Instant::now();
        let verified = fixture.client.exchange(
            oidc::Exchange {
                code: "synthetic-code",
                nonce: &transaction.nonce,
                verifier: transaction.verifier,
            },
            now_ms,
        );
        samples.push(nanos(started.elapsed()));
        assert!(verified.is_ok());
    }
    distribution(
        "provider_exchange_real_tls_including_fixture_signing_and_token_validation",
        &mut samples,
    );
}

fn measure_authorization(state: &ServerState, request: &Request) -> u64 {
    let start = Instant::now();
    let result = api_principal(black_box(request), black_box(state));
    let elapsed = nanos(start.elapsed());
    assert!(result.is_ok());
    black_box(result.unwrap());
    elapsed
}

fn authorization_comparison(fixture: &Fixture, browser: &Browser) {
    let mut bearer = ServerState::empty();
    let key = AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
    bearer.access_manager = AccessManager::ephemeral(key);
    bearer.allowed_origins = Arc::new(HashSet::from([ORIGIN.to_owned()]));
    let bearer_request = request(
        browser.address,
        "GET",
        "/api/session",
        vec![(
            "Authorization".into(),
            format!("Bearer {}", key.canonical()),
        )],
        vec![],
    );
    let cookie_request = authorization_request(browser);
    for _ in 0..WARMUP_RUNS {
        measure_authorization(&bearer, &bearer_request);
        measure_authorization(&fixture.state, &cookie_request);
    }
    let mut bearer_samples = Vec::with_capacity(AUTH_RUNS);
    let mut cookie_samples = Vec::with_capacity(AUTH_RUNS);
    for index in 0..AUTH_RUNS {
        if index % 2 == 0 {
            bearer_samples.push(measure_authorization(&bearer, &bearer_request));
            cookie_samples.push(measure_authorization(&fixture.state, &cookie_request));
        } else {
            cookie_samples.push(measure_authorization(&fixture.state, &cookie_request));
            bearer_samples.push(measure_authorization(&bearer, &bearer_request));
        }
    }
    let baseline = distribution("authorization_bearer_same_build", &mut bearer_samples);
    let feature = distribution("authorization_validated_cookie", &mut cookie_samples);
    emit(
        json!({"workload":"same_build_authorization_comparison", "warmup_each":WARMUP_RUNS,
        "cookie_minus_bearer_p95_ns":i128::from(feature)-i128::from(baseline),
        "cookie_p95_extra_budget_ns":1_000_000, "historical_baseline_measured":false}),
    );
    assert!(
        feature <= baseline.saturating_add(1_000_000),
        "cookie p95 budget exceeded"
    );
}

fn browser_count(state: &ServerState, browser: &Browser) -> usize {
    let actor = api_principal(&authorization_request(browser), state).unwrap();
    let result = authentication::admin::dispatch(
        state,
        SessionId::from_u64(0),
        &actor.principal,
        actor.classification.reason,
        proto::ExternalAuthenticationCommand {
            action: Some(
                proto::external_authentication_command::Action::ListSessions(
                    proto::ListBrowserSessions {
                        page_size: Some(32),
                        page_token: String::new(),
                    },
                ),
            ),
        },
    )
    .unwrap();
    let control_ok::Result::ExternalAuthenticationResult(result) = result else {
        panic!("wrong result")
    };
    let Some(proto::external_authentication_result::Result::Sessions(result)) = result.result
    else {
        panic!("wrong sessions")
    };
    assert!(result.next_page_token.is_empty());
    result.sessions.len()
}

fn rejected_flood(fixture: &Fixture, browser: &Browser, resources: &mut Resources) {
    let before_sessions = browser_count(&fixture.state, browser);
    let before = resources.sample();
    let mut statuses = std::collections::BTreeMap::<u16, usize>::new();
    let start = Instant::now();
    for _ in 0..FLOOD_RUNS {
        let response = authentication::handle(
            &request(
                200,
                "POST",
                "/auth/login",
                vec![(
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                )],
                b"provider_id=company&csrf_token=invalid".to_vec(),
            ),
            &fixture.state,
        )
        .unwrap();
        assert!(matches!(response.status_code, 400 | 401 | 403 | 429));
        *statuses.entry(response.status_code).or_default() += 1;
    }
    let wall = start.elapsed();
    let after = resources.sample();
    assert_eq!(browser_count(&fixture.state, browser), before_sessions);
    emit(
        json!({"workload":"rejected_login_flood", "requests":FLOOD_RUNS,
        "wall_ns":nanos(wall), "process_cpu_ms":after.0.saturating_sub(before.0),
        "cpu_scope":"whole test process; OS cumulative counter; millisecond resolution",
        "rss_before_bytes":before.1, "rss_after_bytes":after.1, "statuses":statuses,
        "retained_authenticated_sessions":before_sessions, "synthetic_clock":false}),
    );
}

fn create_body() -> Vec<u8> {
    let body = serde_json::to_vec(&CreateRequest {
        offer: crate::api::SdpOffer {
            sdp_type: "offer".into(),
            sdp: crate::webrtc::test_api_offer().to_sdp_string(),
        },
    })
    .unwrap();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&body).unwrap();
    encoder.finish().unwrap()
}

fn create_session(state: &ServerState, browser: &Browser, body: &[u8]) -> Response {
    let mut headers = cookie_header(browser);
    headers.extend([
        ("X-KeepPeek-CSRF".into(), browser.csrf.clone()),
        ("Content-Type".into(), "application/json".into()),
        ("Content-Encoding".into(), "gzip".into()),
    ]);
    let request = request(browser.address, "POST", "/create", headers, body.to_vec());
    authenticated_api_request(&request, state, true, AccessRole::User, |identity| {
        create_api_session(&request, state, identity)
    })
}

fn created_id(response: Response) -> SessionId {
    assert_eq!(response.status_code, 201);
    let reader = GzDecoder::new(response.data.into_reader_and_size().0);
    let response: CreateResponse = serde_json::from_reader(reader).unwrap();
    assert_eq!(response.answer.sdp_type, "answer");
    SessionId::from_u64(response.session_id.parse().unwrap())
}

fn register_dependents(state: &ServerState, id: SessionId) {
    session_lifecycle::admit(state, id, || {
        for index in 0..WATCHES_PER_OWNER {
            state
                .state_store_watches
                .register(id, "bench".into(), String::new(), format!("watch-{index}"))
                .expect("register dependent watch");
        }
        Ok(())
    })
    .unwrap();
}

fn revocation(fixture: &Fixture, browsers: &[Browser]) {
    let owners: Vec<_> = browsers
        .iter()
        .map(|browser| {
            let id = created_id(create_session(&fixture.state, browser, &create_body()));
            register_dependents(&fixture.state, id);
            id
        })
        .collect();
    let mut samples = Vec::with_capacity(browsers.len());
    for (browser, id) in browsers.iter().zip(owners) {
        let mut headers = cookie_header(browser);
        headers.push(("X-KeepPeek-CSRF".into(), browser.csrf.clone()));
        let request = request(browser.address, "POST", "/auth/logout", headers, vec![]);
        let start = Instant::now();
        let response = authentication::handle(&request, &fixture.state).unwrap();
        assert_eq!(response.status_code, 204);
        assert!(
            !fixture
                .state
                .api_session_owners
                .lock()
                .unwrap()
                .contains_key(&id)
        );
        assert!(!fixture.state.webrtc.active_api_session_ids().contains(&id));
        for index in 0..WATCHES_PER_OWNER {
            assert!(
                !fixture
                    .state
                    .state_store_watches
                    .owns_watch(id, &format!("watch-{index}"))
            );
        }
        samples.push(nanos(start.elapsed()));
        assert!(api_principal(&authorization_request(browser), &fixture.state).is_err());
    }
    distribution("logout_to_owner_and_watch_cleanup", &mut samples);
    emit(
        json!({"workload":"revocation_scope", "rtc_owners":LOGIN_RUNS,
        "watches_per_owner":WATCHES_PER_OWNER, "transport_shutdown_measured":false,
        "owner_registration":"real create handler, SDP acceptance, UDP session workers, and watches; no connected client",
        "max_budget_ns":1_000_000_000}),
    );
    assert!(
        *samples.last().unwrap() <= 1_000_000_000,
        "revocation budget exceeded"
    );
}

fn concurrent_sessions(resources: &mut Resources) {
    let mut fixture = Fixture::new();
    for (label, principal_limit, address_limit) in [("principal", 8, 64), ("address", 64, 8)] {
        fixture.state.api_session_policy.max_per_principal = principal_limit;
        fixture.state.api_session_policy.max_per_address = address_limit;
        let browser = login(&fixture, 100);
        let body = create_body();
        let barrier = std::sync::Barrier::new(16);
        let done = AtomicBool::new(false);
        let started = Instant::now();
        let (outcomes, peak) = std::thread::scope(|scope| {
            let monitor = scope.spawn(|| sample_session_peaks(&fixture.state, &done));
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        let response = create_session(&fixture.state, &browser, &body);
                        match response.status_code {
                            201 => Some(created_id(response)),
                            429 => None,
                            status => panic!("unexpected concurrent creation status {status}"),
                        }
                    })
                })
                .collect();
            let outcomes = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>();
            done.store(true, Ordering::Release);
            (outcomes, monitor.join().unwrap())
        });
        let admission_ns = nanos(started.elapsed());
        let admitted: Vec<_> = outcomes.into_iter().flatten().collect();
        assert_eq!(admitted.len(), 8, "configured admission limit");
        assert_eq!(fixture.state.api_session_owners.lock().unwrap().len(), 8);
        assert_eq!(fixture.state.webrtc.active_api_session_ids().len(), 8);
        for id in &admitted {
            register_dependents(&fixture.state, *id);
        }
        let rss = resources.sample().1;
        let close_ns = revoke_concurrent(&fixture.state, &browser, &admitted);
        emit(
            json!({"workload":"concurrent_real_session_admission", "limit_kind":label,
            "barrier_workers":16, "principal_limit":principal_limit, "address_limit":address_limit,
            "accepted":admitted.len(), "rejected":8, "retained_owner_high_water":8,
            "retained_transport_high_water":8, "admission_wall_ns":admission_ns,
            "sampled_inflight_owner_peak":peak.0, "sampled_inflight_transport_peak":peak.1,
            "peak_sampling":"separate registry snapshots approximately every 1 ms; may miss shorter peaks",
            "rss_at_capacity_bytes":rss, "logout_cleanup_ns":close_ns,
            "scope":"real create handler and server WebRTC workers; synthetic HTTPS ingress and SDP peer",
            "connected_remote_clients":0}),
        );
        assert!(close_ns <= 1_000_000_000, "concurrent cleanup budget");
    }
}

fn sample_session_peaks(state: &ServerState, done: &AtomicBool) -> (usize, usize) {
    let mut peak = (0, 0);
    for _ in 0..5_000 {
        peak.0 = peak.0.max(state.api_session_owners.lock().unwrap().len());
        peak.1 = peak.1.max(state.webrtc.active_api_session_ids().len());
        if done.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    peak
}

fn revoke_concurrent(state: &ServerState, browser: &Browser, sessions: &[SessionId]) -> u64 {
    let mut headers = cookie_header(browser);
    headers.push(("X-KeepPeek-CSRF".into(), browser.csrf.clone()));
    let request = request(browser.address, "POST", "/auth/logout", headers, vec![]);
    let started = Instant::now();
    assert_eq!(
        authentication::handle(&request, state).unwrap().status_code,
        204
    );
    assert!(state.api_session_owners.lock().unwrap().is_empty());
    assert!(state.webrtc.active_api_session_ids().is_empty());
    for id in sessions {
        for index in 0..WATCHES_PER_OWNER {
            assert!(
                !state
                    .state_store_watches
                    .owns_watch(*id, &format!("watch-{index}"))
            );
        }
    }
    nanos(started.elapsed())
}

fn session_capacity(resources: &mut Resources) {
    let mut sessions = browser_sessions::Sessions::new(browser_sessions::Limits {
        idle: Duration::from_secs(600),
        absolute: Duration::from_secs(3600),
        per_identity: 4096,
        per_address: 4096,
    });
    let before = resources.sample().1;
    for index in 0..4096_u128 {
        sessions
            .issue(
                Some(browser_sessions::Binding {
                    identity_id: Uuid::from_u128(index + 1),
                    revision: 1,
                }),
                ORIGIN,
                Ipv4Addr::new(203, 0, 113, 250).into(),
                Instant::now(),
                now_ms(),
            )
            .unwrap();
    }
    assert!(
        sessions
            .issue(
                None,
                ORIGIN,
                Ipv4Addr::new(203, 0, 113, 251).into(),
                Instant::now(),
                now_ms()
            )
            .is_err()
    );
    assert_eq!(sessions.list().count(), 4096);
    emit(
        json!({"workload":"browser_registry_capacity_component", "retained_high_water":4096,
        "overflow_rejected":true, "rss_before_bytes":before, "rss_after_bytes":resources.sample().1,
        "scope":"direct session registry; not 4096 network connections", "synthetic_clock":false}),
    );
}

fn cache_capacity(resources: &mut Resources) {
    let budgets = oidc::IssuerBudgets::default();
    let mut active = oidc::Cache::default();
    let mut candidate = oidc::CandidateCache::default();
    let mut providers = Vec::new();
    let mut fixtures = Vec::new();
    let before = resources.sample().1;
    for index in 0..8 {
        let fixture = oidc::FixtureProvider::new();
        let now = Instant::now();
        let discover = || {
            oidc::Provider::discover_with_budget(
                &fixture.config,
                fixture.transport.clone(),
                &budgets,
                now,
            )
        };
        providers.push(if index < 4 {
            active
                .get(1, &format!("provider-{index}"), now, discover)
                .unwrap()
        } else {
            candidate.get(&fixture.config, now, discover).unwrap()
        });
        resources.sample();
        fixtures.push(fixture);
    }
    assert_eq!(fixtures.len(), 8);
    assert!(
        active
            .get(1, "overflow", Instant::now(), || panic!(
                "cache overflow discovery"
            ))
            .is_err()
    );
    let ninth = oidc::FixtureProvider::new();
    assert!(
        candidate
            .get(&ninth.config, Instant::now(), || panic!(
                "cache overflow discovery"
            ))
            .is_err()
    );
    assert!(
        oidc::Provider::discover_with_budget(
            &ninth.config,
            ninth.transport.clone(),
            &budgets,
            Instant::now(),
        )
        .is_err()
    );
    emit(
        json!({"workload":"shared_oidc_cache_capacity_component", "active_retained":4,
        "candidate_retained":4, "issuer_leases":providers.len(), "overflow_rejected":true,
        "rss_before_bytes":before, "rss_after_bytes":resources.sample().1,
        "network":"real TLS discoveries; direct cache entry points; no synthetic time"}),
    );
    drop(fixtures);
}

struct Resources {
    system: sysinfo::System,
    rss_high_water: u64,
}

impl Resources {
    fn sample(&mut self) -> (u64, u64) {
        // ponytail: Phase samples bound observer cost; use a profiler for transient allocator peaks.
        let pid = sysinfo::Pid::from_u32(std::process::id());
        self.system
            .refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        let process = self.system.process(pid).expect("benchmark process metrics");
        let sample = (process.accumulated_cpu_time(), process.memory());
        self.rss_high_water = self.rss_high_water.max(sample.1);
        sample
    }
}

fn executable_digest() -> String {
    let mut file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
    let size = file.metadata().unwrap().len();
    assert!(
        size <= 2 * 1024 * 1024 * 1024,
        "benchmark executable exceeds hashing bound"
    );
    let mut buffer = [0; 65_536];
    let mut hash = Sha256::new();
    let mut read = 0_u64;
    for _ in 0..=size.div_ceil(65_536) {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        read += u64::try_from(count).unwrap();
        hash.update(&buffer[..count]);
    }
    assert_eq!(read, size, "benchmark executable changed while hashing");
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn metadata(resources: &mut Resources) {
    resources.system.refresh_cpu_all();
    emit(
        json!({"workload":"metadata", "schema":1, "profile":"release",
        "version":env!("CARGO_PKG_VERSION"), "os":sysinfo::System::long_os_version(),
        "arch":std::env::consts::ARCH, "logical_cpus":resources.system.cpus().len(),
        "cpu":resources.system.cpus().first().map(sysinfo::Cpu::brand),
        "build_label":std::env::var("KEEPPEEK_BENCH_BUILD").ok(),
        "executable_sha256":executable_digest(), "time_unix_ms":unix_time_ms(),
        "ingress":"rouille fake_https handler requests; no browser or ingress TLS",
        "provider":"real local HTTPS; fixture signs synthetic Ed25519 claims",
        "clock":"real wall/monotonic clocks throughout; no rate-window resets",
        "percentile":"nearest rank", "performance_gate":"docs/external-authentication.md"}),
    );
}

fn now_ms() -> i64 {
    i64::try_from(unix_time_ms()).unwrap()
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap()
}

fn emit(value: Value) {
    println!("ISSUE123_BENCH {value}");
}

fn percentile(samples: &[u64], percent: usize) -> u64 {
    assert!(!samples.is_empty() && (1..=100).contains(&percent));
    samples[(samples.len() * percent).div_ceil(100) - 1]
}

fn distribution(name: &str, samples: &mut [u64]) -> u64 {
    samples.sort_unstable();
    let p95 = percentile(samples, 95);
    emit(
        json!({"workload":name, "samples":samples.len(), "unit":"ns",
        "p50":percentile(samples, 50), "p95":p95, "max":samples.last().unwrap()}),
    );
    p95
}

#[test]
fn authentication_benchmark_percentiles_use_nearest_rank() {
    let samples: Vec<_> = (1..=30).collect();
    assert_eq!(percentile(&samples, 50), 15);
    assert_eq!(percentile(&samples, 95), 29);
    assert_eq!(percentile(&samples, 100), 30);
}

#[test]
#[ignore = "opt-in release performance workload; run alone with --nocapture --test-threads=1"]
fn issue123_authentication_benchmark() {
    assert!(
        !black_box(cfg!(debug_assertions)),
        "run this workload with --release"
    );
    let mut resources = Resources {
        system: sysinfo::System::new(),
        rss_high_water: 0,
    };
    metadata(&mut resources);
    let fixture = Fixture::new();
    let mut browsers = Vec::new();
    let mut samples = Vec::new();
    for index in 0..LOGIN_RUNS {
        let start = Instant::now();
        browsers.push(login(&fixture, u8::try_from(index + 1).unwrap()));
        samples.push(nanos(start.elapsed()));
        resources.sample();
    }
    distribution(
        "oidc_login_warm_metadata_real_token_tls_and_persistence",
        &mut samples,
    );
    provider_exchange(&fixture);
    assert_eq!(browser_count(&fixture.state, &browsers[0]), LOGIN_RUNS);
    emit(
        json!({"workload":"login_retention", "authenticated_session_high_water":LOGIN_RUNS,
        "per_identity_limit":64, "per_address_limit":64, "addresses":LOGIN_RUNS,
        "cold_discovery_excluded_from_login_samples":true}),
    );
    authorization_comparison(&fixture, &browsers[0]);
    rejected_flood(&fixture, &browsers[0], &mut resources);
    revocation(&fixture, &browsers);
    concurrent_sessions(&mut resources);
    session_capacity(&mut resources);
    cache_capacity(&mut resources);
    emit(
        json!({"workload":"process_memory", "sampled_rss_high_water_bytes":resources.rss_high_water,
        "scope":"whole process; sampled at phase boundaries and after each login/cache insert; not allocator peak"}),
    );
}
