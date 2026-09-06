use isapi::{Credentials, Method, Ptz, Request, Session};

#[test]
fn ptz_request_preserves_upstream_channel_and_numeric_xml_semantics() {
    let movement = Ptz::new(-25, 40, 10).unwrap();
    let request = movement.momentary(2, 500).unwrap();
    assert_eq!(request.method(), Method::Put);
    assert_eq!(request.resource(), "/ISAPI/PTZCtrl/channels/2/Momentary");
    assert_eq!(request.body(), b"<PTZData><pan>-25</pan><tilt>40</tilt><zoom>10</zoom><Momentary><duration>500</duration></Momentary></PTZData>");
    assert!(Ptz::new(101, 0, 0).unwrap_err().is_invalid_input());
    assert!(movement.momentary(0, 500).unwrap_err().is_invalid_input());
    assert!(movement.momentary(1, 0).unwrap_err().is_invalid_input());
}

#[test]
fn digest_state_uses_supplied_entropy_and_advances_nonce_count() {
    let request = Request::get("/ISAPI/System/deviceInfo?channel=1").unwrap();
    let mut session = Session::new(Credentials::new("operator", "example-secret")).unwrap();
    assert!(
        session
            .authorization(&request, "test-cnonce")
            .unwrap()
            .is_none()
    );
    session
        .handle_challenge(
            "Digest realm=\"camera\", nonce=\"server-nonce\", qop=\"auth\", algorithm=SHA-256",
        )
        .unwrap();
    let first = session
        .authorization(&request, "test-cnonce")
        .unwrap()
        .unwrap();
    let second = session
        .authorization(&request, "test-cnonce")
        .unwrap()
        .unwrap();
    assert!(first.as_str().contains("cnonce=\"test-cnonce\""));
    assert!(
        first
            .as_str()
            .contains("uri=\"/ISAPI/System/deviceInfo?channel=1\"")
    );
    assert!(first.as_str().contains("nc=00000001"));
    assert!(second.as_str().contains("nc=00000002"));
    assert_ne!(first.as_str(), second.as_str());
    for output in [format!("{first:?}"), format!("{session:?}")] {
        assert!(!output.contains("operator"));
        assert!(!output.contains("example-secret"));
        assert!(!output.contains("server-nonce"));
    }
}

#[test]
fn request_owns_passthrough_bytes_and_rejects_unbounded_payloads() {
    let mut input = b"<MotionDetection><enabled>true</enabled></MotionDetection>".to_vec();
    let request = Request::put(
        "/ISAPI/System/Video/inputs/channels/1/motionDetection",
        &input,
    )
    .unwrap();
    input.fill(0);
    assert!(request.body().starts_with(b"<MotionDetection>"));
    assert!(
        Request::put("/ISAPI/test", vec![0; 256 * 1024 + 1])
            .unwrap_err()
            .is_limit()
    );
}

#[test]
fn authentication_rejects_basic_and_header_injection_without_echoing_input() {
    let mut session = Session::new(Credentials::new("operator", "example-secret")).unwrap();
    for challenge in [
        "Basic realm=\"camera\"",
        "Digest realm=\"camera\r\nsecret\", nonce=\"nonce\"",
    ] {
        let error = session.handle_challenge(challenge).unwrap_err();
        assert!(error.is_authentication());
        assert!(!format!("{error:?}").contains(challenge));
    }
}
