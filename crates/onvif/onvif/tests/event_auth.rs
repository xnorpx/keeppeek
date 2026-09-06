use std::time::Duration;

use digest_auth::{AuthContext, AuthorizationHeader, HttpMethod, Qop};
use onvif::{
    event::{Client, Endpoint, Operation, Request, Subscription},
    soap::client::Credentials,
};
use test_hikvision::{FakeHikvision, Reply, onvif::FakeOnvif};

const SOAP: &str = "http://www.w3.org/2003/05/soap-envelope";
const SECURITY: &str =
    "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
const SOAP_OK: &str = "<s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\"><s:Body><ok/></s:Body></s:Envelope>";
const JPEG_PATH: &str = "/ISAPI/Streaming/channels/101/picture";
const FAKE_NONCE: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn soap_auth_int_signs_each_current_envelope_method_and_query() {
    let fake = FakeHikvision::builder().start().unwrap();
    let camera = Endpoint::new(format!("{}/onvif/event", fake.origin())).unwrap();
    let endpoint = camera.resolve("?source=a%2Fb&source=c+d").unwrap();
    let mut client = Client::new(
        camera,
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    fake.enqueue(Reply::raw(format!(
        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"fake-hikvision\", \
         nonce=\"{FAKE_NONCE}\", algorithm=MD5, qop=\"auth-int\"\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    )))
    .unwrap();
    let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
    for _ in 0..2 {
        fake.enqueue(Reply::http(200, "application/soap+xml", SOAP_OK))
            .unwrap();
        client.execute(&request, Duration::from_secs(1)).unwrap();
    }
    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].header("authorization").is_none());
    for request in &requests[1..] {
        assert_eq!(request.method(), "POST");
        assert_eq!(request.target(), "/onvif/event?source=a%2Fb&source=c+d");
        assert_username_token(request.body(), "test");
        assert!(digest_matches(request, HttpMethod::POST, request.body()));
        assert!(!digest_matches(request, HttpMethod::GET, request.body()));
        assert!(!digest_matches(request, HttpMethod::POST, &[]));
    }
    assert_ne!(requests[1].body(), requests[2].body());
    assert!(!digest_matches(
        &requests[2],
        HttpMethod::POST,
        requests[1].body()
    ));
}

fn digest_matches(
    request: &test_hikvision::CapturedRequest,
    method: HttpMethod<'_>,
    body: &[u8],
) -> bool {
    let mut header = AuthorizationHeader::parse(request.header("authorization").unwrap()).unwrap();
    assert_eq!(header.qop, Some(Qop::AUTH_INT));
    assert_eq!(header.uri, request.target());
    let supplied = header.response.clone();
    let context =
        AuthContext::new_with_method("test", "test", request.target(), Some(body), method);
    header.digest(&context);
    supplied == header.response
}

#[test]
fn digest_stale_challenge_must_supply_a_new_nonce() {
    for soap in [false, true] {
        let fake = FakeHikvision::builder().start().unwrap();
        let endpoint = Endpoint::new(format!("{}/onvif/event", fake.origin())).unwrap();
        let mut client = Client::new(
            endpoint.clone(),
            Credentials {
                username: "test".to_owned(),
                password: "test".to_owned(),
            },
        )
        .unwrap();
        for stale in [false, true] {
            fake.enqueue(digest_challenge("fake-hikvision", "repeated-nonce", stale))
                .unwrap();
        }
        let error = if soap {
            let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
            client
                .execute(&request, Duration::from_secs(1))
                .unwrap_err()
        } else {
            client
                .snapshot(
                    &endpoint.resolve(JPEG_PATH).unwrap(),
                    Duration::from_secs(1),
                )
                .unwrap_err()
        };
        assert!(error.is_authentication());
        assert_eq!(fake.requests().len(), 2);
    }
}

#[test]
fn digest_stale_challenges_share_a_three_attempt_budget() {
    for soap in [false, true] {
        let fake = FakeHikvision::builder().start().unwrap();
        let endpoint = Endpoint::new(format!("{}/onvif/event", fake.origin())).unwrap();
        let mut client = Client::new(
            endpoint.clone(),
            Credentials {
                username: "test".to_owned(),
                password: "test".to_owned(),
            },
        )
        .unwrap();
        for nonce in ["expired-first", "expired-second", "expired-third"] {
            fake.enqueue(digest_challenge("fake-hikvision", nonce, true))
                .unwrap();
        }
        let error = if soap {
            let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
            client
                .execute(&request, Duration::from_secs(1))
                .unwrap_err()
        } else {
            client
                .snapshot(
                    &endpoint.resolve(JPEG_PATH).unwrap(),
                    Duration::from_secs(1),
                )
                .unwrap_err()
        };
        assert!(error.is_authentication());
        let requests = fake.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| !request.authenticated()));
        let first =
            digest_auth::AuthorizationHeader::parse(requests[1].header("authorization").unwrap())
                .unwrap();
        let second =
            digest_auth::AuthorizationHeader::parse(requests[2].header("authorization").unwrap())
                .unwrap();
        assert_eq!(first.cnonce, second.cnonce);
        assert_eq!((first.nc, second.nc), (1, 1));
    }
}

#[test]
fn digest_realm_change_reauthenticates_soap_and_get_with_verified_credentials() {
    for soap in [false, true] {
        let fake = FakeHikvision::builder().start().unwrap();
        let camera = Endpoint::new(format!("{}/onvif/event", fake.origin())).unwrap();
        let image = camera.resolve(JPEG_PATH).unwrap();
        let mut client = Client::new(
            camera.clone(),
            Credentials {
                username: "test".to_owned(),
                password: "test".to_owned(),
            },
        )
        .unwrap();
        fake.enqueue(digest_challenge("previous-realm", "previous-nonce", false))
            .unwrap();
        fake.enqueue(Reply::http(
            200,
            "image/jpeg",
            fake.resource(JPEG_PATH).unwrap(),
        ))
        .unwrap();
        client.snapshot(&image, Duration::from_secs(1)).unwrap();
        if soap {
            fake.enqueue(digest_challenge("fake-hikvision", FAKE_NONCE, false))
                .unwrap();
            fake.enqueue(Reply::http(200, "application/soap+xml", SOAP_OK))
                .unwrap();
            let request = Request::create(&camera, Duration::from_secs(90)).unwrap();
            client.execute(&request, Duration::from_secs(1)).unwrap();
        } else {
            client.snapshot(&image, Duration::from_secs(1)).unwrap();
        }
        let requests = fake.requests();
        assert_eq!(requests.len(), 4);
        assert!(!requests[2].authenticated());
        assert!(requests[3].authenticated());
        assert_eq!(requests[3].method(), if soap { "POST" } else { "GET" });
        if soap {
            assert_username_token(requests[3].body(), "test");
        }
        client.snapshot(&image, Duration::from_secs(1)).unwrap();
        assert_eq!(fake.requests().len(), 5);
        assert!(fake.requests()[4].authenticated());
    }
}

#[test]
fn digest_realm_rotation_does_not_extend_wrong_credential_retries() {
    for soap in [false, true] {
        let fake = FakeHikvision::builder().start().unwrap();
        let endpoint = Endpoint::new(format!("{}/onvif/event", fake.origin())).unwrap();
        let mut client = Client::new(
            endpoint.clone(),
            Credentials {
                username: "test".to_owned(),
                password: "private-wrong-password".to_owned(),
            },
        )
        .unwrap();
        for realm in ["realm-one", "realm-two", "realm-three"] {
            fake.enqueue(digest_challenge(realm, "peer-nonce", false))
                .unwrap();
        }
        let error = if soap {
            let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
            client
                .execute(&request, Duration::from_secs(1))
                .unwrap_err()
        } else {
            client
                .snapshot(
                    &endpoint.resolve(JPEG_PATH).unwrap(),
                    Duration::from_secs(1),
                )
                .unwrap_err()
        };
        assert!(error.is_authentication());
        assert_eq!(fake.requests().len(), 2);
        assert!(
            fake.requests()
                .iter()
                .all(|request| !request.authenticated())
        );
        assert!(!format!("{error:?} {error}").contains("private-wrong-password"));
    }
}

fn digest_challenge(realm: &str, nonce: &str, stale: bool) -> Reply {
    Reply::raw(format!(
        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"{realm}\", \
         nonce=\"{nonce}\", algorithm=MD5, qop=\"auth\", stale={stale}\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    ))
}

#[test]
fn digest_is_not_forwarded_to_an_advertised_subscription_port() {
    let subscription_peer = FakeOnvif::builder().start().unwrap();
    let fake = FakeOnvif::builder()
        .subscription_address(format!(
            "{}/onvif/subscription?key={{id}}&source=a%2Fb",
            subscription_peer.origin()
        ))
        .start()
        .unwrap();
    let camera = Endpoint::new(fake.events_endpoint()).unwrap();
    let mut client = Client::new(
        camera.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let request = Request::create(&camera, Duration::from_secs(90)).unwrap();
    let response = client.execute(&request, Duration::from_secs(1)).unwrap();
    let subscription = Subscription::parse(&camera, &response).unwrap();
    subscription_peer
        .next_response(Reply::http(200, "application/soap+xml", SOAP_OK))
        .unwrap();
    client
        .execute(
            &subscription.request(Operation::Synchronize).unwrap(),
            Duration::from_secs(1),
        )
        .unwrap();
    let requests = subscription_peer.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].header("authorization").is_none());
    assert!(requests[1].authenticated());
    assert_eq!(
        requests[1].target(),
        "/onvif/subscription?key=1&source=a%2Fb"
    );
    fake.next_response(Reply::http(200, "application/soap+xml", SOAP_OK))
        .unwrap();
    client.execute(&request, Duration::from_secs(1)).unwrap();
    let requests = fake.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].header("authorization").is_none());
    assert!(requests[3].authenticated());
}

#[test]
fn digest_authenticates_soap_and_snapshots_after_origin_round_trips() {
    let fake = FakeOnvif::builder().start().unwrap();
    let images = FakeHikvision::builder().start().unwrap();
    let camera = Endpoint::new(fake.events_endpoint()).unwrap();
    let endpoint = camera
        .resolve(format!("{}{JPEG_PATH}?source=main%2Fview", images.origin()))
        .unwrap();
    let mut client = Client::new(
        camera.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let request = Request::create(&camera, Duration::from_secs(90)).unwrap();
    client.execute(&request, Duration::from_secs(1)).unwrap();
    let expected = images.resource(JPEG_PATH).unwrap();
    assert_eq!(
        client.snapshot(&endpoint, Duration::from_secs(1)).unwrap(),
        expected
    );
    let image_requests = images.requests();
    assert_eq!(image_requests.len(), 2);
    assert!(image_requests[0].header("authorization").is_none());
    assert!(image_requests[1].authenticated());
    fake.next_response(Reply::http(200, "application/soap+xml", SOAP_OK))
        .unwrap();
    client.execute(&request, Duration::from_secs(1)).unwrap();
    let requests = fake.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].header("authorization").is_none());
    assert!(requests[3].authenticated());
    assert_username_token(requests[3].body(), "test");
}

#[test]
fn soap_keeps_escaped_username_tokens_with_verified_digest() {
    let username = "camera<&'owner";
    let password = "private-password-token";
    let fake = FakeOnvif::builder()
        .credentials(username, password)
        .start()
        .unwrap();
    let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
    let mut client = Client::new(
        endpoint.clone(),
        Credentials {
            username: username.to_owned(),
            password: password.to_owned(),
        },
    )
    .unwrap();
    let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
    client.execute(&request, Duration::from_secs(1)).unwrap();
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].authenticated());
    assert!(requests[1].authenticated());
    for request in &requests {
        assert_username_token(request.body(), username);
        assert!(!String::from_utf8_lossy(request.body()).contains(password));
    }
}

#[test]
fn snapshot_digest_does_not_disable_the_next_soap_username_token() {
    let fake = FakeHikvision::builder().start().unwrap();
    let camera = Endpoint::new(format!("{}/onvif/event", fake.origin())).unwrap();
    let mut client = Client::new(
        camera.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    client
        .snapshot(&camera.resolve(JPEG_PATH).unwrap(), Duration::from_secs(1))
        .unwrap();
    fake.enqueue(Reply::http(200, "application/soap+xml", SOAP_OK))
        .unwrap();
    let request = Request::create(&camera, Duration::from_secs(90)).unwrap();
    client.execute(&request, Duration::from_secs(1)).unwrap();
    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].authenticated());
    assert!(requests[1].body().is_empty());
    assert!(requests[2].authenticated());
    assert_eq!(requests[2].method(), "POST");
    assert_username_token(requests[2].body(), "test");
}

fn assert_username_token(body: &[u8], username: &str) {
    let root = xmltree::Element::parse(body).expect("invalid SOAP XML");
    let header = root
        .get_child(("Header", SOAP))
        .expect("missing SOAP Header");
    let security = header
        .get_child(("Security", SECURITY))
        .expect("missing WS-Security");
    let tokens: Vec<_> = security
        .children
        .iter()
        .filter_map(xmltree::XMLNode::as_element)
        .filter(|node| node.name == "UsernameToken" && node.namespace.as_deref() == Some(SECURITY))
        .collect();
    assert_eq!(tokens.len(), 1);
    let actual = tokens[0].get_child(("Username", SECURITY)).unwrap();
    assert_eq!(actual.get_text().as_deref(), Some(username));
    let password = tokens[0].get_child(("Password", SECURITY)).unwrap();
    assert_eq!(
        password.attributes.get("Type").map(String::as_str),
        Some(
            "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest"
        )
    );
}
