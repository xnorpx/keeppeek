use onvif::event::{
    Kind, Notification, PropertyOperation, SimpleItem, normalize, parse_notifications,
};

const MOTION: &str = r#"
<n:NotificationMessage xmlns:n="http://docs.oasis-open.org/wsn/b-2"
    xmlns:t="http://www.onvif.org/ver10/schema"
    xmlns:q="http://www.onvif.org/ver10/topics">
  <n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">
    q:RuleEngine/CellMotionDetector/Motion
  </n:Topic>
  <n:Message><t:Message UtcTime="2026-09-05T12:00:00Z" PropertyOperation="Changed">
    <t:Source>
      <t:SimpleItem Name="VideoSourceConfigurationToken" Value="camera-1"/>
      <t:SimpleItem Name="Rule" Value="zone-1"/>
    </t:Source>
    <t:Key><t:SimpleItem Name="ObjectId" Value="17"/></t:Key>
    <t:Data><t:SimpleItem Name="IsMotion" Value="false"/></t:Data>
  </t:Message></n:Message>
</n:NotificationMessage>"#;

fn parse(xml: &str) -> Notification {
    let mut notifications = parse_notifications(xml.as_bytes()).unwrap();
    assert_eq!(notifications.len(), 1);
    notifications.remove(0)
}

#[test]
fn independent_prefixes_and_item_order_preserve_inactive_detection() {
    let equivalent = r#"
    <ws:NotificationMessage xmlns:ws="http://docs.oasis-open.org/wsn/b-2"
        xmlns:on="http://www.onvif.org/ver10/schema"
        xmlns:topic="http://www.onvif.org/ver10/topics">
      <ws:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">
        topic:RuleEngine/topic:CellMotionDetector/topic:Motion
      </ws:Topic>
      <ws:Message><on:Message PropertyOperation="Changed" UtcTime="2026-09-05T14:00:00+02:00">
        <on:Source>
          <on:SimpleItem Value="zone-1" Name="Rule"/>
          <on:SimpleItem Value="camera-1" Name="VideoSourceConfigurationToken"/>
        </on:Source>
        <on:Key><on:SimpleItem Value="17" Name="ObjectId"/></on:Key>
        <on:Data><on:SimpleItem Value="0" Name="IsMotion"/></on:Data>
      </on:Message></ws:Message>
    </ws:NotificationMessage>"#;
    let original = normalize(&parse(MOTION)).unwrap().unwrap();
    let renamed = normalize(&parse(equivalent)).unwrap().unwrap();
    assert_eq!(original, renamed);
    assert_eq!(original.kind, Kind::Motion);
    assert_eq!(original.kind.as_str(), "motion");
    assert_eq!(original.active, Some(false));
    assert_eq!(original.source.as_deref(), Some("camera-1"));
    assert_eq!(original.rule.as_deref(), Some("zone-1"));
    assert_eq!(original.operation, Some(PropertyOperation::Changed));
    assert_eq!(original.utc_time.to_rfc3339(), "2026-09-05T12:00:00+00:00");
    assert!(!original.identity.is_empty());
    assert!(original.identity.len() <= 4096);
}

#[test]
fn unrelated_namespaces_do_not_become_motion() {
    let mut notification = parse(MOTION);
    notification.topic.path[0].namespace_uri = Some("urn:unrelated".to_owned());
    assert_eq!(normalize(&notification).unwrap(), None);
}

#[test]
fn missing_or_unknown_property_state_does_not_open_motion() {
    let mut notification = parse(MOTION);
    notification.data.simple[0].value = "unknown".to_owned();
    assert_eq!(normalize(&notification).unwrap(), None);
    notification.data.simple.clear();
    assert_eq!(normalize(&notification).unwrap(), None);
}

fn notification(topic: &str, data: &str) -> Notification {
    parse(&format!(
        r#"<n:NotificationMessage xmlns:n="http://docs.oasis-open.org/wsn/b-2"
      xmlns:t="http://www.onvif.org/ver10/schema"
      xmlns:q="http://www.onvif.org/ver10/topics" xmlns:v="urn:unrelated">
      <n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">
      q:{topic}
      </n:Topic>
      <n:Message><t:Message UtcTime="2026-09-05T12:00:00Z">
      <t:Source>
        <t:SimpleItem Name="VideoSource" Value="video-1"/>
        <t:SimpleItem Name="Rule" Value="rule-1"/>
      </t:Source>
      <t:Data>{data}</t:Data>
      </t:Message></n:Message>
    </n:NotificationMessage>"#
    ))
}

#[test]
fn known_property_topics_preserve_their_specific_kind_and_false_state() {
    let cases = [
        ("RuleEngine/CellMotionDetector/Motion", "IsMotion", "motion"),
        ("RuleEngine/MotionRegionDetector/Motion", "State", "motion"),
        ("VideoSource/MotionAlarm", "State", "motion"),
        ("VideoSource/Tamper", "IsTamper", "tamper"),
        (
            "Device/Trigger/DigitalInput",
            "LogicalState",
            "digital_input",
        ),
        (
            "AudioSource/AudioDetection",
            "IsSoundDetected",
            "audio_detected",
        ),
        ("VideoSource/SignalLoss", "State", "video_loss"),
        (
            "RuleEngine/FieldDetector/ObjectsInside",
            "IsInside",
            "intrusion",
        ),
        ("RuleEngine/FieldDetector/Loitering", "State", "loitering"),
        (
            "RuleEngine/LoiteringDetector/ObjectIsLoitering",
            "State",
            "loitering",
        ),
        ("RuleEngine/ObjectDetector/Person", "State", "person"),
        ("RuleEngine/ObjectDetector/Human", "State", "person"),
        ("RuleEngine/ObjectDetector/Vehicle", "State", "vehicle"),
        ("RuleEngine/ObjectDetector/Animal", "State", "animal"),
        ("RuleEngine/ObjectDetector/Face", "State", "face"),
        (
            "RuleEngine/ObjectDetector/LicensePlate",
            "State",
            "license_plate",
        ),
        ("RuleEngine/ObjectDetector/Package", "State", "package"),
    ];
    for (topic, state, kind) in cases {
        let message = notification(
            topic,
            &format!(r#"<t:SimpleItem Name="{state}" Value="false"/>"#),
        );
        let result = normalize(&message).unwrap().expect(topic);
        assert_eq!(result.kind.as_str(), kind, "{topic}");
        assert_eq!(result.kind.to_string(), kind);
        assert_eq!(result.active, Some(false), "{topic}");
        assert_eq!(result.source.as_deref(), Some("video-1"));
        assert_eq!(result.confidence, None);
        assert_eq!(result.bbox, None);
        assert_eq!(result.text, None);
        assert_eq!(result.count, None);
    }
}

#[test]
fn explicit_boolean_spellings_are_case_insensitive() {
    for (value, active) in [
        ("true", true),
        ("TrUe", true),
        ("1", true),
        ("ON", true),
        ("AcTiVe", true),
        ("false", false),
        ("FaLsE", false),
        ("0", false),
        ("oFf", false),
        ("iNaCtIvE", false),
        (" \tTRUE\r\n", true),
    ] {
        let mut message = parse(MOTION);
        message.data.simple[0].value = value.to_owned();
        assert_eq!(normalize(&message).unwrap().unwrap().active, Some(active));
    }
    for value in ["yes", "no", "2", "-1", "NaN", "", " ", "not active"] {
        let mut message = parse(MOTION);
        message.data.simple[0].value = value.to_owned();
        assert_eq!(normalize(&message).unwrap(), None, "{value}");
    }
}

#[test]
fn point_topics_never_open_long_lived_state() {
    for (topic, kind) in [
        ("RuleEngine/LineDetector/Crossed", "line_crossing"),
        ("RuleEngine/FieldDetector/RegionEntrance", "region_entry"),
        ("RuleEngine/FieldDetector/RegionExit", "region_exit"),
        ("RuleEngine/Recognition/Face", "face"),
        ("RuleEngine/Recognition/LicensePlate", "license_plate"),
        ("RuleEngine/MyRuleDetector/Visitor", "doorbell_press"),
    ] {
        let mut message = notification(topic, "");
        let result = normalize(&message).unwrap().expect(topic);
        assert_eq!(result.kind.as_str(), kind);
        assert_eq!(result.active, None);
        message.data.simple.push(SimpleItem::new("State", "true"));
        assert_eq!(normalize(&message).unwrap().unwrap().active, None);
        message.data.simple[0].value = "false".to_owned();
        assert_eq!(normalize(&message).unwrap(), None);
        message.data.simple[0].value = "unknown".to_owned();
        assert_eq!(normalize(&message).unwrap(), None);
    }
}

#[test]
fn unknown_paths_and_foreign_children_do_not_use_leaf_name_guesses() {
    for topic in [
        "RuleEngine/Unknown/Motion",
        "RuleEngine/Unknown/Person",
        "RuleEngine/LineDetector",
        "Other/MotionAlarm",
        "Device/Trigger/Visitor",
        "Device/Trigger/Doorbell",
        "RuleEngine/ObjectDetector/Unknown",
        "RuleEngine/CellMotionDetector/Motion/Extra",
        "RuleEngine/CellMotionDetector/v:Motion",
        "RuleEngine/v:CellMotionDetector/Motion",
    ] {
        let message = notification(topic, r#"<t:SimpleItem Name="State" Value="true"/>"#);
        assert_eq!(normalize(&message).unwrap(), None, "{topic}");
    }
    let mut message = parse(MOTION);
    message.topic.path[0].namespace_uri = None;
    assert_eq!(normalize(&message).unwrap(), None);
    message.topic.path.clear();
    assert_eq!(normalize(&message).unwrap(), None);
}

#[test]
fn operations_are_retained_and_deleted_closes_only_the_same_identity() {
    let mut message = parse(MOTION);
    let expected = normalize(&message).unwrap().unwrap().identity;
    for operation in [
        None,
        Some(PropertyOperation::Initialized),
        Some(PropertyOperation::Changed),
        Some(PropertyOperation::Other("extension".to_owned())),
    ] {
        message.property_operation = operation.clone();
        let result = normalize(&message).unwrap().unwrap();
        assert_eq!(result.operation, operation);
        assert_eq!(result.identity, expected);
    }
    message.data.simple.clear();
    message.property_operation = Some(PropertyOperation::Deleted);
    let deleted = normalize(&message).unwrap().unwrap();
    assert_eq!(deleted.active, Some(false));
    assert_eq!(deleted.operation, Some(PropertyOperation::Deleted));
    assert_eq!(deleted.identity, expected);
    message.key.simple[0].value = "18".to_owned();
    assert_ne!(normalize(&message).unwrap().unwrap().identity, expected);
    message
        .data
        .simple
        .push(SimpleItem::new("IsMotion", "true"));
    assert!(normalize(&message).is_err());
}

#[test]
fn contradictory_states_and_constructed_duplicate_items_are_rejected() {
    let mut message = parse(MOTION);
    message.data.simple.push(SimpleItem::new("State", "true"));
    assert!(normalize(&message).is_err());
    message.data.simple[1].value = "off".to_owned();
    assert_eq!(normalize(&message).unwrap().unwrap().active, Some(false));
    for section in [0, 1, 2] {
        let mut message = parse(MOTION);
        let items = match section {
            0 => &mut message.source,
            1 => &mut message.key,
            _ => &mut message.data,
        };
        items.simple.push(items.simple[0].clone());
        assert!(normalize(&message).is_err());
    }
}

#[test]
fn sources_are_explicit_camera_tokens_not_inputs_rules_or_data() {
    for name in [
        "VideoSourceConfigurationToken",
        "VideoSourceToken",
        "VideoSource",
        "Source",
        "ChannelID",
        "ChannelId",
        "channelID",
        "Channel",
    ] {
        let mut message = parse(MOTION);
        message.source.simple = vec![SimpleItem::new(name, "camera-token")];
        assert_eq!(
            normalize(&message).unwrap().unwrap().source.as_deref(),
            Some("camera-token")
        );
    }
    let mut message = parse(MOTION);
    message.source.simple = [
        "InputToken",
        "Rule",
        "ObjectId",
        "Token",
        "AudioSourceToken",
    ]
    .map(|name| SimpleItem::new(name, "not-a-camera"))
    .to_vec();
    message
        .data
        .simple
        .push(SimpleItem::new("Source", "not-a-source"));
    message
        .key
        .simple
        .push(SimpleItem::new("VideoSourceToken", "not-a-source"));
    assert_eq!(normalize(&message).unwrap().unwrap().source, None);
    message
        .source
        .simple
        .push(SimpleItem::new("VideoSourceToken", "video"));
    message
        .source
        .simple
        .push(SimpleItem::new("VideoSourceConfigurationToken", "config"));
    assert_eq!(
        normalize(&message).unwrap().unwrap().source.as_deref(),
        Some("config")
    );
}

#[test]
fn rule_identifiers_can_be_in_source_or_key_without_order_dependence() {
    for name in ["Rule", "RuleName", "RuleToken", "RuleId", "RuleID"] {
        let mut message = parse(MOTION);
        message.source.simple.retain(|item| item.name != "Rule");
        message
            .key
            .simple
            .push(SimpleItem::new(name, "rule-secret"));
        assert_eq!(
            normalize(&message).unwrap().unwrap().rule.as_deref(),
            Some("rule-secret")
        );
        message
            .source
            .simple
            .push(SimpleItem::new("Rule", "conflict"));
        assert!(normalize(&message).is_err());
    }
}

#[test]
fn explicit_object_classes_refine_motion_without_changing_state_or_identity() {
    for (class, kind) in [
        ("Human", "person"),
        ("Person", "person"),
        ("HumanBody", "person"),
        ("Vehicle", "vehicle"),
        ("Vehical", "vehicle"),
        ("Car", "vehicle"),
        ("Bus", "vehicle"),
        ("Truck", "vehicle"),
        ("Bicycle", "vehicle"),
        ("Motorcycle", "vehicle"),
        ("Bike", "vehicle"),
        ("Animal", "animal"),
        ("Face", "face"),
        ("HumanFace", "face"),
        ("LicensePlate", "license_plate"),
        ("Package", "package"),
        ("pErSoN", "person"),
    ] {
        for name in ["ObjectClass", "Class", "ClassType", "ClassTypes"] {
            let mut message = parse(MOTION);
            let identity = normalize(&message).unwrap().unwrap().identity;
            message.data.simple.push(SimpleItem::new(name, class));
            let result = normalize(&message).unwrap().unwrap();
            assert_eq!(result.kind.as_str(), kind, "{name}={class}");
            assert_eq!(result.active, Some(false));
            assert_eq!(result.identity, identity);
        }
    }
}

#[test]
fn classification_preserves_rule_point_semantics_and_specific_fallbacks() {
    for (topic, fallback) in [
        ("RuleEngine/LineDetector/Crossed", "line_crossing"),
        ("RuleEngine/FieldDetector/ObjectsInside", "intrusion"),
        ("RuleEngine/FieldDetector/RegionEntrance", "region_entry"),
        ("RuleEngine/FieldDetector/RegionExit", "region_exit"),
        ("RuleEngine/FieldDetector/Loitering", "loitering"),
    ] {
        let mut message = notification(topic, r#"<t:SimpleItem Name="State" Value="true"/>"#);
        let before = normalize(&message).unwrap().unwrap();
        message
            .data
            .simple
            .push(SimpleItem::new("ObjectClass", "Human"));
        let classified = normalize(&message).unwrap().unwrap();
        assert_eq!(classified.kind, Kind::Person);
        assert_eq!(classified.active, before.active);
        for unknown in ["Unknown", "Human Vehicle", "private-class"] {
            message.data.simple[1] = SimpleItem::new("ClassTypes", unknown);
            assert_eq!(
                normalize(&message).unwrap().unwrap().kind.as_str(),
                fallback
            );
        }
    }
    for (topic, kind) in [
        ("VideoSource/Tamper", "tamper"),
        ("VideoSource/SignalLoss", "video_loss"),
        ("Device/Trigger/DigitalInput", "digital_input"),
        ("AudioSource/AudioDetection", "audio_detected"),
        ("RuleEngine/ObjectDetector/Vehicle", "vehicle"),
        ("RuleEngine/Recognition/LicensePlate", "license_plate"),
    ] {
        let message = notification(
            topic,
            r#"<t:SimpleItem Name="State" Value="true"/>
      <t:SimpleItem Name="ObjectClass" Value="Human"/>"#,
        );
        assert_eq!(normalize(&message).unwrap().unwrap().kind.as_str(), kind);
    }
}

#[test]
fn standard_object_detection_requires_unambiguous_explicit_class_evidence() {
    for (classes, kind) in [
        ("Human", "person"),
        ("Human Person", "person"),
        ("Vehicle Car", "vehicle"),
        ("Animal", "animal"),
        ("HumanFace", "face"),
        ("LicensePlate", "license_plate"),
        ("Package", "package"),
    ] {
        let message = notification(
            "RuleEngine/ObjectDetection/Object",
            &format!(r#"<t:SimpleItem Name="ClassTypes" Value="{classes}"/>"#,),
        );
        let result = normalize(&message).unwrap().unwrap();
        assert_eq!(result.kind.as_str(), kind);
        assert_eq!(result.active, None);
    }
    for data in [
        "",
        r#"<t:SimpleItem Name="ClassTypes" Value="Unknown"/>"#,
        r#"<t:SimpleItem Name="ClassTypes" Value="Human Vehicle"/>"#,
        r#"<t:SimpleItem Name="Type" Value="Human"/>"#,
    ] {
        assert_eq!(
            normalize(&notification("RuleEngine/ObjectDetection/Object", data)).unwrap(),
            None
        );
    }
    let message = notification(
        "RuleEngine/Unknown/Object",
        r#"<t:SimpleItem Name="Class" Value="Human"/>"#,
    );
    assert_eq!(normalize(&message).unwrap(), None);
}

#[test]
fn structured_classification_and_shape_use_only_exact_schema_paths() {
    let data = r#"<t:ElementItem Name="Object"><t:Object ObjectId="41">
    <t:Appearance>
    <t:Class><t:Type Likelihood="0.25">Vehicle</t:Type>
      <t:Type Likelihood="0.875">Human</t:Type></t:Class>
    <t:Shape><t:BoundingBox left="-0.5" top="0.75" right="0.5" bottom="-0.5"/></t:Shape>
    </t:Appearance></t:Object></t:ElementItem>"#;
    let result = normalize(&notification("RuleEngine/LineDetector/Crossed", data))
        .unwrap()
        .unwrap();
    assert_eq!(result.kind, Kind::Person);
    assert_eq!(result.active, None);
    assert_eq!(result.confidence, Some(0.875));
    let bbox = result.bbox.unwrap();
    assert_eq!(
        (bbox.x, bbox.y, bbox.width, bbox.height),
        (0.25, 0.125, 0.5, 0.625)
    );
    for ignored in [
        r#"<t:ElementItem Name="Object"><v:Object><t:Appearance><t:Class><t:Type>Human</t:Type></t:Class></t:Appearance></v:Object></t:ElementItem>"#,
        r#"<t:ElementItem Name="Object"><t:Object><v:Appearance><t:Class><t:Type>Human</t:Type></t:Class></v:Appearance></t:Object></t:ElementItem>"#,
        r#"<t:ElementItem Name="Class"><v:Class><t:Type>Human</t:Type></v:Class></t:ElementItem>"#,
        r#"<t:ElementItem Name="Other"><t:Class><t:Type>Human</t:Type></t:Class></t:ElementItem>"#,
        r#"<t:ElementItem Name="BoundingBox"><v:BoundingBox left="bad"/></t:ElementItem>"#,
    ] {
        let result = normalize(&notification("RuleEngine/LineDetector/Crossed", ignored))
            .unwrap()
            .unwrap();
        assert_eq!(result.kind, Kind::LineCrossing);
        assert_eq!(result.confidence, None);
        assert_eq!(result.bbox, None);
    }
}

#[test]
fn class_candidates_choose_explicit_likelihood_and_ties_keep_the_rule() {
    for (candidates, expected) in [
        (
            r#"<t:ClassCandidate><t:Type>Vehicle</t:Type><t:Likelihood>0.75</t:Likelihood></t:ClassCandidate>"#,
            "vehicle",
        ),
        (
            r#"<t:Type Likelihood="0.8">Human</t:Type><t:Type Likelihood="0.9">Unknown</t:Type>"#,
            "line_crossing",
        ),
        (
            r#"<t:Type Likelihood="0.8">Human</t:Type><t:Type Likelihood="0.8">Vehicle</t:Type>"#,
            "line_crossing",
        ),
        (r#"<t:Type>Human</t:Type><t:Type>Person</t:Type>"#, "person"),
    ] {
        let message = notification(
            "RuleEngine/LineDetector/Crossed",
            &format!(
                r#"<t:ElementItem Name="Class"><t:Class>{candidates}</t:Class></t:ElementItem>"#,
            ),
        );
        assert_eq!(
            normalize(&message).unwrap().unwrap().kind.as_str(),
            expected
        );
    }
}

#[test]
fn recognition_attributes_and_debug_keep_opaque_values_private() {
    let data = r#"<t:SimpleItem Name="Likelihood" Value="0.75"/>
    <t:ElementItem Name="BoundingBox"><t:Rectangle left="-0.5" right="0.5" top="0.75" bottom="-0.5"/></t:ElementItem>
    <t:ElementItem Name="LicensePlateInfo"><t:LicensePlateInfo>
    <t:PlateNumber Likelihood="0.9">private-plate</t:PlateNumber>
    </t:LicensePlateInfo></t:ElementItem>"#;
    let mut message = notification("RuleEngine/Recognition/LicensePlate", data);
    message.source.simple[0].value = "private-source".to_owned();
    message.source.simple[1].value = "private-rule".to_owned();
    message
        .key
        .simple
        .push(SimpleItem::new("private-key-name", "private-key"));
    message.property_operation = Some(PropertyOperation::Other("private-operation".to_owned()));
    let result = normalize(&message).unwrap().unwrap();
    assert_eq!(result.kind, Kind::LicensePlate);
    assert_eq!(result.confidence, Some(0.75));
    assert!(result.bbox.is_some());
    assert_eq!(result.text.as_deref(), Some("private-plate"));
    assert_eq!(result.operation, message.property_operation);
    let debug = format!("{result:?} {result:#?}");
    for secret in [
        "private-plate",
        "private-source",
        "private-rule",
        "private-key",
        "private-operation",
    ] {
        assert!(!debug.contains(secret));
    }
    assert!(debug.contains("LicensePlate"));
}

#[test]
fn confidence_bounds_are_validated_without_clamping_or_defaulting() {
    for value in ["0", "0.125", "1"] {
        let message = notification(
            "RuleEngine/LineDetector/Crossed",
            &format!(r#"<t:SimpleItem Name="Confidence" Value="{value}"/>"#,),
        );
        assert_eq!(
            normalize(&message).unwrap().unwrap().confidence,
            Some(value.parse().unwrap())
        );
    }
    for value in [
        "NaN",
        "inf",
        "-inf",
        "-0.001",
        "1.001",
        "1.0000000001",
        "private-number",
        "",
    ] {
        let message = notification(
            "RuleEngine/LineDetector/Crossed",
            &format!(r#"<t:SimpleItem Name="Likelihood" Value="{value}"/>"#,),
        );
        let error = normalize(&message).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("private-number"));
    }
    let message = notification(
        "RuleEngine/LineDetector/Crossed",
        r#"
    <t:SimpleItem Name="Confidence" Value="0.5"/><t:SimpleItem Name="Likelihood" Value="0.75"/>"#,
    );
    assert!(normalize(&message).is_err());
}

#[test]
fn bounding_boxes_must_be_finite_nonempty_and_within_the_image() {
    for attributes in [
        r#"left="-1" top="1" right="1" bottom="-1""#,
        r#"left="0" top="1" right="1" bottom="0""#,
    ] {
        let result = normalize(&notification("RuleEngine/LineDetector/Crossed", &format!(
      r#"<t:ElementItem Name="BoundingBox"><t:BoundingBox {attributes}/></t:ElementItem>"#,
    ))).unwrap().unwrap();
        let bbox = result.bbox.unwrap();
        assert!(bbox.x >= 0.0 && bbox.y >= 0.0);
        assert!(bbox.width > 0.0 && bbox.height > 0.0);
        assert!(bbox.x + bbox.width <= 1.0 && bbox.y + bbox.height <= 1.0);
    }
    for attributes in [
        r#"left="0" top="1" right="0" bottom="0""#,
        r#"left="0.5" top="1" right="0" bottom="0""#,
        r#"left="0" top="0" right="1" bottom="1""#,
        r#"left="-1.0000000001" top="1" right="1" bottom="-1""#,
        r#"left="0" top="1" right="1.1" bottom="0""#,
        r#"left="NaN" top="1" right="1" bottom="0""#,
        r#"left="0" top="inf" right="1" bottom="0""#,
        r#"left="0" top="1" right="1""#,
        r#"left="0.9999999999999999" top="1" right="1" bottom="0""#,
    ] {
        let message = notification(
            "RuleEngine/LineDetector/Crossed",
            &format!(
                r#"<t:ElementItem Name="BoundingBox"><t:BoundingBox {attributes}/></t:ElementItem>"#,
            ),
        );
        assert!(normalize(&message).is_err(), "{attributes}");
    }
}

#[test]
fn object_counts_are_explicit_u64_observations_not_alarm_states() {
    for topic in [
        "RuleEngine/ObjectDetector/Count",
        "RuleEngine/CountAggregation/Counter",
        "RuleEngine/CountAggregation/OccupancyCounter",
    ] {
        for count in [0, 1, u64::MAX] {
            let message = notification(
                topic,
                &format!(r#"<t:SimpleItem Name="Count" Value="{count}"/>"#),
            );
            let result = normalize(&message).unwrap().unwrap();
            assert_eq!(result.kind, Kind::ObjectCount);
            assert_eq!(result.kind.as_str(), "object_count");
            assert_eq!(result.count, Some(count));
            assert_eq!(result.active, None);
        }
        assert_eq!(normalize(&notification(topic, "")).unwrap(), None);
        assert_eq!(
            normalize(&notification(
                topic,
                r#"<t:SimpleItem Name="ActivePostCount" Value="5"/>"#
            ))
            .unwrap(),
            None
        );
    }
    for value in ["-1", "18446744073709551616", "1.5", "NaN", "private-count"] {
        let message = notification(
            "RuleEngine/ObjectDetector/Count",
            &format!(r#"<t:SimpleItem Name="Count" Value="{value}"/>"#,),
        );
        assert!(normalize(&message).is_err());
    }
}

#[test]
fn canonical_keys_preserve_sections_rules_inputs_and_object_ids() {
    let message = parse(MOTION);
    let expected = normalize(&message).unwrap().unwrap().identity;
    let mut changed = message.clone();
    changed.source.simple[1].value = "another-rule".to_owned();
    assert_ne!(normalize(&changed).unwrap().unwrap().identity, expected);
    changed = message.clone();
    changed.key.simple[0].value = "another-object".to_owned();
    assert_ne!(normalize(&changed).unwrap().unwrap().identity, expected);
    changed = message.clone();
    changed.source.simple.push(changed.key.simple.remove(0));
    assert_ne!(normalize(&changed).unwrap().unwrap().identity, expected);
    let mut first = message.clone();
    first.key.simple = vec![SimpleItem::new("a", "b;c=d")];
    let mut second = message;
    second.key.simple = vec![SimpleItem::new("a", "b"), SimpleItem::new("c", "d")];
    assert_ne!(
        normalize(&first).unwrap().unwrap().identity,
        normalize(&second).unwrap().unwrap().identity
    );
    first.source.simple = vec![SimpleItem::new("InputToken", "input-1")];
    second = first.clone();
    second.source.simple[0].value = "input-2".to_owned();
    assert_eq!(normalize(&first).unwrap().unwrap().source, None);
    assert_ne!(
        normalize(&first).unwrap().unwrap().identity,
        normalize(&second).unwrap().unwrap().identity
    );
}

#[test]
fn canonical_keys_ignore_item_order_data_time_and_operations() {
    let mut message = parse(MOTION);
    message.key.simple.extend([
        SimpleItem::new("z", "opaque:;=|"),
        SimpleItem::new("a", "\u{e9}"),
    ]);
    let expected = normalize(&message).unwrap().unwrap().identity;
    for _ in 0..3 {
        message.key.simple.rotate_left(1);
        message.source.simple.reverse();
        assert_eq!(normalize(&message).unwrap().unwrap().identity, expected);
    }
    message.utc_time += chrono::Duration::hours(3);
    message.property_operation = Some(PropertyOperation::Initialized);
    message.data.simple = vec![
        SimpleItem::new("IsMotion", "true"),
        SimpleItem::new("ObjectId", "new-data-object"),
    ];
    assert_eq!(normalize(&message).unwrap().unwrap().identity, expected);
    message.property_operation = Some(PropertyOperation::Deleted);
    message.data.simple.clear();
    assert_eq!(normalize(&message).unwrap().unwrap().identity, expected);
}

#[test]
fn structured_keys_are_prefix_and_attribute_order_independent() {
    let mut first = notification(
        "RuleEngine/LineDetector/Crossed",
        r#"
    <t:ElementItem Name="Region"><v:Region v:id="zone" rank="1"><v:Part>A</v:Part><v:Part>B</v:Part></v:Region></t:ElementItem>"#,
    );
    first.key.element = std::mem::take(&mut first.data.element);
    let mut second = notification(
        "RuleEngine/LineDetector/Crossed",
        r#"
    <t:ElementItem Name="Region"><alt:Region xmlns:alt="urn:unrelated" rank="1" alt:id="zone"><alt:Part>A</alt:Part><alt:Part>B</alt:Part></alt:Region></t:ElementItem>"#,
    );
    second.key.element = std::mem::take(&mut second.data.element);
    let expected = normalize(&first).unwrap().unwrap().identity;
    assert_eq!(normalize(&second).unwrap().unwrap().identity, expected);
    second.key.element[0].value.attributes.reverse();
    assert_eq!(normalize(&second).unwrap().unwrap().identity, expected);
    second.key.element[0].value.children.reverse();
    assert_ne!(normalize(&second).unwrap().unwrap().identity, expected);
    second = first.clone();
    second.key.element[0].value.name.namespace_uri = Some("urn:different".to_owned());
    assert_ne!(normalize(&second).unwrap().unwrap().identity, expected);
    second = first.clone();
    second.key.element[0].value.attributes[0].value = "other-zone".to_owned();
    assert_ne!(normalize(&second).unwrap().unwrap().identity, expected);
}

#[test]
fn identity_limit_counts_framing_and_utf8_bytes_without_truncation() {
    let mut message = parse(MOTION);
    message.key.simple[0].value = "x".repeat(1000);
    let baseline = normalize(&message).unwrap().unwrap().identity.len();
    let fill_length = 1000 + (4096 - baseline);
    message.key.simple[0].value = "x".repeat(fill_length);
    assert_eq!(normalize(&message).unwrap().unwrap().identity.len(), 4096);
    message.key.simple[0].value.push('x');
    assert!(normalize(&message).is_err());
    message.key.simple[0].value = "\u{e9}".repeat(fill_length / 2 + 1);
    assert!(normalize(&message).is_err());
}

#[test]
fn constructed_notifications_bound_total_items_and_topic_segments() {
    let mut message = parse(MOTION);
    message.source.simple.clear();
    message.key.simple.clear();
    message
        .data
        .simple
        .extend((0..255).map(|index| SimpleItem::new(format!("extra-{index}"), "x")));
    assert!(normalize(&message).unwrap().is_some());
    message
        .source
        .simple
        .push(SimpleItem::new("Source", "camera"));
    assert!(normalize(&message).is_err());
    message = parse(MOTION);
    message.topic.path.resize(32, message.topic.path[0].clone());
    assert_eq!(normalize(&message).unwrap(), None);
    message.topic.path.push(message.topic.path[0].clone());
    assert!(normalize(&message).is_err());
}

fn structured_notification() -> Notification {
    notification(
        "RuleEngine/LineDetector/Crossed",
        r#"
    <t:ElementItem Name="Details"><v:Details v:id="opaque"/></t:ElementItem>"#,
    )
}

#[test]
fn canonical_xml_key_text_is_not_discarded_as_indentation() {
    let mut message = structured_notification();
    message.key.element = std::mem::take(&mut message.data.element);
    let child = message.key.element[0].value.clone();
    message.key.element[0].value.children.push(child);
    let empty = normalize(&message).unwrap().unwrap().identity;
    message.key.element[0].value.text = " ".to_owned();
    let space = normalize(&message).unwrap().unwrap().identity;
    assert_ne!(space, empty);
    message.key.element[0].value.text = "  ".to_owned();
    assert_ne!(normalize(&message).unwrap().unwrap().identity, space);
}

#[test]
fn constructed_xml_has_depth_node_and_attribute_limits() {
    let mut message = structured_notification();
    let leaf = message.data.element[0].value.clone();
    let mut tree = leaf.clone();
    for _ in 1..32 {
        let mut parent = leaf.clone();
        parent.children.push(tree);
        tree = parent;
    }
    message.data.element[0].value = tree;
    assert!(normalize(&message).unwrap().is_some());
    let mut parent = leaf.clone();
    parent.children.push(message.data.element[0].value.clone());
    message.data.element[0].value = parent;
    assert!(normalize(&message).is_err());
    message.data.element[0].value = leaf.clone();
    message.data.element[0].value.children = vec![leaf.clone(); 255];
    assert!(normalize(&message).unwrap().is_some());
    message.data.element[0].value.children.push(leaf.clone());
    assert!(normalize(&message).is_err());
    message.data.element[0].value = leaf.clone();
    message.data.element[0].value.attributes = (0..32)
        .map(|index| {
            let mut attribute = leaf.attributes[0].clone();
            attribute.name.local_name = format!("attribute-{index}");
            attribute
        })
        .collect();
    assert!(normalize(&message).unwrap().is_some());
    let mut attribute = leaf.attributes[0].clone();
    attribute.name.local_name = "extra".to_owned();
    message.data.element[0].value.attributes.push(attribute);
    assert!(normalize(&message).is_err());
}

#[test]
fn constructed_xml_and_simple_items_share_duplicate_name_checks() {
    let mut message = structured_notification();
    let attribute = message.data.element[0].value.attributes[0].clone();
    message.data.element[0].value.attributes.push(attribute);
    assert!(normalize(&message).is_err());
    message = structured_notification();
    message
        .data
        .simple
        .push(SimpleItem::new("Details", "duplicate"));
    assert!(normalize(&message).is_err());
    message = structured_notification();
    message.data.element.push(message.data.element[0].clone());
    assert!(normalize(&message).is_err());
}

#[test]
fn text_and_opaque_inputs_have_per_value_and_aggregate_byte_limits() {
    let mut message = notification("RuleEngine/LineDetector/Crossed", "");
    message
        .data
        .simple
        .push(SimpleItem::new("Text", "\u{e9}".repeat(128)));
    assert_eq!(
        normalize(&message).unwrap().unwrap().text.unwrap().len(),
        256
    );
    message.data.simple[0].value.push('x');
    assert!(normalize(&message).is_err());
    message = structured_notification();
    message.data.element[0].value.text = "x".repeat(64 * 1024);
    assert!(normalize(&message).unwrap().is_some());
    message.data.element[0].value.text.push('x');
    assert!(normalize(&message).is_err());
    message = structured_notification();
    message.data.element[0].value.text = "x".repeat(64 * 1024);
    let mut another = message.data.element[0].clone();
    for index in 1..3 {
        another.name = format!("Details-{index}");
        message.data.element.push(another.clone());
    }
    assert!(normalize(&message).unwrap().is_some());
    another.name = "Details-4".to_owned();
    message.data.element.push(another);
    assert!(normalize(&message).is_err());
    message = parse(MOTION);
    message.property_operation = Some(PropertyOperation::Other("x".repeat(257)));
    assert!(normalize(&message).is_err());
    message = parse(MOTION);
    message
        .data
        .simple
        .push(SimpleItem::new("Opaque", "x".repeat(4097)));
    assert!(normalize(&message).is_err());
}

#[test]
fn class_candidates_and_recognized_xml_fields_are_bounded_and_validated() {
    let mut message = notification("RuleEngine/ObjectDetection/Object", "");
    message
        .data
        .simple
        .push(SimpleItem::new("ClassTypes", "Human ".repeat(32)));
    assert_eq!(normalize(&message).unwrap().unwrap().kind, Kind::Person);
    message.data.simple[0].value.push_str("Human");
    assert!(normalize(&message).is_err());
    for data in [
        r#"<t:ElementItem Name="Class"><t:Class><t:Type Likelihood="NaN">Human</t:Type></t:Class></t:ElementItem>"#,
        r#"<t:ElementItem Name="Class"><t:Class><t:ClassCandidate><t:Type>Human</t:Type></t:ClassCandidate></t:Class></t:ElementItem>"#,
        r#"<t:ElementItem Name="Class"><t:Class><t:Type><v:Text>Human</v:Text></t:Type></t:Class></t:ElementItem>"#,
        r#"<t:ElementItem Name="Object"><t:Object><t:Appearance/><t:Appearance/></t:Object></t:ElementItem>"#,
        r#"<t:ElementItem Name="LicensePlateInfo"><t:LicensePlateInfo><t:PlateNumber>A</t:PlateNumber><t:PlateNumber>B</t:PlateNumber></t:LicensePlateInfo></t:ElementItem>"#,
    ] {
        assert!(normalize(&notification("RuleEngine/LineDetector/Crossed", data)).is_err());
    }
}

#[test]
fn missing_state_is_not_inferred_from_source_key_or_detection_attributes() {
    let mut message = parse(MOTION);
    message.data.simple = vec![
        SimpleItem::new("Class", "Human"),
        SimpleItem::new("Count", "3"),
    ];
    message.source.simple.push(SimpleItem::new("State", "true"));
    message.key.simple.push(SimpleItem::new("IsMotion", "true"));
    assert_eq!(normalize(&message).unwrap(), None);
    message.property_operation = Some(PropertyOperation::Deleted);
    assert_eq!(normalize(&message).unwrap().unwrap().active, Some(false));
    message.property_operation = Some(PropertyOperation::Initialized);
    message
        .data
        .simple
        .push(SimpleItem::new("IsMotion", "true"));
    let result = normalize(&message).unwrap().unwrap();
    assert_eq!(result.active, Some(true));
    assert_eq!(result.operation, Some(PropertyOperation::Initialized));
    message
        .data
        .simple
        .push(SimpleItem::new("State", "unknown"));
    assert_eq!(normalize(&message).unwrap(), None);
    message
        .data
        .simple
        .push(SimpleItem::new("IsMotion", "false"));
    assert!(normalize(&message).is_err());
}
