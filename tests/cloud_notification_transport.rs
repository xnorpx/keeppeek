use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use keeppeek::{
    config::cloud_notifications::Settings,
    notifications::{
        cloud::{CloudSignalRuntime, HttpsSignalTransport, SignalTransport},
        cloud_limits::{
            CloudFailureCode, CloudState, INTENT_TTL_MS, ROLLING_DAY_MS, ROLLING_HOUR_MS,
        },
        cloud_protocol::{
            CANONICAL_BODY_BYTES, SignalEnvelope, parse_and_verify_signal, public_key_from_seed,
            sign_signal,
        },
        cloud_state::SignalDeliveryOutcome,
    },
};

const TEST_SEED: [u8; 32] = [0x77; 32];
const TEST_NVR_ID: [u8; 16] = [
    0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
];
const TEST_NVR_ID_STR: &str = "aabbccdd-eeff-0011-2233-445566778899";

#[derive(Clone, Debug)]
struct CapturedHttpRequest {
    request_line: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

type MockHttpResponse = (u16, Vec<(String, String)>, String);

fn spawn_fake_http_server(
    responses: Vec<MockHttpResponse>,
) -> (
    String,
    Arc<Mutex<Vec<CapturedHttpRequest>>>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_worker = captured.clone();
    let handle = std::thread::spawn(move || {
        for (status, extra_headers, body_text) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let req = read_http_request(&mut stream);
            captured_worker.lock().unwrap().push(req);
            let mut reply = format!(
                "HTTP/1.1 {status} TEST\r\nContent-Length: {}\r\nConnection: close\r\n",
                body_text.len()
            );
            for (name, value) in extra_headers {
                reply.push_str(&format!("{name}: {value}\r\n"));
            }
            reply.push_str("\r\n");
            reply.push_str(&body_text);
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (format!("http://{addr}"), captured, handle)
}

fn read_http_request(stream: &mut std::net::TcpStream) -> CapturedHttpRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "unexpected EOF while reading HTTP headers");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8(buffer[..header_end].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap().to_owned();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "unexpected EOF while reading HTTP body");
        buffer.extend_from_slice(&chunk[..read]);
    }
    CapturedHttpRequest {
        request_line,
        headers,
        body: buffer[header_end..header_end + content_length].to_vec(),
    }
}

#[test]
fn http_transport_sends_exact_109_bytes_without_redirects_bearer_or_privacy_canaries() {
    let secret_canary = "CANARY-SECRET-SIGNING-KEY";
    let camera_canary = "CANARY-FRONT-PORCH-CAMERA";
    let event_canary = "CANARY-PERSON-EVENT-999";
    let provider_text_canary = "CANARY-RAW-PROVIDER-ERROR-TEXT";

    let (origin, captured, server) = spawn_fake_http_server(vec![
        (202, vec![], provider_text_canary.to_owned()),
        (
            302,
            vec![(
                "Location".to_owned(),
                "https://evil.example/redirect".to_owned(),
            )],
            provider_text_canary.to_owned(),
        ),
        (
            429,
            vec![("Retry-After".to_owned(), "45".to_owned())],
            provider_text_canary.to_owned(),
        ),
        (401, vec![], provider_text_canary.to_owned()),
        (409, vec![], provider_text_canary.to_owned()),
    ]);

    let transport = HttpsSignalTransport::with_timeout(Duration::from_secs(2), true);
    let body = sign_signal(
        SignalEnvelope {
            nvr_id: TEST_NVR_ID,
            cloud_event_id: [0x22; 16],
            issued_at_secs: 1_756_728_000,
            signing_key_version: 1,
        },
        &TEST_SEED,
    )
    .unwrap();

    assert_eq!(
        transport.send_signal(&origin, &body),
        SignalDeliveryOutcome::Accepted
    );
    // Redirect must NOT be followed and must classify as dependency failure.
    assert_eq!(
        transport.send_signal(&origin, &body),
        SignalDeliveryOutcome::DependencyFailure
    );
    assert_eq!(
        transport.send_signal(&origin, &body),
        SignalDeliveryOutcome::RateLimited {
            retry_after_ms: Some(45_000)
        }
    );
    assert_eq!(
        transport.send_signal(&origin, &body),
        SignalDeliveryOutcome::CredentialRejected
    );
    assert_eq!(
        transport.send_signal(&origin, &body),
        SignalDeliveryOutcome::IntegrityConflict
    );

    // Production transport rejects plain HTTP origins without touching the network.
    let strict_transport = HttpsSignalTransport::new();
    assert_eq!(
        strict_transport.send_signal(&origin, &body),
        SignalDeliveryOutcome::BadRequest
    );

    server.join().unwrap();
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 5);
    let pub_key = public_key_from_seed(&TEST_SEED).unwrap();
    for req in requests.iter() {
        assert_eq!(req.request_line, "POST /api/v1/nvr/events HTTP/1.1");
        assert_eq!(
            req.headers.get("content-type").map(String::as_str),
            Some("application/octet-stream")
        );
        assert_eq!(
            req.headers.get("content-length").map(String::as_str),
            Some("109")
        );
        assert!(!req.headers.contains_key("authorization"));
        assert_eq!(req.body.len(), CANONICAL_BODY_BYTES);
        parse_and_verify_signal(&req.body, &pub_key).unwrap();
        let headers_blob = format!("{:?}", req.headers);
        for canary in [secret_canary, camera_canary, event_canary] {
            assert!(!headers_blob.contains(canary));
            assert!(!String::from_utf8_lossy(&req.body).contains(canary));
        }
    }
}

struct ScriptedTransport {
    outcomes: Mutex<VecDeque<SignalDeliveryOutcome>>,
    bodies: Mutex<Vec<[u8; CANONICAL_BODY_BYTES]>>,
}

impl SignalTransport for ScriptedTransport {
    fn send_signal(
        &self,
        _service_origin: &str,
        body: &[u8; CANONICAL_BODY_BYTES],
    ) -> SignalDeliveryOutcome {
        self.bodies.lock().unwrap().push(*body);
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(SignalDeliveryOutcome::DependencyFailure)
    }
}

fn setup_scripted_runtime(
    name: &str,
    outcomes: Vec<SignalDeliveryOutcome>,
) -> (PathBuf, CloudSignalRuntime, Arc<ScriptedTransport>) {
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-cloud-matrix-{name}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.toml");
    std::fs::write(
        dir.join("secrets.toml"),
        format!("CLOUD_KEY = \"{}\"\n", STANDARD.encode(TEST_SEED)),
    )
    .unwrap();
    let settings = Settings {
        revision: 1,
        enabled: true,
        image_previews_enabled: false,
        service_origin: Some("https://service.keeppeek.example".to_owned()),
        nvr_id: Some(TEST_NVR_ID_STR.to_owned()),
        signing_key_version: Some(1),
        signing_key: Some("{secret:CLOUD_KEY}".to_owned()),
    };
    let transport = Arc::new(ScriptedTransport {
        outcomes: Mutex::new(VecDeque::from(outcomes)),
        bodies: Mutex::new(Vec::new()),
    });
    let cloud = CloudSignalRuntime::open_with_transport(
        config_path,
        &dir.join("cloud-notifications.db"),
        settings,
        transport.clone(),
        1_000_000,
    )
    .unwrap();
    (dir, cloud, transport)
}

#[test]
fn retry_matrix_reuses_exact_bytes_honors_retry_after_and_latches_credential_rejection() {
    let (dir, cloud, transport) = setup_scripted_runtime(
        "retry-matrix",
        vec![
            SignalDeliveryOutcome::RateLimited {
                retry_after_ms: Some(10_000),
            },
            SignalDeliveryOutcome::ServiceUnavailable {
                retry_after_ms: Some(5_000),
            },
            SignalDeliveryOutcome::Accepted,
            SignalDeliveryOutcome::IntegrityConflict,
            SignalDeliveryOutcome::CredentialRejected,
        ],
    );

    // Event 1: 429 (10s) -> 503 (5s) -> 202 Accepted on attempt 3 with exact same bytes.
    cloud
        .enqueue_logical_event("logical-1", "rule-1", "evt-1", "cam-1", 1, 1_000_000)
        .unwrap();
    assert!(cloud.step_once(1_000_000, 0.5).unwrap());
    assert_eq!(cloud.status(1_005_000).state, CloudState::Throttled);
    assert!(!cloud.step_once(1_009_999, 0.5).unwrap());

    // Newer pending event arrives during 429 wait; pending stays bounded at 1.
    cloud
        .enqueue_logical_event("logical-2", "rule-1", "evt-2", "cam-1", 1, 1_005_000)
        .unwrap();
    assert_eq!(cloud.store().queue_depth(), (1, 1));

    assert!(cloud.step_once(1_010_000, 0.5).unwrap());
    assert_eq!(cloud.status(1_012_000).state, CloudState::BackingOff);
    assert!(!cloud.step_once(1_014_999, 0.5).unwrap());
    assert!(cloud.step_once(1_015_000, 0.5).unwrap());

    let bodies = transport.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(bodies[1], bodies[2]);

    // Event 2 hits 409 IntegrityConflict -> terminal, no retry.
    assert!(cloud.step_once(1_050_000, 0.5).unwrap());
    assert_eq!(
        cloud.status(1_050_000).last_failure,
        CloudFailureCode::Integrity
    );
    assert_eq!(cloud.store().queue_depth(), (0, 0));

    // Event 3 hits 401/403 -> Unavailable until pairing is repaired.
    cloud
        .enqueue_logical_event("logical-3", "rule-1", "evt-3", "cam-1", 1, 1_100_000)
        .unwrap();
    assert!(cloud.step_once(1_100_000, 0.5).unwrap());
    assert_eq!(cloud.status(1_100_000).state, CloudState::Unavailable);
    assert_eq!(
        cloud.status(1_100_000).last_failure,
        CloudFailureCode::CredentialRejected
    );

    // Subsequent events cannot send until signing key version is repaired.
    cloud
        .enqueue_logical_event("logical-4", "rule-1", "evt-4", "cam-1", 1, 1_200_000)
        .unwrap();
    assert!(!cloud.step_once(1_200_000, 0.5).unwrap());

    let mut repaired = cloud.settings();
    repaired.signing_key_version = Some(2);
    cloud.apply_settings(repaired, 1_201_000).unwrap();
    assert_eq!(cloud.status(1_201_000).state, CloudState::Ready);

    std::fs::remove_dir_all(dir).unwrap();
}

/// Test-only simulator of authoritative Keeppeek Service ingress, fencing, and provider budgets.
struct SimulatedKeeppeekService {
    endpoint_authority: HashMap<[u8; 16], EndpointAuthority>,
    replays: HashMap<([u8; 16], [u8; 16]), ReplayRecord>,
    endpoint_hour: HashMap<String, BucketLedger>,
    endpoint_day: HashMap<String, BucketLedger>,
    account_hour: HashMap<String, BucketLedger>,
    account_day: HashMap<String, BucketLedger>,
    account_gen_hour: HashMap<String, BucketLedger>,
    account_gen_day: HashMap<String, BucketLedger>,
    account_cooldown_until_ms: HashMap<String, i64>,
    global_gcra_tat_ms: i64,
    active_apns_leases: usize,
    active_fcm_leases: usize,
    active_shared_leases: usize,
    clock_floor_ms: i64,
    next_op_id: u64,
}

#[derive(Clone)]
struct EndpointAuthority {
    stable_endpoint_id: String,
    account_id: String,
    binding_generation: u64,
    active_key_version: u32,
    public_key: [u8; 32],
    revoked: bool,
}

#[derive(Clone)]
struct ReplayRecord {
    body: [u8; CANONICAL_BODY_BYTES],
    op_id: u64,
    binding_generation: u64,
    accepted: bool,
}

#[derive(Default)]
struct BucketLedger {
    entries: VecDeque<(i64, u64)>,
    charged_ops: HashSet<u64>,
}

impl BucketLedger {
    fn consume(&mut self, now_ms: i64, window_ms: i64, limit: usize, op_id: u64) -> bool {
        let cutoff = now_ms.saturating_sub(window_ms);
        while self.entries.front().is_some_and(|&(ts, _)| ts <= cutoff) {
            if let Some((_, old_op)) = self.entries.pop_front() {
                self.charged_ops.remove(&old_op);
            }
        }
        if self.charged_ops.contains(&op_id) {
            return true;
        }
        if self.entries.len() >= limit {
            return false;
        }
        self.entries.push_back((now_ms, op_id));
        self.charged_ops.insert(op_id);
        true
    }
}

impl SimulatedKeeppeekService {
    fn new() -> Self {
        Self {
            endpoint_authority: HashMap::new(),
            replays: HashMap::new(),
            endpoint_hour: HashMap::new(),
            endpoint_day: HashMap::new(),
            account_hour: HashMap::new(),
            account_day: HashMap::new(),
            account_gen_hour: HashMap::new(),
            account_gen_day: HashMap::new(),
            account_cooldown_until_ms: HashMap::new(),
            global_gcra_tat_ms: 0,
            active_apns_leases: 0,
            active_fcm_leases: 0,
            active_shared_leases: 0,
            clock_floor_ms: 0,
            next_op_id: 1,
        }
    }

    fn register_endpoint(&mut self, nvr_id: [u8; 16], authority: EndpointAuthority) {
        self.endpoint_authority.insert(nvr_id, authority);
    }

    fn admit_signal(&mut self, body: &[u8], raw_now_ms: i64) -> u16 {
        let now_ms = raw_now_ms.max(self.clock_floor_ms);
        self.clock_floor_ms = now_ms;
        if body.len() != CANONICAL_BODY_BYTES {
            return 400;
        }
        let mut nvr_id = [0_u8; 16];
        nvr_id.copy_from_slice(&body[1..17]);
        let Some(authority) = self.endpoint_authority.get(&nvr_id).cloned() else {
            return 401;
        };
        if authority.revoked {
            return 403;
        }
        let Ok(verified) = parse_and_verify_signal(body, &authority.public_key) else {
            return 401;
        };
        if verified.signing_key_version != authority.active_key_version {
            return 401;
        }
        let canonical: [u8; CANONICAL_BODY_BYTES] = body.try_into().unwrap();
        let replay_key = (nvr_id, verified.cloud_event_id);
        let op_id = if let Some(existing) = self.replays.get(&replay_key) {
            if existing.body != canonical {
                return 409;
            }
            if existing.binding_generation != authority.binding_generation {
                return 403;
            }
            if existing.accepted {
                return 202;
            }
            existing.op_id
        } else {
            let op_id = self.next_op_id;
            self.next_op_id += 1;
            self.replays.insert(
                replay_key,
                ReplayRecord {
                    body: canonical,
                    op_id,
                    binding_generation: authority.binding_generation,
                    accepted: false,
                },
            );
            op_id
        };

        // Independent durable consumes in catalog order without refunding earlier buckets.
        if !self
            .endpoint_hour
            .entry(authority.stable_endpoint_id.clone())
            .or_default()
            .consume(now_ms, ROLLING_HOUR_MS, 120, op_id)
        {
            return 429;
        }
        if !self
            .endpoint_day
            .entry(authority.stable_endpoint_id.clone())
            .or_default()
            .consume(now_ms, ROLLING_DAY_MS, 500, op_id)
        {
            return 429;
        }
        if !self
            .account_hour
            .entry(authority.account_id.clone())
            .or_default()
            .consume(now_ms, ROLLING_HOUR_MS, 300, op_id)
        {
            return 429;
        }
        if !self
            .account_day
            .entry(authority.account_id.clone())
            .or_default()
            .consume(now_ms, ROLLING_DAY_MS, 1_000, op_id)
        {
            return 429;
        }
        // Revalidate authority generation before final acceptance.
        let current = self.endpoint_authority.get(&nvr_id);
        if current.is_none_or(|a| a.revoked || a.binding_generation != authority.binding_generation)
        {
            return 403;
        }
        if let Some(record) = self.replays.get_mut(&replay_key) {
            record.accepted = true;
        }
        // Account notification generation cooldown (3m) and 20/h, 120/d caps.
        let cooldown_ready = self
            .account_cooldown_until_ms
            .get(&authority.account_id)
            .is_none_or(|&until| now_ms >= until);
        if cooldown_ready
            && self
                .account_gen_hour
                .entry(authority.account_id.clone())
                .or_default()
                .consume(now_ms, ROLLING_HOUR_MS, 20, op_id)
            && self
                .account_gen_day
                .entry(authority.account_id.clone())
                .or_default()
                .consume(now_ms, ROLLING_DAY_MS, 120, op_id)
        {
            self.account_cooldown_until_ms
                .insert(authority.account_id, now_ms + 180_000);
        }
        202
    }

    fn acquire_provider_lease(&mut self, provider: &str, now_ms: i64) -> bool {
        // Durable global GCRA: 100 attempts/sec (10ms interval), burst 200 (tolerance 1990ms).
        let clamped = now_ms.max(self.clock_floor_ms);
        self.clock_floor_ms = clamped;
        if self.global_gcra_tat_ms > clamped + 1_990 {
            return false;
        }
        self.global_gcra_tat_ms = self.global_gcra_tat_ms.max(clamped) + 10;
        match provider {
            "apns" if self.active_apns_leases < 8 => {
                self.active_apns_leases += 1;
                true
            }
            "fcm" if self.active_fcm_leases < 8 => {
                self.active_fcm_leases += 1;
                true
            }
            "apns" | "fcm" if self.active_shared_leases < 16 => {
                self.active_shared_leases += 1;
                true
            }
            _ => false,
        }
    }
}

#[test]
fn hostile_client_simulator_enforces_endpoint_account_rotation_replay_and_lease_bounds() {
    let mut service = SimulatedKeeppeekService::new();
    let seed_v1 = [0x12; 32];
    let seed_v2 = [0x34; 32];
    let nvr_a = [0xa1; 16];
    let nvr_b = [0xb2; 16];
    let nvr_c = [0xc3; 16];

    service.register_endpoint(
        nvr_a,
        EndpointAuthority {
            stable_endpoint_id: "ep-1".to_owned(),
            account_id: "acct-1".to_owned(),
            binding_generation: 1,
            active_key_version: 1,
            public_key: public_key_from_seed(&seed_v1).unwrap(),
            revoked: false,
        },
    );

    // Hostile client strips NVR pacing and blasts 125 unique events inside 1 hour.
    let mut first_body = [0_u8; CANONICAL_BODY_BYTES];
    for idx in 0..125_u16 {
        let mut event_id = [0_u8; 16];
        event_id[..2].copy_from_slice(&idx.to_be_bytes());
        let body = sign_signal(
            SignalEnvelope {
                nvr_id: nvr_a,
                cloud_event_id: event_id,
                issued_at_secs: 1_756_728_000,
                signing_key_version: 1,
            },
            &seed_v1,
        )
        .unwrap();
        if idx == 0 {
            first_body = body;
        }
        let status = service.admit_signal(&body, 10_000_000 + i64::from(idx));
        if idx < 120 {
            assert_eq!(status, 202);
        } else {
            assert_eq!(status, 429);
        }
    }

    // Exact replay returns 202 without consuming extra capacity; same-ID different-body is 409.
    assert_eq!(service.admit_signal(&first_body, 10_001_000), 202);
    let mutated = sign_signal(
        SignalEnvelope {
            nvr_id: nvr_a,
            cloud_event_id: [0_u8; 16],
            issued_at_secs: 1_756_728_001,
            signing_key_version: 1,
        },
        &seed_v1,
    )
    .unwrap();
    assert_eq!(service.admit_signal(&mutated, 10_001_000), 409);

    // Credential rotation does NOT restore stable endpoint capacity.
    service.register_endpoint(
        nvr_a,
        EndpointAuthority {
            stable_endpoint_id: "ep-1".to_owned(),
            account_id: "acct-1".to_owned(),
            binding_generation: 1,
            active_key_version: 2,
            public_key: public_key_from_seed(&seed_v2).unwrap(),
            revoked: false,
        },
    );
    let rotated = sign_signal(
        SignalEnvelope {
            nvr_id: nvr_a,
            cloud_event_id: [0x99; 16],
            issued_at_secs: 1_756_728_005,
            signing_key_version: 2,
        },
        &seed_v2,
    )
    .unwrap();
    assert_eq!(service.admit_signal(&rotated, 10_002_000), 429);

    // Adding endpoints under the same account hits the 300/hour account ceiling.
    for (nvr, ep_id) in [(nvr_b, "ep-2"), (nvr_c, "ep-3")] {
        service.register_endpoint(
            nvr,
            EndpointAuthority {
                stable_endpoint_id: ep_id.to_owned(),
                account_id: "acct-1".to_owned(),
                binding_generation: 1,
                active_key_version: 1,
                public_key: public_key_from_seed(&seed_v1).unwrap(),
                revoked: false,
            },
        );
    }
    let mut extra_accepted = 0_usize;
    for (nvr, count) in [(nvr_b, 120_u16), (nvr_c, 120_u16)] {
        for idx in 0..count {
            let mut event_id = [0_u8; 16];
            event_id[0] = nvr[0];
            event_id[1..3].copy_from_slice(&idx.to_be_bytes());
            let body = sign_signal(
                SignalEnvelope {
                    nvr_id: nvr,
                    cloud_event_id: event_id,
                    issued_at_secs: 1_756_728_010,
                    signing_key_version: 1,
                },
                &seed_v1,
            )
            .unwrap();
            if service.admit_signal(&body, 10_003_000 + i64::from(idx)) == 202 {
                extra_accepted += 1;
            }
        }
    }
    // 120 from ep-1 + 120 from ep-2 + 60 from ep-3 = exact 300/hour account cap.
    assert_eq!(120 + extra_accepted, 300);

    // Provider lease partitioning: 8 reserved APNs, 8 reserved FCM, 16 shared = 32 total.
    let mut apns_leases = 0;
    for _ in 0..30 {
        if service.acquire_provider_lease("apns", 20_000_000) {
            apns_leases += 1;
        }
    }
    assert_eq!(apns_leases, 24); // 8 APNs reserved + 16 shared.
    let mut fcm_leases = 0;
    for _ in 0..20 {
        if service.acquire_provider_lease("fcm", 20_000_000) {
            fcm_leases += 1;
        }
    }
    assert_eq!(fcm_leases, 8); // 8 FCM reserved remain protected despite APNs saturation!
    assert_eq!(INTENT_TTL_MS, 300_000);
}
