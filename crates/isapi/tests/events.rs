use isapi::Event;

fn notification(fields: &str) -> String {
    format!(
        "<EventNotificationAlert xmlns=\"http://www.isapi.org/ver20/XMLSchema\">{fields}</EventNotificationAlert>"
    )
}

#[test]
fn motion_preserves_explicit_fields_without_inventing_classification() {
    let event = Event::parse(notification("<channelID>1</channelID><dateTime>2026-09-04T21:10:55-5:00</dateTime><activePostCount>2</activePostCount><eventType>VMD</eventType><eventState>active</eventState>")).unwrap();
    assert_eq!(event.event_type(), "VMD");
    assert_eq!(event.active(), Some(true));
    assert_eq!(event.channel_id(), Some(1));
    assert_eq!(event.date_time(), Some("2026-09-04T21:10:55-5:00"));
    assert_eq!(event.active_post_count(), Some(2));
    assert_eq!(event.detection_target(), None);
    assert!(event.is_motion());
}

#[test]
fn prefixed_xml_and_explicit_target_remain_separate_from_motion() {
    let xml = b"<h:EventNotificationAlert xmlns:h=\"http://www.hikvision.com/ver10/XMLSchema\"><h:eventType>linedetection</h:eventType><h:eventState>inactive</h:eventState><h:dynChannelID>2</h:dynChannelID><h:detectionTarget>human</h:detectionTarget><h:channelName>North &amp; gate &#x4EBA;</h:channelName></h:EventNotificationAlert>";
    let event = Event::parse(xml).unwrap();
    assert_eq!(event.active(), Some(false));
    assert_eq!(event.dynamic_channel_id(), Some(2));
    assert_eq!(event.detection_target(), Some("human"));
    assert_eq!(event.channel_name(), Some("North & gate \u{4eba}"));
    assert!(!event.is_motion());
}

#[test]
fn unfamiliar_event_and_state_values_are_preserved() {
    let event = Event::parse(notification("<eventType>newVendorRule</eventType><eventState>pulse</eventState><Extension><eventType>not-the-event</eventType></Extension>")).unwrap();
    assert_eq!(event.event_type(), "newVendorRule");
    assert_eq!(event.state(), "pulse");
    assert_eq!(event.active(), None);
}

#[test]
fn malformed_and_ambiguous_xml_is_rejected_without_echoing_payload() {
    for xml in [
        notification("<eventType>VMD</eventType><eventType>other</eventType><eventState>active</eventState>"),
        notification("<eventType>VMD<Child/></eventType><eventState>active</eventState>"),
        notification("<eventType>VMD</eventType>"),
        notification("<eventType>VMD</eventType><eventState>active</eventState><channelID>-1</channelID>"),
        format!("<!DOCTYPE x [<!ENTITY secret SYSTEM \"file:///private/example\">]>{}", notification("<eventType>&secret;</eventType><eventState>active</eventState>")),
        format!("{}<SecondRoot/>", notification("<eventType>VMD</eventType><eventState>active</eventState>")),
        notification("<eventType test=\"1\" test=\"2\">VMD</eventType><eventState>active</eventState>"),
        "<EventNotificationAlert xmlns=\"urn:unrecognized\"><eventType>VMD</eventType><eventState>active</eventState></EventNotificationAlert>".to_owned(),
    ] {
        let error = Event::parse(&xml).unwrap_err();
        assert!(!format!("{error:?}").contains(&xml));
    }
}

#[test]
fn xml_limits_and_encoding_fail_closed() {
    assert!(
        Event::parse(vec![b' '; 256 * 1024 + 1])
            .unwrap_err()
            .is_limit()
    );
    let deep = format!("{}{}", "<nested>".repeat(33), "</nested>".repeat(33));
    assert!(Event::parse(notification(&deep)).is_err());
    let unsupported = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-7\"?>{}",
        notification("<eventType>VMD</eventType><eventState>active</eventState>")
    );
    assert!(Event::parse(unsupported).is_err());
    assert!(Event::parse([0xff, 0xfe, 0x00]).is_err());
}

#[test]
fn documented_heartbeat_namespace_and_states_are_recognized() {
    for (event_type, state) in [("videoloss", "inactive"), ("heartBeat", "active")] {
        let xml = format!(
            "<EventNotificationAlert xmlns=\"http://www.isapi.com/ver20/XMLSchema\"><eventType>{event_type}</eventType><eventState>{state}</eventState></EventNotificationAlert>"
        );
        assert!(Event::parse(xml).unwrap().is_heartbeat());
    }
    let alarm = Event::parse(notification(
        "<eventType>videoloss</eventType><eventState>active</eventState>",
    ))
    .unwrap();
    assert!(!alarm.is_heartbeat());
}

#[test]
fn json_events_accept_documented_numeric_or_string_channels_and_explicit_targets() {
    let plain = br#"{"eventType":"VMD","eventState":"active","channelID":"2","activePostCount":3,"detectionTarget":"vehicle","dateTime":"2026-09-04T12:00:00Z"}"#;
    let event = Event::parse_json(plain).unwrap();
    assert_eq!(event.channel_id(), Some(2));
    assert_eq!(event.active_post_count(), Some(3));
    assert_eq!(event.detection_target(), Some("vehicle"));
    let wrapped =
        br#"{"EventNotificationAlert":{"eventType":"VMD","eventState":"inactive","channelID":1}}"#;
    assert_eq!(Event::parse_json(wrapped).unwrap().active(), Some(false));
    for invalid in [
        br#"{"eventType":"VMD","eventType":"IO","eventState":"active"}"#.as_slice(),
        br#"{"eventType":"VMD","eventState":"active","channelID":-1}"#,
        br#"{"eventType":"VMD","eventState":"active","channelID":1.5}"#,
    ] {
        assert!(Event::parse_json(invalid).is_err());
    }
}

#[test]
fn overwritten_json_fields_cannot_hide_excessive_structure() {
    let deep = format!("{}0{}", "[".repeat(33), "]".repeat(33));
    let bytes = format!(
        "{{\"eventType\":\"VMD\",\"eventState\":\"active\",\"extension\":{deep},\"extension\":0}}"
    );
    assert!(Event::parse_json(bytes).is_err());
    let duplicate = br#"{"eventType":"VMD","eventState":"active","extension":{"mode":1,"mode":2}}"#;
    assert!(Event::parse_json(duplicate).is_err());
}
