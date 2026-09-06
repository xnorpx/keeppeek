use std::time::Duration;

use xml::reader::{EventReader, XmlEvent};

use super::super::{FakeOnvif, notification};
use super::transport::{Client, EVENTS, envelope, texts};

pub(super) const EVENTS_NS: &str = "http://www.onvif.org/ver10/events/wsdl";
pub(super) const WSNT: &str = "http://docs.oasis-open.org/wsn/b-2";
pub(super) const CREATE: &str = "<tev:CreatePullPointSubscription><tev:InitialTerminationTime>PT90S</tev:InitialTerminationTime></tev:CreatePullPointSubscription>";

pub(super) fn create(client: &mut Client) -> (String, String) {
    let (status, body) = client.post(EVENTS, &envelope(CREATE, ""));
    assert_eq!(status, 200, "{body}");
    assert_root(&body, EVENTS_NS, "CreatePullPointSubscriptionResponse");
    assert_eq!(texts(&body, WSNT, "CurrentTime").len(), 1);
    assert_eq!(texts(&body, WSNT, "TerminationTime").len(), 1);
    let address = texts(&body, "http://www.w3.org/2005/08/addressing", "Address");
    let address = url::Url::parse(&address[0]).unwrap();
    let identifier = texts(&body, "urn:test-hikvision:onvif", "Identifier");
    assert_eq!(identifier.len(), 1);
    let header = format!(
        r#"<v:Identifier xmlns:v="urn:test-hikvision:onvif">{}</v:Identifier>"#,
        identifier[0]
    );
    (
        format!("{}?{}", address.path(), address.query().unwrap()),
        header,
    )
}

pub(super) fn pull(timeout: &str, limit: u32, header: &str) -> String {
    envelope(
        &format!(
            "<tev:PullMessages><tev:Timeout>{timeout}</tev:Timeout><tev:MessageLimit>{limit}</tev:MessageLimit></tev:PullMessages>"
        ),
        header,
    )
}

pub(super) fn assert_root(body: &str, namespace: &str, local_name: &str) {
    let mut depth = 0;
    let mut roots = Vec::new();
    for event in EventReader::from_str(body) {
        match event.unwrap() {
            XmlEvent::StartElement { name, .. } => {
                if depth == 2 {
                    roots.push(name);
                }
                depth += 1;
            }
            XmlEvent::EndElement { .. } => depth -= 1,
            _ => {}
        }
    }
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].namespace.as_deref(), Some(namespace));
    assert_eq!(roots[0].local_name, local_name);
}

#[test]
fn subscription_lifecycle_is_stateful_and_preserves_protocol_roots() {
    let first = notification(
        "VideoSource/MotionAlarm",
        true,
        "Changed",
        "2026-09-05T12:00:00Z",
        "source-1",
    );
    let second = notification(
        "VideoSource/Tamper",
        false,
        "Initialized",
        "2026-09-05T12:00:01Z",
        "source-1",
    );
    let fake = FakeOnvif::builder()
        .notifications(vec![first.clone(), second.clone()])
        .start()
        .unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    assert_eq!(target, "/onvif/subscription?key=1");
    assert_eq!(fake.active_subscriptions(), 1);
    assert_eq!(fake.subscription_count(), 1);
    assert_eq!(client.post(EVENTS, &envelope(CREATE, "")).0, 500);
    assert_eq!(fake.subscription_count(), 1);
    let (status, body) = client.post(
        &target,
        &envelope("<tev:SetSynchronizationPoint/>", &header),
    );
    assert_eq!(status, 200);
    assert_root(&body, EVENTS_NS, "SetSynchronizationPointResponse");
    let (status, body) = client.post(&target, &pull("PT0S", 1, &header));
    assert_eq!(status, 200);
    assert_root(&body, EVENTS_NS, "PullMessagesResponse");
    assert_eq!(texts(&body, EVENTS_NS, "CurrentTime").len(), 1);
    assert_eq!(texts(&body, EVENTS_NS, "TerminationTime").len(), 1);
    assert!(body.contains(&first));
    assert!(!body.contains(&second));
    let (status, body) = client.post(&target, &pull("PT0S", 256, &header));
    assert_eq!(status, 200);
    assert!(body.contains(&second));
    assert!(!body.contains(&first));
    assert!(fake.wait_for_pulls(2, Duration::ZERO));
    let (status, body) = client.post(
        &target,
        &envelope(
            "<wsnt:Renew><wsnt:TerminationTime>PT90S</wsnt:TerminationTime></wsnt:Renew>",
            &header,
        ),
    );
    assert_eq!(status, 200);
    assert_root(&body, WSNT, "RenewResponse");
    assert_eq!(fake.renew_count(), 1);
    let (status, body) = client.post(&target, &envelope("<wsnt:Unsubscribe/>", &header));
    assert_eq!(status, 200);
    assert_root(&body, WSNT, "UnsubscribeResponse");
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
    assert_eq!(client.post(&target, &pull("PT0S", 1, &header)).0, 500);
}

#[test]
fn subscription_reference_is_a_header_element_not_a_substring() {
    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    let wrong_headers = [
        String::new(),
        header.replace("urn:test-hikvision:onvif", "urn:wrong"),
        header.replace(">1<", ">2<"),
        format!("<wrapper>{header}</wrapper>"),
        format!("{header}{header}"),
    ];
    for wrong in wrong_headers {
        for operation in [
            "<tev:SetSynchronizationPoint/>",
            "<wsnt:Unsubscribe/>",
            "<wsnt:Renew><wsnt:TerminationTime>PT90S</wsnt:TerminationTime></wsnt:Renew>",
        ] {
            assert_eq!(client.post(&target, &envelope(operation, &wrong)).0, 500);
        }
        assert_eq!(client.post(&target, &pull("PT0S", 1, &wrong)).0, 500);
    }
    assert_eq!(fake.active_subscriptions(), 1);
    assert_eq!(fake.renew_count(), 0);
    assert_eq!(fake.unsubscribe_count(), 0);
    assert!(!fake.wait_for_pulls(1, Duration::ZERO));
    let (status, _) = client.post(&target, &pull("PT0S", 1, &header));
    assert_eq!(status, 200);
}

#[test]
fn short_lease_expires_during_pull_and_allows_a_new_subscription() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_millis(250))
        .start()
        .unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    let (status, body) = client.post(&target, &pull("PT1S", 1, &header));
    assert_eq!(status, 500);
    assert!(body.contains("ResourceUnknown"));
    assert_eq!(fake.active_subscriptions(), 0);
    let (next_target, next_header) = create(&mut client);
    assert_ne!(target, next_target);
    assert_ne!(header, next_header);
    assert_eq!(fake.subscription_count(), 2);
    assert_eq!(client.post(&target, &pull("PT0S", 1, &header)).0, 500);
}

#[test]
fn lower_pull_limits_and_invalid_parameters_do_not_consume_notifications() {
    let message = notification(
        "RuleEngine/ObjectDetector/Person",
        true,
        "Changed",
        "2026-09-05T12:00:00Z",
        "source-2",
    );
    let fake = FakeOnvif::builder()
        .max_timeout(Duration::from_millis(50))
        .max_message_limit(1)
        .notifications(vec![message.clone()])
        .start()
        .unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    for (timeout, limit) in [
        ("PT1S", 1),
        ("PT0S", 2),
        ("PT-1S", 1),
        ("invalid", 1),
        ("PT0S", 0),
    ] {
        let (status, body) = client.post(&target, &pull(timeout, limit, &header));
        assert_eq!(status, 500);
        assert_eq!(texts(&body, EVENTS_NS, "MaxMessageLimit"), ["1"]);
        assert_eq!(texts(&body, EVENTS_NS, "MaxTimeout"), ["PT0.050000000S"]);
        assert!(!body.contains(&message));
    }
    let (status, body) = client.post(&target, &pull("PT0S", 1, &header));
    assert_eq!(status, 200);
    assert!(body.contains(&message));
}
