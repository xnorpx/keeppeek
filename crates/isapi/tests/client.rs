#![cfg(feature = "ureq")]

use std::thread;
use std::time::{Duration, Instant};
use test_hikvision::{FakeHikvision, Reply};

use isapi::Credentials;
use isapi::blocking::Client;

fn serve(responses: Vec<String>) -> (String, FakeHikvision) {
    let camera = FakeHikvision::builder()
        .replies(
            responses
                .into_iter()
                .map(|response| Reply::raw(response.into_bytes())),
        )
        .start()
        .unwrap();
    (camera.origin(), camera)
}

fn idle_camera(prefix: &[u8]) -> FakeHikvision {
    let reply = Reply::raw(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/mixed; boundary=camera\r\nConnection: close\r\n\r\n".to_vec())
        .then(Duration::ZERO, prefix.to_vec()).hold_open();
    FakeHikvision::builder().replies([reply]).start().unwrap()
}

fn credentials() -> Credentials {
    Credentials::new("test-operator", "test-secret")
}

#[test]
fn management_adapter_validates_device_failures_and_snapshot_media_types() {
    let rejected = "<ResponseStatus><statusCode>4</statusCode><subStatusCode>notSupport</subStatusCode></ResponseStatus>";
    let (origin, worker) = serve(vec![format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{rejected}",
        rejected.len()
    )]);
    let mut client = Client::new(&origin, credentials()).unwrap();
    assert_eq!(
        client
            .query(&isapi::management::DeviceInfo::query().unwrap())
            .unwrap_err()
            .device_status(),
        Some(4)
    );
    assert_eq!(worker.requests().len(), 1);
    let (origin, worker) = serve(vec!["HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: 5\r\nConnection: close\r\n\r\n<ok/>".to_owned()]);
    let mut client = Client::new(&origin, credentials()).unwrap();
    assert!(client.snapshot(101).is_err());
    assert!(worker.requests()[0].target() == "/ISAPI/Streaming/channels/101/picture");
}

#[test]
fn digest_challenge_preserves_query_and_keeps_credentials_out_of_urls() {
    let (origin, worker) = serve(vec![
        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"camera\", nonce=\"test-nonce\", qop=\"auth\", algorithm=MD5\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n<ok/>".into(),
    ]);
    let mut client = Client::new(&origin, credentials()).unwrap();
    let body = client.get("/ISAPI/System/deviceInfo?channel=1").unwrap();
    assert_eq!(body, b"<ok/>");
    let requests = worker.requests();
    assert!(requests[0].header("authorization").is_none());
    let authorization = requests[1].header("authorization").unwrap();
    assert!(authorization.starts_with("Digest "));
    assert!(authorization.contains("uri=\"/ISAPI/System/deviceInfo?channel=1\""));
    assert_eq!(requests[1].method(), "GET");
    assert_eq!(requests[1].target(), "/ISAPI/System/deviceInfo?channel=1");
    assert!(!authorization.contains("test-secret"));
}

#[test]
fn redirects_are_rejected_without_contacting_the_target() {
    let target = FakeHikvision::builder().start().unwrap();
    let (origin, worker) = serve(vec![format!(
        "HTTP/1.1 302 Found\r\nLocation: http://{}/ISAPI/System/deviceInfo\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        target.address()
    )]);
    let mut client = Client::new(&origin, credentials()).unwrap();
    assert_eq!(
        client
            .get("/ISAPI/System/deviceInfo")
            .unwrap_err()
            .http_status(),
        Some(302)
    );
    assert_eq!(worker.requests().len(), 1);
    assert!(target.requests().is_empty());
}

#[test]
fn rejects_unsafe_origins_and_paths_before_network_io() {
    for origin in [
        "file:///etc/passwd",
        "http://user:secret@127.0.0.1",
        "http://127.0.0.1/path",
        "http://127.0.0.1?query=1",
        "http://127.0.0.1#fragment",
    ] {
        assert!(
            Client::new(origin, credentials())
                .unwrap_err()
                .is_invalid_input()
        );
    }
    let mut client = Client::new("http://127.0.0.1:9", credentials()).unwrap();
    for resource in [
        "http://example.com/ISAPI/",
        "//example.com/ISAPI/",
        "/ISAPI/../other",
        "/ISAPI/test#fragment",
    ] {
        assert!(client.get(resource).unwrap_err().is_invalid_input());
    }
}

#[test]
fn debug_output_never_contains_credentials() {
    let credentials = credentials();
    let credential_debug = format!("{credentials:?}");
    let client = Client::new("http://127.0.0.1:9", credentials).unwrap();
    let client_debug = format!("{client:?}");
    for output in [credential_debug, client_debug] {
        assert!(!output.contains("test-secret"));
        assert!(!output.contains("test-operator"));
    }
}

#[test]
fn persistent_alert_stream_emits_parts_without_reopening_between_events() {
    let event = "<EventNotificationAlert><eventType>VMD</eventType><eventState>active</eventState></EventNotificationAlert>";
    let body = format!(
        "--camera\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n{event}\r\n--camera\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}\r\n--camera--\r\n",
        event.len()
    );
    let (origin, worker) = serve(vec![format!(
        "HTTP/1.1 200 OK\r\nContent-Type: multipart/mixed; boundary=camera\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )]);
    let mut client = Client::new(&origin, credentials()).unwrap();
    let mut stream = client.alert_stream(Duration::from_secs(2)).unwrap();
    let part = stream.next_part().unwrap().unwrap();
    assert_eq!(part.event().unwrap().unwrap().active(), Some(true));
    let part = stream.next_part().unwrap().unwrap();
    assert_eq!(part.kind(), isapi::PartKind::Json);
    assert!(part.event().unwrap_err().is_protocol());
    assert!(stream.next_part().unwrap().is_none());
    let requests = worker.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].target(),
        "/ISAPI/Event/notification/alertStream"
    );
}

#[test]
fn stale_digest_nonce_is_retried_but_bad_credentials_are_not_looped() {
    let challenge = |nonce: &str, stale: bool| {
        format!(
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"camera\", nonce=\"{nonce}\", qop=\"auth\", stale={stale}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    };
    let (origin, worker) = serve(vec![
        challenge("initial", false),
        challenge("fresh", true),
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    ]);
    let mut client = Client::new(&origin, credentials()).unwrap();
    client.get("/ISAPI/System/deviceInfo").unwrap();
    let requests = worker.requests();
    assert!(
        requests[1]
            .header("authorization")
            .unwrap()
            .contains("nonce=\"initial\"")
    );
    assert!(
        requests[2]
            .header("authorization")
            .unwrap()
            .contains("nonce=\"fresh\"")
    );
    let (origin, worker) = serve(vec![
        challenge("initial", false),
        challenge("initial", false),
    ]);
    let mut client = Client::new(&origin, credentials()).unwrap();
    assert!(
        client
            .get("/ISAPI/System/deviceInfo")
            .unwrap_err()
            .is_authentication()
    );
    assert_eq!(worker.requests().len(), 2);
}

#[test]
fn stream_lifetime_bounds_a_stalled_read() {
    let camera = idle_camera(&[]);
    let mut client = Client::new(camera.origin(), credentials()).unwrap();
    let mut stream = client.alert_stream(Duration::from_millis(100)).unwrap();
    let before = Instant::now();
    let error = stream.next_part().unwrap_err();
    assert!(error.is_timeout(), "{error}");
    assert!(before.elapsed() < Duration::from_secs(2));
}

#[test]
fn alert_body_outlives_the_connection_setup_timeout() {
    let reply = Reply::raw(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/mixed; boundary=camera\r\nConnection: close\r\n\r\n".to_vec())
        .then(Duration::from_millis(5500), b"--camera\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}\r\n--camera--\r\n".to_vec());
    let camera = FakeHikvision::builder().replies([reply]).start().unwrap();
    let mut client = Client::new(camera.origin(), credentials()).unwrap();
    let mut stream = client.alert_stream(Duration::from_secs(8)).unwrap();
    let result = stream.next_part();
    assert_eq!(result.unwrap().unwrap().body(), b"{}");
}

#[test]
fn cancellation_interrupts_an_idle_continuous_stream() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    let camera = idle_camera(&[]);
    let origin = camera.origin();
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancelled);
    let (ready_tx, ready_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut client = Client::builder(origin, credentials())
            .cancelled(move || signal.load(Ordering::Acquire))
            .build()
            .unwrap();
        let mut stream = client.subscribe().unwrap();
        ready_tx.send(()).unwrap();
        stream.next_part().unwrap_err()
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    cancelled.store(true, Ordering::Release);
    let error = worker.join().unwrap();
    assert!(error.is_cancelled(), "{error}");
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn continuous_stream_times_out_without_a_complete_notification() {
    let camera = idle_camera(
        b"--camera\r\nContent-Type: application/xml\r\nContent-Length: 20\r\n\r\n<partial>",
    );
    let mut client = Client::builder(camera.origin(), credentials())
        .idle_timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let mut stream = client.subscribe().unwrap();
    let before = Instant::now();
    let result = stream.next_part();
    assert!(result.unwrap_err().is_timeout());
    assert!(before.elapsed() < Duration::from_millis(500));
}

#[test]
fn caller_deadline_bounds_continuous_stream_reads() {
    let camera = idle_camera(&[]);
    let mut client = Client::new(camera.origin(), credentials()).unwrap();
    let mut stream = client.subscribe().unwrap();
    let before = Instant::now();
    let result = stream.next_part_until(before + Duration::from_millis(50));
    assert!(result.unwrap_err().is_timeout());
    assert!(before.elapsed() < Duration::from_millis(500));
}
