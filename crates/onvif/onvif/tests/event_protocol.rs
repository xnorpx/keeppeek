use std::time::Duration;

use onvif::event::{Endpoint, Lease, Operation, Pull, Subscription};

const SUBSCRIPTION: &str = r#"<e:CreatePullPointSubscriptionResponse xmlns:e="http://www.onvif.org/ver10/events/wsdl" xmlns:a="http://www.w3.org/2005/08/addressing" xmlns:b="http://docs.oasis-open.org/wsn/b-2" xmlns:v="urn:camera"><e:SubscriptionReference><a:Address>http://0.0.0.0/subscription?id=7</a:Address><a:ReferenceParameters><v:Identifier v:mode="exact">opaque&amp;secret</v:Identifier></a:ReferenceParameters></e:SubscriptionReference><b:CurrentTime>2020-01-01T00:00:00Z</b:CurrentTime><b:TerminationTime>2020-01-01T00:01:30Z</b:TerminationTime></e:CreatePullPointSubscriptionResponse>"#;

#[test]
fn subscription_preserves_reference_parameters_and_uses_camera_relative_lease() {
    let endpoint = Endpoint::new("http://192.0.2.20:8080/events").unwrap();
    let subscription = Subscription::parse(&endpoint, SUBSCRIPTION.as_bytes()).unwrap();
    assert_eq!(
        subscription.endpoint().as_str(),
        "http://192.0.2.20:8080/subscription?id=7"
    );
    assert_eq!(subscription.lease().remaining(), Duration::from_secs(90));
    assert!(subscription.lease().renew_after() < Duration::from_secs(90));
    for operation in [
        Operation::Synchronize,
        Operation::Pull {
            timeout: Duration::from_secs(2),
            limit: 32,
        },
        Operation::Renew {
            lifetime: Duration::from_secs(90),
        },
        Operation::Unsubscribe,
    ] {
        let request = subscription.request(operation).unwrap();
        let envelope = request.envelope(None, "urn:uuid:test-request").unwrap();
        let root = xmltree::Element::parse(envelope.as_bytes()).unwrap();
        let header = root.get_child("Header").unwrap();
        let identifier = header.get_child("Identifier").unwrap();
        assert_eq!(identifier.namespace.as_deref(), Some("urn:camera"));
        assert_eq!(identifier.get_text().as_deref(), Some("opaque&secret"));
        assert!(envelope.contains("IsReferenceParameter=\"true\""));
        assert!(!format!("{request:?}").contains("secret"));
        assert!(!format!("{subscription:?}").contains("secret"));
    }
}

#[test]
fn bad_notification_does_not_hide_valid_neighbors_and_pull_updates_the_lease() {
    let xml = r#"<e:PullMessagesResponse xmlns:e="http://www.onvif.org/ver10/events/wsdl" xmlns:n="http://docs.oasis-open.org/wsn/b-2" xmlns:t="http://www.onvif.org/ver10/schema" xmlns:q="http://www.onvif.org/ver10/topics"><e:CurrentTime>2020-01-01T00:00:10Z</e:CurrentTime><e:TerminationTime>2020-01-01T00:01:40Z</e:TerminationTime><n:NotificationMessage><n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">q:VideoSource/MotionAlarm</n:Topic><n:Message><t:Message UtcTime="bad"><t:Data/></t:Message></n:Message></n:NotificationMessage><n:NotificationMessage><n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">q:VideoSource/MotionAlarm</n:Topic><n:Message><t:Message UtcTime="2020-01-01T00:00:10Z" PropertyOperation="Changed"><t:Data><t:SimpleItem Name="State" Value="true"/></t:Data></t:Message></n:Message></n:NotificationMessage></e:PullMessagesResponse>"#;
    let pull = Pull::parse(xml.as_bytes()).unwrap();
    assert_eq!(pull.notifications.len(), 1);
    assert_eq!(pull.invalid_messages, 1);
    assert_eq!(pull.lease.remaining(), Duration::from_secs(90));
    assert_eq!(
        pull.notifications[0].topic.path[0].namespace_uri.as_deref(),
        Some("http://www.onvif.org/ver10/topics")
    );
}

#[test]
fn protocol_rejects_malformed_leases_and_forbidden_reference_headers() {
    let endpoint = Endpoint::new("http://192.0.2.20/events").unwrap();
    assert!(
        Subscription::parse(
            &endpoint,
            SUBSCRIPTION.replace("00:01:30", "00:00:00").as_bytes()
        )
        .is_err()
    );
    assert!(
        Subscription::parse(
            &endpoint,
            SUBSCRIPTION
                .replace(
                    "<v:Identifier v:mode=\"exact\">opaque&amp;secret</v:Identifier>",
                    "<a:To>http://example.org</a:To>"
                )
                .as_bytes()
        )
        .is_err()
    );
    assert!(Lease::parse(br#"<b:RenewResponse xmlns:b="http://docs.oasis-open.org/wsn/b-2"><b:CurrentTime>bad</b:CurrentTime><b:TerminationTime>bad</b:TerminationTime></b:RenewResponse>"#).is_err());
}
