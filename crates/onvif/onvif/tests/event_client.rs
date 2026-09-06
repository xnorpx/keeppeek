use std::time::{Duration, Instant};

use onvif::{
    event::{Client, Endpoint, Operation, Pull, Request, Subscription},
    soap::client::Credentials,
};
use test_hikvision::onvif::{FakeOnvif, notification};

const SOAP_OK: &str = "<s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\"><s:Body><ok/></s:Body></s:Envelope>";

#[test]
fn soap_status_errors_do_not_wait_for_bodies_or_follow_redirects() {
    let fake = FakeOnvif::builder().start().unwrap();
    let (mut client, request) = response_client(&fake);
    client.execute(&request, Duration::from_secs(1)).unwrap();
    for status in [201, 204, 206, 301, 302, 303, 307, 308, 404] {
        fake.next_response(
            test_hikvision::Reply::raw(format!(
                "HTTP/1.1 {status} Test\r\nLocation: /private-redirect\r\n\
             Content-Type: application/soap+xml\r\nContent-Length: 1000\r\n\
             Connection: close\r\n\r\n"
            ))
            .hold_open(),
        )
        .unwrap();
        let before = fake.requests().len();
        let error = client
            .execute(&request, Duration::from_millis(150))
            .unwrap_err();
        assert_eq!(error.http_status(), Some(status));
        assert_eq!(fake.requests().len(), before + 1);
        assert!(!format!("{error:?} {error}").contains("private-redirect"));
    }
}

#[test]
fn soap_deadline_covers_stalled_headers_and_body() {
    let fake = FakeOnvif::builder().start().unwrap();
    let (mut client, request) = response_client(&fake);
    for reply in [
        test_hikvision::Reply::raw(Vec::new()).hold_open(),
        soap_reply(
            "Content-Type: application/soap+xml\r\nContent-Length: 1000\r\n",
            b"<s:Envelope",
        )
        .hold_open(),
    ] {
        fake.next_response(reply).unwrap();
        let timeout = Duration::from_millis(150);
        let started = Instant::now();
        let error = client.execute(&request, timeout).unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(
            error.to_string(),
            "ONVIF event network request failed or timed out"
        );
        assert!(elapsed >= timeout.saturating_sub(Duration::from_millis(25)));
        assert!(
            elapsed < timeout + Duration::from_millis(150),
            "elapsed: {elapsed:?}"
        );
    }
    assert_eq!(fake.requests().len(), 3);
}

#[test]
fn soap_rejects_duplicate_selected_headers() {
    let fake = FakeOnvif::builder().start().unwrap();
    let (mut client, request) = response_client(&fake);
    for (name, value) in [
        ("Content-Type", "application/soap+xml".to_owned()),
        ("Content-Length", SOAP_OK.len().to_string()),
        ("Content-Encoding", "identity".to_owned()),
        ("Transfer-Encoding", "chunked".to_owned()),
        ("WWW-Authenticate", "Digest realm=private-realm".to_owned()),
        ("Location", "/private-peer-location".to_owned()),
    ] {
        let headers = format!(
            "Content-Type: application/soap+xml\r\n{name}: {value}\r\n{}: {value}\r\n",
            name.to_ascii_lowercase()
        );
        fake.next_response(soap_reply(&headers, SOAP_OK.as_bytes()))
            .unwrap();
        let error = client
            .execute(&request, Duration::from_secs(1))
            .unwrap_err();
        assert!(!format!("{error:?} {error}").contains("private-peer"));
    }
}

#[test]
fn soap_rejects_unsupported_encodings_and_ambiguous_lengths() {
    let fake = FakeOnvif::builder().start().unwrap();
    let (mut client, request) = response_client(&fake);
    for extra in [
        "Content-Encoding: gzip\r\n",
        "Content-Encoding: br\r\n",
        "Content-Encoding: identity, identity\r\n",
        "Transfer-Encoding: gzip\r\n",
        "Transfer-Encoding: gzip, chunked\r\n",
        "Transfer-Encoding: chunked, gzip\r\n",
        "Transfer-Encoding: identity\r\n",
        "Transfer-Encoding: chunked\r\nContent-Length: 1\r\n",
        "Content-Length: +109\r\n",
        "Content-Length: -1\r\n",
        "Content-Length: 109, 109\r\n",
        "Content-Length: 18446744073709551617\r\n",
    ] {
        let headers = format!("Content-Type: application/soap+xml\r\n{extra}");
        fake.next_response(soap_reply(&headers, SOAP_OK.as_bytes()))
            .unwrap();
        client
            .execute(&request, Duration::from_secs(1))
            .unwrap_err();
    }
}

#[test]
fn soap_accepts_media_parameters_identity_casing_and_chunked_bodies() {
    let fake = FakeOnvif::builder().start().unwrap();
    let (mut client, request) = response_client(&fake);
    for media_type in [
        "application/soap+xml",
        "APPLICATION/SOAP+XML; charset=utf-8",
    ] {
        let headers = format!(
            "Content-Type: {media_type}\r\nContent-Encoding: IDENTITY\r\n\
             Transfer-Encoding: chunked\r\nSet-Cookie: first=1\r\nSet-Cookie: second=2\r\n"
        );
        let chunks = format!("{:x}\r\n{SOAP_OK}\r\n0\r\n\r\n", SOAP_OK.len());
        fake.next_response(
            soap_reply(&headers, chunks.as_bytes())
                .fragmented(7)
                .unwrap(),
        )
        .unwrap();
        let body = client.execute(&request, Duration::from_secs(1)).unwrap();
        assert_eq!(body, SOAP_OK.as_bytes());
    }
}

fn response_client(fake: &FakeOnvif) -> (Client, Request) {
    let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
    let client = Client::new(
        endpoint.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
    (client, request)
}

#[test]
fn soap_http_failures_are_typed_before_decoding_private_non_xml_bodies() {
    let fake = FakeOnvif::builder().start().unwrap();
    let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
    let mut client = Client::new(
        endpoint.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
    for status in [400, 403, 404, 410, 429, 500, 503] {
        fake.next_response(test_hikvision::Reply::http(
            status,
            "text/html",
            format!("private-peer-body {}", fake.origin()),
        ))
        .unwrap();
        let before = fake.requests().len();
        let error = client
            .execute(&request, Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(error.http_status(), Some(status));
        assert_eq!(
            fake.requests().len() - before,
            if before == 0 { 2 } else { 1 }
        );
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains("private-peer-body"));
        assert!(!diagnostic.contains(&fake.origin()));
    }
}

#[test]
fn soap_rejects_missing_and_incorrect_media_types() {
    let fake = FakeOnvif::builder().start().unwrap();
    let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
    let mut client = Client::new(
        endpoint.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
    for headers in [
        "",
        "Content-Type: text/html\r\n",
        "Content-Type: text/xml\r\n",
        "Content-Type: application/soap+xml-extra\r\n",
        "Content-Type: application/soap+xml, application/json\r\n",
        "Content-Type: multipart/related; boundary=private-boundary\r\n",
        "Content-Type: \r\n",
    ] {
        fake.next_response(soap_reply(headers, SOAP_OK.as_bytes()))
            .unwrap();
        let error = client
            .execute(&request, Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(error.to_string(), "invalid ONVIF event protocol response");
    }
}

fn soap_reply(headers: &str, body: &[u8]) -> test_hikvision::Reply {
    let mut bytes = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    bytes.extend_from_slice(body);
    test_hikvision::Reply::raw(bytes)
}

#[test]
fn pull_limits_reject_hostile_durations_without_panicking() {
    let fake = FakeOnvif::builder().start().unwrap();
    let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
    let mut client = Client::new(
        endpoint.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
    let durations = [
        "PT18446744073709551617S".to_owned(),
        "PT18446744073709551617M".to_owned(),
        "PT18446744073709551617H".to_owned(),
        "P18446744073709551617D".to_owned(),
        "P18446744073709551617Y".to_owned(),
        format!("PT0.{}S", "9".repeat(1024)),
        "PT0.18446744073709551617S".to_owned(),
        "-PT1S".to_owned(),
        "PT-1S".to_owned(),
        "PTNaNS".to_owned(),
        "PTinfS".to_owned(),
        "PT1e100S".to_owned(),
        "PT0S".to_owned(),
        "PT120.001S".to_owned(),
        "PT2M1S".to_owned(),
        "P1D".to_owned(),
        "PT1H".to_owned(),
        "PT0.999999999999999999S999999999999999999".to_owned(),
    ];
    for duration in durations {
        fake.next_response(pull_limit_reply(&duration)).unwrap();
        let error = client
            .execute(&request, Duration::from_secs(5))
            .unwrap_err();
        assert!(
            error.pull_limits().is_none(),
            "accepted duration {duration}"
        );
        assert_eq!(error.to_string(), "invalid ONVIF event protocol response");
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains("peer-private-payload"));
        assert!(!diagnostic.contains(&fake.origin()));
    }
}

#[test]
fn pull_limits_accept_bounded_xsd_duration_spellings() {
    let fake = FakeOnvif::builder().start().unwrap();
    let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
    let mut client = Client::new(
        endpoint.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let request = Request::create(&endpoint, Duration::from_secs(90)).unwrap();
    for (text, expected) in [
        ("PT0.000000001S", Duration::from_nanos(1)),
        ("PT0.25S", Duration::from_millis(250)),
        ("PT10S", Duration::from_secs(10)),
        ("PT1M60S", Duration::from_secs(10)),
        ("PT2M", Duration::from_secs(10)),
        ("PT120S", Duration::from_secs(10)),
        ("P0DT0H2M0S", Duration::from_secs(10)),
    ] {
        fake.next_response(pull_limit_reply(text)).unwrap();
        let error = client
            .execute(&request, Duration::from_secs(5))
            .unwrap_err();
        let limits = error
            .pull_limits()
            .unwrap_or_else(|| panic!("rejected {text}"));
        assert_eq!(limits.timeout, expected, "duration {text}");
        assert_eq!(limits.messages, 32);
    }
}

fn pull_limit_reply(duration: &str) -> test_hikvision::Reply {
    let body = format!(
        "<s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" \
         xmlns:e=\"http://www.onvif.org/ver10/events/wsdl\"><s:Body><s:Fault>\
         <s:Code><s:Value>s:Receiver</s:Value></s:Code>\
         <s:Reason><s:Text xml:lang=\"en\">peer-private-payload</s:Text></s:Reason>\
         <s:Detail><e:PullMessagesFaultResponse><e:MaxTimeout>{duration}</e:MaxTimeout>\
         <e:MaxMessageLimit>32</e:MaxMessageLimit></e:PullMessagesFaultResponse>\
         </s:Detail></s:Fault></s:Body></s:Envelope>"
    );
    test_hikvision::Reply::http(500, "application/soap+xml; charset=utf-8", body)
}

#[test]
fn pullpoint_client_authenticates_preserves_reference_headers_and_closes() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_secs(3))
        .notifications(vec![notification(
            "VideoSource/MotionAlarm",
            true,
            "Changed",
            "2026-09-05T12:00:00Z",
            "source-2",
        )])
        .start()
        .unwrap();
    let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
    let mut client = Client::new(
        endpoint.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let response = client
        .execute(
            &Request::create(&endpoint, Duration::from_secs(90)).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
    let subscription = Subscription::parse(&endpoint, &response).unwrap();
    assert_eq!(fake.active_subscriptions(), 1);
    client
        .execute(
            &subscription.request(Operation::Synchronize).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
    let pulled = client
        .execute(
            &subscription
                .request(Operation::Pull {
                    timeout: Duration::from_secs(1),
                    limit: 32,
                })
                .unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(Pull::parse(&pulled).unwrap().notifications.len(), 1);
    client
        .execute(
            &subscription
                .request(Operation::Renew {
                    lifetime: Duration::from_secs(90),
                })
                .unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(fake.renew_count(), 1);
    client
        .execute(
            &subscription.request(Operation::Unsubscribe).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(fake.active_subscriptions(), 0);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert!(!format!("{client:?}").contains("test"));
}

#[test]
fn pullpoint_fault_limits_and_bad_credentials_are_typed_and_payload_safe() {
    let fake = FakeOnvif::builder()
        .max_message_limit(2)
        .max_timeout(Duration::from_secs(1))
        .start()
        .unwrap();
    let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
    let mut client = Client::new(
        endpoint.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let response = client
        .execute(
            &Request::create(&endpoint, Duration::from_secs(90)).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
    let subscription = Subscription::parse(&endpoint, &response).unwrap();
    let error = client
        .execute(
            &subscription
                .request(Operation::Pull {
                    timeout: Duration::from_secs(2),
                    limit: 32,
                })
                .unwrap(),
            Duration::from_secs(5),
        )
        .unwrap_err();
    let limits = error.pull_limits().unwrap();
    assert_eq!(limits.timeout, Duration::from_secs(1));
    assert_eq!(limits.messages, 2);
    client
        .execute(
            &subscription.request(Operation::Unsubscribe).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
    let mut wrong = Client::new(
        endpoint.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "private-wrong".to_owned(),
        },
    )
    .unwrap();
    let error = wrong
        .execute(
            &Request::create(&endpoint, Duration::from_secs(90)).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap_err();
    assert!(error.is_authentication());
    assert!(!format!("{error:?}").contains("private-wrong"));
    assert_eq!(fake.active_subscriptions(), 0);
}

#[test]
fn event_discovery_retains_capabilities_and_topics_without_opening_a_subscription() {
    let fake = FakeOnvif::builder().start().unwrap();
    let endpoint = Endpoint::new(format!("{}/onvif/device_service", fake.origin())).unwrap();
    let mut client = Client::new(
        endpoint,
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let service = client.discover(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(service.endpoint().as_str(), fake.events_endpoint());
    assert_eq!(service.max_pull_points(), Some(1));
    assert!(service.kinds().contains(&onvif::event::Kind::Motion));
    assert!(service.kinds().contains(&onvif::event::Kind::Tamper));
    assert!(!service.topic_dialects().is_empty());
    assert_eq!(fake.subscription_count(), 0);
    assert!(service.pull_supported());
}
