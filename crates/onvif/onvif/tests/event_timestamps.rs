use chrono::{DateTime, Utc};
use onvif::event::{
    Metadata, NotificationParseError, Pull, TimestampReason, TimestampSource, parse_notifications,
    parse_notifications_at,
};

fn received_time() -> DateTime<Utc> {
    "2026-09-05T12:00:05.123Z".parse().unwrap()
}

fn notification_xml(attributes: &str) -> String {
    format!(
        r#"<n:NotificationMessage xmlns:n="http://docs.oasis-open.org/wsn/b-2"
            xmlns:t="http://www.onvif.org/ver10/schema"
            xmlns:q="http://www.onvif.org/ver10/topics" xmlns:v="urn:other">
            <n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">q:VideoSource/MotionAlarm</n:Topic>
            <n:Message><t:Message {attributes} PropertyOperation="Changed">
                <t:Data><t:SimpleItem Name="State" Value="true"/></t:Data>
            </t:Message></n:Message>
        </n:NotificationMessage>"#
    )
}

#[test]
fn strict_notifications_still_require_a_valid_timestamp() {
    assert!(matches!(
        parse_notifications(notification_xml("").as_bytes()),
        Err(NotificationParseError::Missing("UtcTime"))
    ));
    assert!(matches!(
        parse_notifications(notification_xml(r#"UtcTime="invalid""#).as_bytes()),
        Err(NotificationParseError::UtcTime(_))
    ));
}

#[test]
fn missing_notification_time_uses_the_explicit_receipt() {
    let notifications =
        parse_notifications_at(notification_xml("").as_bytes(), received_time()).unwrap();
    assert_eq!(notifications.len(), 1);
    let notification = &notifications[0];
    assert_eq!(notification.utc_time, received_time());
    assert_eq!(
        notification.timestamp_source,
        TimestampSource::Received {
            reason: TimestampReason::Missing,
        }
    );
    assert_eq!(notification.data.simple[0].value, "true");
}

#[test]
fn invalid_notification_time_uses_receipt_without_retaining_raw_input() {
    for invalid in [
        "",
        "private-invalid-camera-time",
        "2026-02-30T12:00:00Z",
        "2026-09-05T12:00:00",
    ] {
        let xml = notification_xml(&format!(r#"UtcTime="{invalid}""#));
        let notifications = parse_notifications_at(xml.as_bytes(), received_time()).unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].utc_time, received_time());
        assert_eq!(
            notifications[0].timestamp_source,
            TimestampSource::Received {
                reason: TimestampReason::Invalid,
            }
        );
        assert!(!format!("{:?}", notifications[0]).contains("private-invalid-camera-time"));
    }
}

#[test]
fn valid_notification_times_keep_camera_provenance_and_offset_normalization() {
    let camera_time: DateTime<Utc> = "2026-09-05T12:00:00.456Z".parse().unwrap();
    for timestamp in ["2026-09-05T12:00:00.456Z", "2026-09-05T14:00:00.456+02:00"] {
        let xml = notification_xml(&format!(r#"UtcTime="{timestamp}""#));
        let strict = parse_notifications(xml.as_bytes()).unwrap();
        let tolerant = parse_notifications_at(xml.as_bytes(), received_time()).unwrap();
        assert_eq!(tolerant, strict);
        assert_eq!(tolerant.len(), 1);
        assert_eq!(tolerant[0].utc_time, camera_time);
        assert_eq!(tolerant[0].timestamp_source, TimestampSource::Camera);
    }
}

#[test]
fn qualified_notification_time_is_not_a_missing_unqualified_time() {
    for attributes in [
        r#"v:UtcTime="2026-09-05T12:00:00Z""#,
        r#"t:UtcTime="2026-09-05T12:00:00Z""#,
        r#"v:UtcTime="invalid""#,
    ] {
        let xml = notification_xml(attributes);
        assert!(parse_notifications(xml.as_bytes()).is_err());
        assert!(parse_notifications_at(xml.as_bytes(), received_time()).is_err());
    }
}

fn pull_xml(notifications: &str) -> String {
    format!(
        r#"<e:PullMessagesResponse xmlns:e="http://www.onvif.org/ver10/events/wsdl">
            <e:CurrentTime>2026-09-05T12:00:00Z</e:CurrentTime>
            <e:TerminationTime>2026-09-05T12:01:30Z</e:TerminationTime>
            {notifications}
        </e:PullMessagesResponse>"#
    )
}

fn metadata_xml(notifications: &str) -> String {
    format!(
        r#"<t:MetadataStream xmlns:t="http://www.onvif.org/ver10/schema">
            <t:Event>{notifications}</t:Event>
        </t:MetadataStream>"#
    )
}

fn mixed_notifications() -> String {
    [
        notification_xml(r#"UtcTime="2026-09-05T12:00:00Z""#),
        notification_xml(""),
        notification_xml(r#"UtcTime="private-invalid-camera-time""#),
        notification_xml(r#"v:UtcTime="2026-09-05T12:00:00Z""#),
        notification_xml("").replace("Value=", "v:Value="),
        notification_xml(r#"UtcTime="2026-09-05T14:00:01+02:00""#),
    ]
    .concat()
}

#[test]
fn strict_wrappers_still_count_missing_and_invalid_timestamps() {
    let contents = mixed_notifications();
    let pull = Pull::parse(pull_xml(&contents).as_bytes()).unwrap();
    let metadata = Metadata::parse(metadata_xml(&contents).as_bytes()).unwrap();
    assert_eq!(pull.invalid_messages, 4);
    assert_eq!(metadata.invalid_messages, 4);
    assert_eq!(pull.notifications, metadata.notifications);
    assert_eq!(pull.notifications.len(), 2);
    assert!(
        pull.notifications
            .iter()
            .all(|message| message.timestamp_source == TimestampSource::Camera)
    );
}

#[test]
fn receipt_aware_wrappers_keep_fallbacks_and_valid_neighbors_in_order() {
    let contents = mixed_notifications();
    let pull = Pull::parse_at(pull_xml(&contents).as_bytes(), received_time()).unwrap();
    let metadata = Metadata::parse_at(metadata_xml(&contents).as_bytes(), received_time()).unwrap();
    assert_eq!(pull.invalid_messages, 2);
    assert_eq!(metadata.invalid_messages, 2);
    assert_eq!(pull.notifications, metadata.notifications);
    assert_eq!(pull.notifications.len(), 4);
    assert_eq!(pull.lease.remaining(), std::time::Duration::from_secs(90));
    assert_eq!(
        pull.notifications
            .iter()
            .map(|message| message.timestamp_source)
            .collect::<Vec<_>>(),
        [
            TimestampSource::Camera,
            TimestampSource::Received {
                reason: TimestampReason::Missing,
            },
            TimestampSource::Received {
                reason: TimestampReason::Invalid,
            },
            TimestampSource::Camera,
        ]
    );
    assert_eq!(pull.notifications[1].utc_time, received_time());
    assert_eq!(pull.notifications[2].utc_time, received_time());
    assert_eq!(
        pull.notifications[0].utc_time.timestamp_millis(),
        1_788_609_600_000
    );
    assert_eq!(
        pull.notifications[3].utc_time.timestamp_millis(),
        1_788_609_601_000
    );
}

#[test]
fn receipt_aware_pull_does_not_relax_lease_timestamps() {
    let xml = pull_xml(&notification_xml(""));
    for field in ["CurrentTime", "TerminationTime"] {
        let invalid = xml.replace(&format!("<e:{field}>"), &format!("<e:{field}>invalid"));
        assert!(Pull::parse_at(invalid.as_bytes(), received_time()).is_err());
    }
}

#[test]
fn receipt_aware_parsing_keeps_structural_xml_failures() {
    for contents in [
        notification_xml(r#"UtcTime="invalid" UtcTime="invalid""#),
        notification_xml("").replace("</t:Data>", "</t:Wrong>"),
    ] {
        assert!(parse_notifications_at(contents.as_bytes(), received_time()).is_err());
        assert!(Pull::parse_at(pull_xml(&contents).as_bytes(), received_time()).is_err());
        assert!(Metadata::parse_at(metadata_xml(&contents).as_bytes(), received_time()).is_err());
    }
    let oversized = vec![b' '; onvif::event::NOTIFICATION_XML_SIZE_BYTES_MAX + 1];
    assert!(matches!(
        parse_notifications_at(&oversized, received_time()),
        Err(NotificationParseError::PayloadTooLarge { .. })
    ));
    let deep = format!("{}{}", "<node>".repeat(33), "</node>".repeat(33));
    assert!(matches!(
        parse_notifications_at(deep.as_bytes(), received_time()),
        Err(NotificationParseError::DepthExceeded { .. })
    ));
    assert!(Pull::parse_at(pull_xml(&deep).as_bytes(), received_time()).is_err());
    assert!(Metadata::parse_at(metadata_xml(&deep).as_bytes(), received_time()).is_err());
}

#[test]
fn timestamp_fallbacks_still_count_toward_notification_limits() {
    for count in [256, 257] {
        let messages = notification_xml("").repeat(count);
        let batch = format!("<root>{messages}</root>");
        let parsed = parse_notifications_at(batch.as_bytes(), received_time());
        let pull = Pull::parse_at(pull_xml(&messages).as_bytes(), received_time());
        let metadata = Metadata::parse_at(metadata_xml(&messages).as_bytes(), received_time());
        if count == 256 {
            assert_eq!(parsed.unwrap().len(), count);
            let pull = pull.unwrap();
            assert_eq!(pull.notifications.len(), count);
            assert_eq!(pull.invalid_messages, 0);
            let metadata = metadata.unwrap();
            assert_eq!(metadata.notifications.len(), count);
            assert_eq!(metadata.invalid_messages, 0);
        } else {
            assert!(matches!(
                parsed,
                Err(NotificationParseError::CountExceeded { .. })
            ));
            assert!(pull.is_err());
            assert!(metadata.is_err());
        }
    }
}
