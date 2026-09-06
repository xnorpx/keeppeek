use std::time::{Duration, Instant};

use digest_auth::{AuthContext, AuthorizationHeader, HttpMethod, Qop};
use onvif::{
    event::{Client, ClientError, Endpoint, Request},
    soap::client::Credentials,
};
use test_hikvision::{FakeHikvision, Reply};

const JPEG_PATH: &str = "/ISAPI/Streaming/channels/101/picture";
const FAKE_NONCE: &str = "0123456789abcdef0123456789abcdef";

fn setup(fake: &FakeHikvision, password: &str) -> (Client, Endpoint) {
    let camera = Endpoint::new(format!("{}/onvif/event", fake.origin())).unwrap();
    let endpoint = camera.resolve(JPEG_PATH).unwrap();
    let client = Client::new(
        camera,
        Credentials {
            username: "test".to_owned(),
            password: password.to_owned(),
        },
    )
    .unwrap();
    (client, endpoint)
}

fn rejected(result: Result<Vec<u8>, ClientError>) -> ClientError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("snapshot unexpectedly accepted the response"),
    }
}

fn wire_reply(headers: &str, body: &[u8]) -> Reply {
    let mut bytes = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    bytes.extend_from_slice(body);
    Reply::raw(bytes)
}

fn paced_reply(headers: &str, body: &[u8]) -> Reply {
    let mut reply = wire_reply(headers, &[]);
    for chunk in body.chunks(8192) {
        reply = reply.then(Duration::from_millis(1), chunk.to_vec());
    }
    reply
}

fn challenge(nonce: &str, qop: &str, stale: bool) -> Vec<u8> {
    format!(
        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"fake-hikvision\", nonce=\"{nonce}\", algorithm=MD5, qop=\"{qop}\", stale={stale}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

fn authorization(request: &test_hikvision::CapturedRequest) -> AuthorizationHeader {
    let header = request
        .header("authorization")
        .expect("missing Digest header");
    AuthorizationHeader::parse(header).unwrap_or_else(|_| panic!("invalid Digest header"))
}

#[test]
fn snapshot_authenticates_an_empty_get_with_the_exact_query_uri() {
    let fake = FakeHikvision::builder().start().unwrap();
    let camera = Endpoint::new(format!("{}/onvif/event", fake.origin())).unwrap();
    let target = format!("{JPEG_PATH}?size=small&source=a%2Fb&source=c+d");
    let endpoint = camera.resolve(&target).unwrap();
    let mut client = Client::new(
        camera,
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();

    let jpeg = client.snapshot(&endpoint, Duration::from_secs(5)).unwrap();
    assert!(jpeg == fake.resource(JPEG_PATH).unwrap());
    assert!(jpeg.starts_with(&[0xff, 0xd8]));
    assert!(jpeg.ends_with(&[0xff, 0xd9]));
    assert!(
        jpeg.windows(9)
            .any(|header| { header == [0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0xb4, 0x01, 0x40] })
    );

    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].header("authorization").is_none());
    assert!(requests[1].authenticated());
    for request in &requests {
        assert_eq!(request.method(), "GET");
        assert!(request.target() == target);
        assert!(request.body().is_empty());
        assert!(request.header("content-type").is_none());
        assert!(request.header("soapaction").is_none());
        assert_eq!(request.header("accept-encoding"), Some("identity"));
    }
}

#[test]
fn snapshot_rejects_wrong_credentials_without_exposing_secrets() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "private-wrong-password");
    let endpoint = endpoint.resolve("?private-query=private-value").unwrap();
    let error = rejected(client.snapshot(&endpoint, Duration::from_secs(1)));
    assert!(error.is_authentication());
    assert_eq!(fake.requests().len(), 2);
    assert!(
        fake.requests()
            .iter()
            .all(|request| !request.authenticated())
    );
    let diagnostic = format!("{client:?} {endpoint:?} {error} {error:?}");
    for secret in ["private-wrong-password", "private-value", JPEG_PATH, "test"] {
        assert!(!diagnostic.contains(secret));
    }
}

#[test]
fn snapshot_never_follows_redirects_or_accepts_other_success_statuses() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    for status in [201, 204, 206, 301, 302, 303, 307, 308, 403, 404, 500] {
        fake.enqueue(Reply::raw(format!(
            "HTTP/1.1 {status} Test\r\nLocation: {JPEG_PATH}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )))
        .unwrap();
        let count = fake.requests().len();
        let error = rejected(client.snapshot(&endpoint, Duration::from_secs(1)));
        assert_eq!(error.http_status(), Some(status));
        assert_eq!(fake.requests().len(), count + 1);
    }
}

#[test]
fn snapshot_accepts_jpeg_media_type_parameters_and_identity_encoding() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    let expected = fake.resource(JPEG_PATH).unwrap();
    for media_type in [
        "image/jpeg",
        "image/jpeg; name=frame.jpg",
        "IMAGE/JPEG; name=frame.jpg",
    ] {
        let headers = format!(
            "Content-Type: {media_type}\r\nContent-Encoding: identity\r\nSet-Cookie: first=1\r\nSet-Cookie: second=2\r\n"
        );
        fake.enqueue(wire_reply(&headers, &expected)).unwrap();
        let jpeg = client.snapshot(&endpoint, Duration::from_secs(1)).unwrap();
        assert!(jpeg == expected);
    }
}

#[test]
fn snapshot_rejects_missing_and_wrong_jpeg_media_types() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    let jpeg = fake.resource(JPEG_PATH).unwrap();
    for headers in [
        "",
        "Content-Type: image/png\r\n",
        "Content-Type: image/jpeg-extra\r\n",
        "Content-Type: image/jpeg, image/png\r\n",
        "Content-Type: text/html\r\n",
        "Content-Type: multipart/mixed; boundary=frame\r\n",
        "Content-Type: \r\n",
    ] {
        fake.enqueue(wire_reply(headers, &jpeg)).unwrap();
        rejected(client.snapshot(&endpoint, Duration::from_secs(1)));
    }
}

#[test]
fn snapshot_rejects_duplicate_selected_headers() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    for (name, value) in [
        ("Content-Type", "image/jpeg"),
        ("Content-Length", "4"),
        ("Content-Encoding", "identity"),
        ("Transfer-Encoding", "chunked"),
        ("WWW-Authenticate", "Digest realm=private-realm"),
        ("Location", "/private-location"),
    ] {
        let headers = format!(
            "Content-Type: image/jpeg\r\n{name}: {value}\r\n{}: {value}\r\n",
            name.to_ascii_lowercase()
        );
        fake.enqueue(wire_reply(&headers, &[0xff, 0xd8, 0xff, 0xd9]))
            .unwrap();
        rejected(client.snapshot(&endpoint, Duration::from_secs(1)));
    }
}

#[test]
fn snapshot_rejects_unsupported_encodings_and_ambiguous_lengths() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    for extra in [
        "Content-Encoding: gzip\r\n",
        "Content-Encoding: br\r\n",
        "Content-Encoding: identity, identity\r\n",
        "Transfer-Encoding: gzip\r\n",
        "Transfer-Encoding: gzip, chunked\r\n",
        "Transfer-Encoding: chunked, gzip\r\n",
        "Transfer-Encoding: identity\r\n",
        "Transfer-Encoding: chunked\r\nContent-Length: 4\r\n",
        "Content-Length: +4\r\n",
        "Content-Length: -1\r\n",
        "Content-Length: 4, 4\r\n",
        "Content-Length: 18446744073709551616\r\n",
    ] {
        let headers = format!("Content-Type: image/jpeg\r\n{extra}");
        fake.enqueue(wire_reply(&headers, &[0xff, 0xd8, 0xff, 0xd9]))
            .unwrap();
        rejected(client.snapshot(&endpoint, Duration::from_secs(1)));
    }
}

#[test]
fn snapshot_rejects_incomplete_jpeg_boundaries() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    for body in [
        &[][..],
        &[0xff, 0xd8][..],
        &[0xff, 0xd9][..],
        &[0x00, 0xff, 0xd8, 0xff, 0xd9][..],
        &[0xff, 0xd8, 0xff, 0xd9, 0x00][..],
        b"private-response-body",
    ] {
        fake.enqueue(Reply::http(200, "image/jpeg", body)).unwrap();
        let error = rejected(client.snapshot(&endpoint, Duration::from_secs(1)));
        assert!(!format!("{error} {error:?}").contains("private-response-body"));
    }
}

#[test]
fn snapshot_enforces_the_inclusive_one_mib_limit_with_and_without_a_length() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    let mut body = vec![0_u8; 1024 * 1024];
    body[..2].copy_from_slice(&[0xff, 0xd8]);
    let length = body.len();
    body[length - 2..].copy_from_slice(&[0xff, 0xd9]);
    for declared in [true, false] {
        let headers = if declared {
            format!("Content-Type: image/jpeg\r\nContent-Length: {length}\r\n")
        } else {
            "Content-Type: image/jpeg\r\n".to_owned()
        };
        fake.enqueue(paced_reply(&headers, &body)).unwrap();
        let jpeg = client.snapshot(&endpoint, Duration::from_secs(1)).unwrap();
        assert!(jpeg == body);
    }
    body.insert(2, 0);
    for reply in [
        Reply::http(200, "image/jpeg", &body),
        paced_reply("Content-Type: image/jpeg\r\n", &body).hold_open(),
    ] {
        fake.enqueue(reply).unwrap();
        let error = rejected(client.snapshot(&endpoint, Duration::from_secs(1)));
        assert_eq!(error.to_string(), "invalid ONVIF event protocol response");
    }
}

#[test]
fn snapshot_accepts_chunked_jpeg_without_other_transfer_codings() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    let expected = fake.resource(JPEG_PATH).unwrap();
    let mut body = Vec::new();
    for chunk in expected.chunks(251) {
        body.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        body.extend_from_slice(chunk);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");
    fake.enqueue(
        wire_reply(
            "Content-Type: image/jpeg\r\nTransfer-Encoding: chunked\r\n",
            &body,
        )
        .fragmented(7)
        .unwrap(),
    )
    .unwrap();
    let jpeg = client.snapshot(&endpoint, Duration::from_secs(1)).unwrap();
    assert!(jpeg == expected);
}

#[test]
fn snapshot_reuses_digest_state_across_soap_and_get_requests() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    let events = endpoint.resolve("/onvif/event").unwrap();
    let request = Request::create(&events, Duration::from_secs(90)).unwrap();
    let soap = "<s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\"><s:Body><ok/></s:Body></s:Envelope>";
    fake.enqueue(Reply::raw(challenge(FAKE_NONCE, "auth", false)))
        .unwrap();
    fake.enqueue(Reply::http(200, "application/soap+xml", soap))
        .unwrap();
    client.execute(&request, Duration::from_secs(1)).unwrap();
    client.snapshot(&endpoint, Duration::from_secs(1)).unwrap();
    fake.enqueue(Reply::http(200, "application/soap+xml", soap))
        .unwrap();
    client.execute(&request, Duration::from_secs(1)).unwrap();

    let requests = fake.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].header("authorization").is_none());
    assert!(
        requests[1..]
            .iter()
            .all(test_hikvision::CapturedRequest::authenticated)
    );
    assert_eq!(requests[2].method(), "GET");
    assert!(requests[2].body().is_empty());
    assert_eq!(requests[3].method(), "POST");
    let first = authorization(&requests[1]);
    let second = authorization(&requests[2]);
    let third = authorization(&requests[3]);
    assert_eq!((first.nc, second.nc, third.nc), (1, 2, 3));
    assert!(first.cnonce == second.cnonce);
    assert!(second.cnonce == third.cnonce);
}

#[test]
fn snapshot_hashes_the_empty_entity_for_digest_auth_int() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    let endpoint = endpoint
        .resolve("?image=main%2Fview&token=fixture")
        .unwrap();
    fake.enqueue(Reply::raw(challenge("auth-int-nonce", "auth-int", false)))
        .unwrap();
    fake.enqueue(Reply::http(
        200,
        "image/jpeg",
        fake.resource(JPEG_PATH).unwrap(),
    ))
    .unwrap();
    client.snapshot(&endpoint, Duration::from_secs(1)).unwrap();

    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].header("authorization").is_none());
    assert!(requests[1].body().is_empty());
    let mut header = authorization(&requests[1]);
    assert_eq!(header.qop, Some(Qop::AUTH_INT));
    assert!(header.uri == requests[1].target());
    let supplied = header.response.clone();
    let context = AuthContext::new_with_method(
        "test",
        "test",
        requests[1].target(),
        Some(&[]),
        HttpMethod::GET,
    );
    header.digest(&context);
    assert!(supplied == header.response);
    for (method, body) in [
        (HttpMethod::POST, &[][..]),
        (HttpMethod::GET, &b"not-an-empty-entity"[..]),
    ] {
        let context =
            AuthContext::new_with_method("test", "test", requests[1].target(), Some(body), method);
        header.digest(&context);
        assert_ne!(supplied, header.response);
    }
}

#[test]
fn snapshot_retries_a_stale_challenge_without_changing_the_client_nonce() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    fake.enqueue(Reply::raw(challenge("expired-nonce", "auth", false)))
        .unwrap();
    fake.enqueue(Reply::raw(challenge(FAKE_NONCE, "auth", true)))
        .unwrap();
    let jpeg = client.snapshot(&endpoint, Duration::from_secs(1)).unwrap();
    assert!(jpeg == fake.resource(JPEG_PATH).unwrap());
    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    assert!(!requests[1].authenticated());
    assert!(requests[2].authenticated());
    assert!(authorization(&requests[1]).cnonce == authorization(&requests[2]).cnonce);
}

#[test]
fn snapshot_stops_after_three_authentication_attempts() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    for nonce in ["expired-first", "expired-second", "expired-third"] {
        fake.enqueue(Reply::raw(challenge(nonce, "auth", true)))
            .unwrap();
    }
    let error = rejected(client.snapshot(&endpoint, Duration::from_secs(1)));
    assert!(error.is_authentication());
    assert_eq!(fake.requests().len(), 3);
}

#[test]
fn snapshot_never_falls_back_to_basic_even_with_a_cached_digest_challenge() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    let basic = b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"private\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    fake.enqueue(Reply::raw(basic.to_vec())).unwrap();
    assert!(rejected(client.snapshot(&endpoint, Duration::from_secs(1))).is_authentication());
    client.snapshot(&endpoint, Duration::from_secs(1)).unwrap();
    fake.enqueue(Reply::raw(basic.to_vec())).unwrap();
    assert!(rejected(client.snapshot(&endpoint, Duration::from_secs(1))).is_authentication());
    let requests = fake.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].header("authorization").is_none());
    for request in requests {
        assert!(
            request
                .header("authorization")
                .is_none_or(|value| value.starts_with("Digest "))
        );
        assert!(request.body().is_empty());
    }
}

#[test]
fn snapshot_rejects_invalid_timeouts_and_foreign_endpoints_before_network_io() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    for timeout in [
        Duration::ZERO,
        Duration::from_secs(5) + Duration::from_nanos(1),
        Duration::MAX,
    ] {
        rejected(client.snapshot(&endpoint, timeout));
    }
    let foreign = Endpoint::new("http://127.0.0.2:9/onvif/event").unwrap();
    rejected(client.snapshot(&foreign, Duration::from_secs(1)));
    for userinfo in ["private-user@", "private-user:private-password@"] {
        let url = format!("http://{userinfo}{}{JPEG_PATH}", fake.address());
        endpoint
            .resolve(url)
            .expect_err("embedded credentials must be rejected");
    }
    let camera = Endpoint::new(format!("https://{}/onvif/event", fake.address())).unwrap();
    let mut tls = Client::new(
        camera,
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    rejected(tls.snapshot(&endpoint, Duration::from_secs(1)));
    assert!(fake.requests().is_empty());
}

#[test]
fn snapshot_deadline_covers_stalled_headers_and_body() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    for reply in [
        Reply::raw(Vec::new()).hold_open(),
        wire_reply(
            "Content-Type: image/jpeg\r\nContent-Length: 100\r\n",
            &[0xff, 0xd8],
        )
        .hold_open(),
    ] {
        fake.enqueue(reply).unwrap();
        let timeout = Duration::from_millis(150);
        let started = Instant::now();
        let error = rejected(client.snapshot(&endpoint, timeout));
        let elapsed = started.elapsed();
        assert_eq!(
            error.to_string(),
            "ONVIF event network request failed or timed out"
        );
        assert!(elapsed >= timeout.saturating_sub(Duration::from_millis(25)));
        assert!(
            elapsed <= timeout + Duration::from_millis(100),
            "elapsed: {elapsed:?}"
        );
    }
    assert_eq!(fake.requests().len(), 2);
}

#[test]
fn snapshot_uses_one_deadline_across_challenges_and_the_body() {
    let fake = FakeHikvision::builder().start().unwrap();
    let (mut client, endpoint) = setup(&fake, "test");
    for (nonce, stale) in [("expired", false), (FAKE_NONCE, true)] {
        fake.enqueue(
            Reply::raw(Vec::new())
                .then(Duration::from_millis(100), challenge(nonce, "auth", stale)),
        )
        .unwrap();
    }
    fake.enqueue(wire_reply("Content-Type: image/jpeg\r\n", &[0xff, 0xd8]).hold_open())
        .unwrap();
    let timeout = Duration::from_millis(350);
    let started = Instant::now();
    let error = rejected(client.snapshot(&endpoint, timeout));
    let elapsed = started.elapsed();
    assert_eq!(
        error.to_string(),
        "ONVIF event network request failed or timed out"
    );
    assert!(elapsed >= timeout.saturating_sub(Duration::from_millis(25)));
    assert!(
        elapsed <= timeout + Duration::from_millis(100),
        "elapsed: {elapsed:?}"
    );
    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].authenticated());
}
