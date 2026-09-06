use isapi::{CallbackAuth, Credentials, Format, Method, Request, Session};
use std::time::Duration;

fn request() -> Request {
    Request::with_body(
        Method::Post,
        "/ISAPI/Event/notification/callback",
        Format::Xml,
        b"<event/>",
    )
    .unwrap()
}

#[test]
fn callback_digest_binds_method_uri_identity_nonce_and_monotonic_count() {
    let credentials = || Credentials::new("camera", "test-only-password");
    let mut receiver = CallbackAuth::new(credentials()).unwrap();
    let challenge = receiver
        .challenge("0123456789abcdef0123456789abcdef", Duration::ZERO)
        .unwrap();
    let mut sender = Session::new(credentials()).unwrap();
    sender.handle_challenge(&challenge).unwrap();
    let authorization = sender
        .authorization(&request(), "camera-client-nonce")
        .unwrap()
        .unwrap();
    receiver
        .verify(
            authorization.as_str(),
            "POST",
            request().resource(),
            Duration::from_secs(1),
        )
        .unwrap();
    assert!(
        receiver
            .verify(
                authorization.as_str(),
                "POST",
                request().resource(),
                Duration::from_secs(1)
            )
            .is_err()
    );
    let second = sender
        .authorization(&request(), "new-client-nonce")
        .unwrap()
        .unwrap();
    assert!(
        receiver
            .verify(
                second.as_str(),
                "GET",
                request().resource(),
                Duration::from_secs(1)
            )
            .is_err()
    );
    receiver
        .verify(
            second.as_str(),
            "POST",
            request().resource(),
            Duration::from_secs(1),
        )
        .unwrap();
    assert!(!format!("{receiver:?}").contains("test-only-password"));
}

#[test]
fn callbacks_reject_wrong_secrets_expired_nonces_basic_and_missing_qop() {
    let mut receiver = CallbackAuth::new(Credentials::new("camera", "correct-password")).unwrap();
    let challenge = receiver
        .challenge("0123456789abcdef0123456789abcdef", Duration::ZERO)
        .unwrap();
    let mut sender = Session::new(Credentials::new("camera", "wrong-password")).unwrap();
    sender.handle_challenge(&challenge).unwrap();
    let authorization = sender.authorization(&request(), "nonce").unwrap().unwrap();
    assert!(
        receiver
            .verify(
                authorization.as_str(),
                "POST",
                request().resource(),
                Duration::ZERO
            )
            .is_err()
    );
    assert!(
        receiver
            .verify(
                "Basic dGVzdDp0ZXN0",
                "POST",
                request().resource(),
                Duration::ZERO
            )
            .is_err()
    );
    let mut sender = Session::new(Credentials::new("camera", "correct-password")).unwrap();
    sender.handle_challenge(&challenge).unwrap();
    let authorization = sender.authorization(&request(), "nonce").unwrap().unwrap();
    assert!(
        receiver
            .verify(
                authorization.as_str(),
                "POST",
                request().resource(),
                Duration::from_secs(61)
            )
            .is_err()
    );
}
