use std::time::{Duration, Instant};

use onvif::{
    event::{Client, Endpoint},
    soap::client::Credentials,
};
use test_hikvision::{FakeHikvision, Reply};

#[test]
fn snapshot_uses_one_deadline_across_challenges_and_the_body() {
    let fake = FakeHikvision::builder().start().unwrap();
    let camera = Endpoint::new(format!("{}/onvif/event", fake.origin())).unwrap();
    let endpoint = camera
        .resolve("/ISAPI/Streaming/channels/101/picture")
        .unwrap();
    let mut client = Client::new(
        camera,
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    for (nonce, stale) in [
        ("expired", false),
        ("0123456789abcdef0123456789abcdef", true),
    ] {
        let challenge = format!(
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"fake-hikvision\", nonce=\"{nonce}\", algorithm=MD5, qop=\"auth\", stale={stale}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        fake.enqueue(
            Reply::raw(Vec::new()).then(Duration::from_millis(200), challenge.into_bytes()),
        )
        .unwrap();
    }
    let mut response =
        b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: image/jpeg\r\n\r\n".to_vec();
    response.extend_from_slice(&[0xff, 0xd8]);
    fake.enqueue(Reply::raw(response).hold_open()).unwrap();
    let timeout = Duration::from_secs(1);
    let started = Instant::now();
    let error = client
        .snapshot(&endpoint, timeout)
        .expect_err("incomplete snapshot must time out");
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
