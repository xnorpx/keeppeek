use isapi::{Event, Target};

#[test]
fn nested_region_objects_retain_identity_attributes_and_geometry() {
    let xml = r#"<EventNotificationAlert xmlns="http://www.isapi.org/ver20/XMLSchema"><eventType>linedetection</eventType><eventState>active</eventState><channelID>1</channelID><uuid>event-1</uuid><DetectionRegionList><DetectionRegionEntry><regionID>4</regionID><detectionTarget>human</detectionTarget><TargetList><Target><targetID>7</targetID><targetType>human</targetType><confidenceLevel>92</confidenceLevel><targetRect><X>100</X><Y>200</Y><width>300</width><height>400</height></targetRect><humanInfo><jacketColor>red</jacketColor><unknownAttribute>retained</unknownAttribute></humanInfo><contentID>person-7.jpg</contentID></Target></TargetList></DetectionRegionEntry></DetectionRegionList><VendorExtension><opaque>keep me</opaque></VendorExtension></EventNotificationAlert>"#;
    let event = Event::parse(xml).unwrap();
    assert_eq!(event.id(), Some("event-1"));
    let object = &event.objects()[0];
    assert_eq!(object.id(), Some("7"));
    assert_eq!(object.region(), Some("4"));
    assert_eq!(object.target(), Some(Target::Person));
    assert_eq!(object.confidence().unwrap().normalized(), 0.92);
    assert_eq!(
        object.attributes().get("jacketColor").map(String::as_str),
        Some("red")
    );
    assert_eq!(
        object.bbox().unwrap().normalized(None).unwrap(),
        [0.1, 0.2, 0.3, 0.4]
    );
    assert_eq!(object.image_id(), Some("person-7.jpg"));
    assert!(
        serde_json::to_string(event.data())
            .unwrap()
            .contains("keep me")
    );
}

#[test]
fn anpr_retains_plate_vehicle_attributes_and_explicit_picture_references() {
    let xml = r#"<EventNotificationAlert><eventType>ANPR</eventType><eventState>active</eventState><channelID>2</channelID><ANPR><licensePlate>TEST123</licensePlate><confidenceLevel>98</confidenceLevel><vehicleType>car</vehicleType><vehicleInfo><color>blue</color><vehicleLogo>example</vehicleLogo></vehicleInfo><pictureInfoList><pictureInfo><fileName>vehicle.jpg</fileName><type>vehiclePicture</type><plateRect><X>20</X><Y>40</Y><width>60</width><height>80</height></plateRect></pictureInfo></pictureInfoList></ANPR></EventNotificationAlert>"#;
    let event = Event::parse(xml).unwrap();
    let object = &event.objects()[0];
    assert_eq!(object.target(), Some(Target::Vehicle));
    assert_eq!(object.plate(), Some("TEST123"));
    assert_eq!(
        object.attributes().get("color").map(String::as_str),
        Some("blue")
    );
    assert_eq!(
        object.bbox().unwrap().normalized(Some((200, 200))).unwrap(),
        [0.1, 0.2, 0.3, 0.4]
    );
    assert_eq!(object.image_id(), Some("vehicle.jpg"));
    assert_eq!(event.images()[0].id(), "vehicle.jpg");
}

#[test]
fn json_collections_remain_separate_and_unknown_nested_targets_are_not_guessed() {
    let event = Event::parse_json(br#"{"eventType":"targetCapture","eventState":"active","channelID":1,"detectionResult":[{"targetID":"p1","targetType":"human","humanInfo":{"ageGroup":"adult"}},{"targetID":"v1","targetType":"vehicle","vehicleInfo":{"vehicleType":"car"}}],"unknown":{"targetType":"human"}}"#).unwrap();
    assert_eq!(event.objects().len(), 2);
    assert_eq!(event.objects()[0].target(), Some(Target::Person));
    assert_eq!(event.objects()[1].target(), Some(Target::Vehicle));
    assert!(
        serde_json::to_string(event.data())
            .unwrap()
            .contains("unknown")
    );
    let unknown = Event::parse_json(
        br#"{"eventType":"VMD","eventState":"active","extension":{"targetType":"human"}}"#,
    )
    .unwrap();
    assert!(unknown.objects().is_empty());
}

#[test]
fn duplicate_identity_conflicting_classification_and_invalid_geometry_fail_closed() {
    for objects in [
        r#"[{"targetID":"same","targetType":"human"},{"targetID":"same","targetType":"vehicle"}]"#,
        r#"[{"targetType":"human","detectionTarget":"vehicle"}]"#,
        r#"[{"targetType":"human","targetRect":{"X":900,"Y":0,"width":200,"height":1}}]"#,
        r#"[{"targetType":"human","confidenceLevel":101}]"#,
    ] {
        let text = format!(
            r#"{{"eventType":"targetCapture","eventState":"active","detectionResult":{objects}}}"#
        );
        assert!(Event::parse_json(text).is_err());
    }
}
