use std::time::Duration;

use super::super::{FakeOnvif, notification};
use super::lifecycle::{CREATE, create, pull};
use super::transport::{Client, DEVICE, EVENTS, GET_SERVICES, envelope, texts};
use crate::Reply;

#[test]
fn response_overrides_require_authentication_and_preserve_subscription_state() {
    let message = notification(
        "VideoSource/MotionAlarm",
        true,
        "Changed",
        "2026-09-05T12:00:00Z",
        "source-3",
    );
    let fake = FakeOnvif::builder()
        .notifications(vec![message.clone()])
        .start()
        .unwrap();
    fake.next_response(Reply::http(503, "application/soap+xml", "temporary-fault"))
        .unwrap();
    let mut client = Client::new(&fake);
    assert_eq!(
        client.post(DEVICE, &envelope(GET_SERVICES, "")),
        (503, "temporary-fault".to_owned())
    );
    let (target, header) = create(&mut client);
    let fault = envelope(
        "<s:Fault><s:Code><s:Value>s:Receiver</s:Value></s:Code><s:Reason><s:Text xml:lang=\"en\">scripted-fault</s:Text></s:Reason></s:Fault>",
        "",
    );
    fake.next_pull_response(Reply::http(500, "application/soap+xml", &fault))
        .unwrap();
    fake.next_pull_response(Reply::http(200, "application/soap+xml", "<malformed"))
        .unwrap();
    assert_eq!(client.send(&target, &pull("PT0S", 1, &header), None).0, 401);
    let (status, body) = client.post(&target, &pull("PT0S", 1, ""));
    assert_eq!(status, 500);
    assert!(!body.contains("scripted-fault"));
    assert_eq!(
        client.post(&target, &pull("PT0S", 1, &header)),
        (500, fault)
    );
    assert_eq!(
        client.post(&target, &pull("PT0S", 1, &header)),
        (200, "<malformed".to_owned())
    );
    let (status, body) = client.post(&target, &pull("PT0S", 1, &header));
    assert_eq!(status, 200);
    assert!(body.contains(&message));
    assert_eq!(fake.subscription_count(), 1);
    assert_eq!(fake.active_subscriptions(), 1);
    assert_eq!(fake.pull_count(), 3);
}

#[test]
fn subscription_address_templates_advertise_foreign_and_wildcard_authorities() {
    for host in ["192.0.2.77", "0.0.0.0"] {
        let template = format!("http://{host}:{{port}}/onvif/subscription?key={{id}}&fixture=true");
        let fake = FakeOnvif::builder()
            .subscription_address(template)
            .start()
            .unwrap();
        let mut client = Client::new(&fake);
        let (status, body) = client.post(EVENTS, &envelope(CREATE, ""));
        assert_eq!(status, 200);
        assert_eq!(
            texts(&body, "http://www.w3.org/2005/08/addressing", "Address"),
            [format!(
                "http://{host}:{}/onvif/subscription?key=1&fixture=true",
                fake.address().port()
            )]
        );
        let (status, body) = client.post(DEVICE, &envelope(GET_SERVICES, ""));
        assert_eq!(status, 200);
        assert!(
            texts(&body, "http://www.onvif.org/ver10/device/wsdl", "XAddr")
                .iter()
                .all(|address| address.starts_with(&fake.origin()))
        );
    }
}

#[test]
fn queued_notifications_and_overrides_share_one_capacity_budget() {
    let fake = FakeOnvif::builder()
        .notifications(vec![String::new(); 256])
        .start()
        .unwrap();
    assert!(fake.push("<notification/>").is_err());
    assert!(fake.next_response(Reply::raw(Vec::new())).is_err());
    assert!(fake.next_pull_response(Reply::raw(Vec::new())).is_err());
    let fake = FakeOnvif::builder().start().unwrap();
    fake.next_pull_response(Reply::raw(vec![b'x'; 8 * 1024 * 1024]))
        .unwrap();
    assert!(fake.push("x").is_err());
    assert!(fake.next_response(Reply::raw(vec![b'x'])).is_err());
}

#[test]
fn builder_and_script_limits_fail_before_allocating_server_resources() {
    assert!(FakeOnvif::builder().lease(Duration::ZERO).start().is_err());
    assert!(
        FakeOnvif::builder()
            .lease(Duration::from_secs(86_401))
            .start()
            .is_err()
    );
    assert!(
        FakeOnvif::builder()
            .max_timeout(Duration::ZERO)
            .start()
            .is_err()
    );
    assert!(FakeOnvif::builder().max_message_limit(0).start().is_err());
    assert!(
        FakeOnvif::builder()
            .notifications(vec![String::new(); 257])
            .start()
            .is_err()
    );
    assert!(
        FakeOnvif::builder()
            .subscription_address("file:///fixture")
            .start()
            .is_err()
    );
    assert!(
        FakeOnvif::builder()
            .subscription_address("http://secret:secret@127.0.0.1/x")
            .start()
            .is_err()
    );
    let fake = FakeOnvif::builder().start().unwrap();
    assert!(
        fake.next_pull_response(Reply::raw("x").then(Duration::from_secs(3), "y"))
            .is_err()
    );
    assert!(
        fake.next_response(Reply::raw(vec![b'x'; 8 * 1024 * 1024 + 1]))
            .is_err()
    );
}
