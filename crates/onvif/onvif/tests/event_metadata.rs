use onvif::event::{Metadata, ObjectClass, XmlElement, parse_notifications};

fn document(contents: &str) -> String {
    format!(
        r#"<t:MetadataStream xmlns:t="http://www.onvif.org/ver10/schema"
            xmlns:n="http://docs.oasis-open.org/wsn/b-2"
            xmlns:q="http://www.onvif.org/ver10/topics" xmlns:v="urn:fixture">
            {contents}</t:MetadataStream>"#
    )
}

fn notification(utc_time: &str) -> String {
    format!(
        r#"<n:NotificationMessage>
            <n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">
                q:RuleEngine/LineDetector/Crossed
            </n:Topic>
            <n:Message><t:Message UtcTime="{utc_time}" PropertyOperation="Changed">
                <t:Source><t:SimpleItem Name="VideoSource" Value="source-1"/></t:Source>
                <t:Data><t:ElementItem Name="Details">
                    <v:Details v:kind="opaque"><v:Value>private-detail</v:Value></v:Details>
                </t:ElementItem></t:Data>
            </t:Message></n:Message>
        </n:NotificationMessage>"#
    )
}

#[test]
fn event_messages_keep_valid_neighbors_and_original_expanded_xml() {
    let valid = notification("2026-09-05T12:00:00Z");
    let invalid = notification("private-invalid-timestamp");
    let xml = document(&format!(
        "<t:Event>{invalid}{valid}{invalid}{valid}{invalid}</t:Event>"
    ));
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    let expected = parse_notifications(document(&format!("<t:Event>{valid}</t:Event>")).as_bytes())
        .unwrap()
        .remove(0);

    assert_eq!(metadata.invalid_messages, 3);
    assert_eq!(metadata.notifications, vec![expected.clone(), expected]);
    let details: &XmlElement = &metadata.notifications[0].data.element[0].value;
    assert_eq!(details.name.namespace_uri.as_deref(), Some("urn:fixture"));
    assert_eq!(
        details.attributes[0].name.namespace_uri.as_deref(),
        Some("urn:fixture")
    );
    assert!(!format!("{metadata:?}").contains("private-detail"));
}

#[test]
fn metadata_accepts_an_empty_stream_and_ignores_ptz_contents() {
    let metadata = Metadata::parse(document("").as_bytes()).unwrap();
    assert!(metadata.notifications.is_empty());
    assert_eq!(metadata.invalid_messages, 0);
    let xml = document(&format!("<t:PTZ>{}</t:PTZ>", notification("bad")));
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert!(metadata.notifications.is_empty());
    assert_eq!(metadata.invalid_messages, 0);
}

#[test]
fn metadata_rejects_unrelated_roots_namespaces_and_document_types() {
    for xml in [
        "<MetadataStream/>",
        r#"<MetadataStream xmlns="urn:fixture"/>"#,
        r#"<Frame xmlns="http://www.onvif.org/ver10/schema"/>"#,
        r#"<!DOCTYPE MetadataStream><MetadataStream xmlns="http://www.onvif.org/ver10/schema"/>"#,
        "<private-peer-payload>",
    ] {
        let error = Metadata::parse(xml.as_bytes()).unwrap_err();
        assert!(!error.to_string().contains("private-peer-payload"));
        assert!(!format!("{error:?}").contains("private-peer-payload"));
    }
}

fn frame(contents: &str) -> String {
    format!(r#"<t:Frame UtcTime="2026-09-05T12:00:00Z">{contents}</t:Frame>"#)
}

fn analytics(contents: &str) -> String {
    document(&format!("<t:VideoAnalytics>{contents}</t:VideoAnalytics>"))
}

#[test]
fn analytics_frames_are_partial_and_multiplex_with_events() {
    let update = r#"<t:Frame UtcTime="2026-09-05T14:00:00+02:00" Source="module-1">
        <t:Object ObjectId="17"/>
    </t:Frame>"#;
    let deletion = frame(r#"<t:ObjectTree><t:Delete ObjectId="17"/></t:ObjectTree>"#);
    let xml = document(&format!(
        "<t:VideoAnalytics>{update}{}</t:VideoAnalytics><t:Event>{}</t:Event>
         <t:VideoAnalytics>{deletion}</t:VideoAnalytics>",
        frame(""),
        notification("2026-09-05T12:00:00Z")
    ));
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert_eq!(metadata.notifications.len(), 1);
    assert_eq!(metadata.frames.len(), 3);
    assert_eq!(
        metadata.frames[0].utc_time.to_rfc3339(),
        "2026-09-05T12:00:00+00:00"
    );
    assert_eq!(metadata.frames[0].source.as_deref(), Some("module-1"));
    assert_eq!(metadata.frames[0].objects[0].id, "17");
    assert_eq!(metadata.frames[0].objects[0].class, None);
    assert!(metadata.frames[1].objects.is_empty());
    assert!(metadata.frames[1].deleted_ids.is_empty());
    assert_eq!(metadata.frames[1].source, None);
    assert_eq!(metadata.frames[2].deleted_ids, ["17"]);
}

#[test]
fn analytics_reads_only_exact_paths_and_unqualified_attributes() {
    let xml = analytics(&frame(
        r#"<v:Object ObjectId="foreign"/><v:Extension><t:Object ObjectId="nested"/></v:Extension>
           <t:Object ObjectId="5" v:ObjectId="foreign"/>"#,
    ));
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert_eq!(metadata.frames[0].objects.len(), 1);
    assert_eq!(metadata.frames[0].objects[0].id, "5");
    let xml =
        analytics(r#"<v:Frame/><t:Frame UtcTime="2026-09-05T12:00:00Z" v:Source="foreign"/>"#);
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert_eq!(metadata.frames.len(), 1);
    assert_eq!(metadata.frames[0].source, None);
    for content in [
        r#"<t:Frame v:UtcTime="2026-09-05T12:00:00Z"/>"#.to_owned(),
        frame(r#"<t:Object v:ObjectId="foreign"/>"#),
        frame(r#"<t:ObjectTree><t:Delete><t:ObjectId>5</t:ObjectId></t:Delete></t:ObjectTree>"#),
    ] {
        assert!(Metadata::parse(analytics(&content).as_bytes()).is_err());
    }
}

#[test]
fn analytics_bounds_frames_objects_and_explicit_deletions() {
    let xml = analytics(&frame("").repeat(64));
    assert_eq!(Metadata::parse(xml.as_bytes()).unwrap().frames.len(), 64);
    let xml = analytics(&frame("").repeat(65));
    assert!(Metadata::parse(xml.as_bytes()).is_err());
    for (element, wrapper) in [("Object", ""), ("Delete", "ObjectTree")] {
        for count in [128, 129] {
            let entries = (0..count)
                .map(|index| format!(r#"<t:{element} ObjectId="{index}"/>"#))
                .collect::<String>();
            let contents = if wrapper.is_empty() {
                entries
            } else {
                format!("<t:{wrapper}>{entries}</t:{wrapper}>")
            };
            let parsed = Metadata::parse(analytics(&frame(&contents)).as_bytes());
            assert_eq!(parsed.is_ok(), count == 128);
        }
    }
}

#[test]
fn analytics_bounds_ids_and_rejects_missing_or_invalid_timestamps() {
    for length in [256, 257] {
        let contents = format!(r#"<t:Object ObjectId="{}"/>"#, "7".repeat(length));
        let parsed = Metadata::parse(analytics(&frame(&contents)).as_bytes());
        assert_eq!(parsed.is_ok(), length == 256);
    }
    for contents in [
        "<t:Frame/>".to_owned(),
        r#"<t:Frame UtcTime="2026-13-05T12:00:00"/>"#.to_owned(),
        r#"<t:Frame UtcTime="2026-02-30T12:00:00Z"/>"#.to_owned(),
        r#"<t:Frame UtcTime="2026-09-05T12:00:00+99:99"/>"#.to_owned(),
        frame(r#"<t:Object ObjectId=""/>"#),
        frame("<t:Object/>"),
    ] {
        assert!(Metadata::parse(analytics(&contents).as_bytes()).is_err());
    }
}

#[test]
fn timezone_free_onvif_frame_times_are_explicitly_utc() {
    for time in ["2026-09-05T12:00:00", "2026-09-05T12:00:00.321"] {
        let xml = analytics(&format!(r#"<t:Frame UtcTime="{time}"/>"#));
        let metadata = Metadata::parse(xml.as_bytes()).unwrap();
        assert_eq!(
            metadata.frames[0].utc_time.to_rfc3339(),
            format!("{time}+00:00")
        );
    }
}

fn object_xml(appearance: &str) -> String {
    analytics(&frame(&format!(
        r#"<t:Object ObjectId="17"><t:Appearance>{appearance}</t:Appearance></t:Object>"#
    )))
}

#[test]
fn class_candidates_and_modern_types_map_only_explicit_categories() {
    for (label, expected) in [
        ("Human", ObjectClass::Person),
        ("HumanBody", ObjectClass::Person),
        ("Person", ObjectClass::Person),
        ("Vehicle", ObjectClass::Vehicle),
        ("Vehical", ObjectClass::Vehicle),
        ("Car", ObjectClass::Vehicle),
        ("Bicycle", ObjectClass::Vehicle),
        ("Animal", ObjectClass::Animal),
        ("HumanFace", ObjectClass::Face),
        ("Face", ObjectClass::Face),
        ("LicensePlate", ObjectClass::LicensePlate),
        ("Package", ObjectClass::Package),
    ] {
        for class in [
            format!(
                "<t:ClassCandidate><t:Type>{label}</t:Type>
                 <t:Likelihood>0.75</t:Likelihood></t:ClassCandidate>"
            ),
            format!(r#"<t:Type Likelihood="0.75">{label}</t:Type>"#),
        ] {
            let xml = object_xml(&format!("<t:Class>{class}</t:Class>"));
            let metadata = Metadata::parse(xml.as_bytes()).unwrap();
            let object = &metadata.frames[0].objects[0];
            assert_eq!(object.class, Some(expected));
            assert_eq!(object.confidence, Some(0.75));
        }
    }
}

#[test]
fn strongest_unknown_and_tied_classes_stay_unclassified() {
    for (second, probability) in [("Unknown", "0.9"), ("Vehicle", "0.4")] {
        let xml = object_xml(&format!(
            r#"<t:Class><t:Type Likelihood="0.4">Human</t:Type>
                <t:Type Likelihood="{probability}">{second}</t:Type></t:Class>
                <t:HumanBody/>"#
        ));
        let metadata = Metadata::parse(xml.as_bytes()).unwrap();
        assert_eq!(metadata.frames[0].objects[0].class, None);
        assert_eq!(
            metadata.frames[0].objects[0].confidence,
            probability.parse().ok()
        );
    }
    let xml = object_xml(
        r#"<t:Class><t:Type Likelihood="0.1">Unknown</t:Type>
            <t:Type Likelihood="0.9">Human</t:Type></t:Class>"#,
    );
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert_eq!(
        metadata.frames[0].objects[0].class,
        Some(ObjectClass::Person)
    );
    assert_eq!(metadata.frames[0].objects[0].confidence, Some(0.9));
}

#[test]
fn typed_appearance_fields_provide_explicit_classification_without_invented_confidence() {
    for (appearance, expected) in [
        ("<t:HumanBody/>", ObjectClass::Person),
        ("<t:HumanFace/>", ObjectClass::Face),
        ("<t:HumanBody/><t:HumanFace/>", ObjectClass::Person),
        (
            "<t:VehicleInfo><t:Type>Truck</t:Type></t:VehicleInfo>",
            ObjectClass::Vehicle,
        ),
        (
            "<t:LicensePlateInfo><t:PlateNumber>TEST-42</t:PlateNumber></t:LicensePlateInfo>",
            ObjectClass::LicensePlate,
        ),
    ] {
        let metadata = Metadata::parse(object_xml(appearance).as_bytes()).unwrap();
        assert_eq!(metadata.frames[0].objects[0].class, Some(expected));
        assert_eq!(metadata.frames[0].objects[0].confidence, None);
    }
}

#[test]
fn unknown_labels_and_misplaced_or_foreign_fields_do_not_create_detections() {
    for appearance in [
        "<t:Class><t:Type>Other</t:Type></t:Class>",
        "<t:Class><t:Type>unknown-human-motion</t:Type></t:Class>",
        "<v:Class><t:Type>Human</t:Type></v:Class>",
        "<v:HumanBody/><v:HumanFace/>",
        "<t:Extension><t:HumanBody/><t:Type>Vehicle</t:Type></t:Extension>",
        "<t:HumanBody/><t:VehicleInfo><t:Type>Car</t:Type></t:VehicleInfo>",
    ] {
        let metadata = Metadata::parse(object_xml(appearance).as_bytes()).unwrap();
        let object = &metadata.frames[0].objects[0];
        assert_eq!(object.class, None);
        assert_eq!(object.text, None);
        assert!(!format!("{object:?}").contains("unknown-human-motion"));
        assert!(metadata.notifications.is_empty());
    }
}

#[test]
fn class_and_recognition_probabilities_must_be_finite_and_in_range() {
    for value in [
        "NaN",
        "INF",
        "-0.01",
        "1.01",
        "1.00000001",
        "1e400",
        "not-a-number",
    ] {
        for appearance in [
            format!(r#"<t:Class><t:Type Likelihood="{value}">Human</t:Type></t:Class>"#),
            format!(
                "<t:Class><t:ClassCandidate><t:Type>Unknown</t:Type>
                 <t:Likelihood>{value}</t:Likelihood></t:ClassCandidate></t:Class>"
            ),
            format!(
                r#"<t:LicensePlateInfo><t:PlateNumber Likelihood="{value}">TEST</t:PlateNumber>
                    </t:LicensePlateInfo>"#
            ),
        ] {
            assert!(Metadata::parse(object_xml(&appearance).as_bytes()).is_err());
        }
    }
}

#[test]
fn recognition_text_is_explicit_bounded_and_redacted_from_debug() {
    for (element, field) in [("LicensePlateInfo", "PlateNumber"), ("BarcodeInfo", "Data")] {
        let appearance = format!(
            "<t:{element}><t:{field}>PRIVATE-RECOGNITION</t:{field}></t:{element}>
             <t:Extension><v:Text>PRIVATE-UNKNOWN</v:Text></t:Extension>"
        );
        let metadata = Metadata::parse(object_xml(&appearance).as_bytes()).unwrap();
        assert_eq!(
            metadata.frames[0].objects[0].text.as_deref(),
            Some("PRIVATE-RECOGNITION")
        );
        for debug in [
            format!("{metadata:?}"),
            format!("{:?}", metadata.frames[0]),
            format!("{:?}", metadata.frames[0].objects[0]),
        ] {
            assert!(!debug.contains("PRIVATE-RECOGNITION"));
            assert!(!debug.contains("PRIVATE-UNKNOWN"));
        }
        for length in [256, 257] {
            let appearance = format!(
                "<t:{element}><t:{field}>{}</t:{field}></t:{element}>",
                "X".repeat(length)
            );
            assert_eq!(
                Metadata::parse(object_xml(&appearance).as_bytes()).is_ok(),
                length == 256
            );
        }
    }
}

fn shape(attributes: &str) -> String {
    format!(r#"<t:Shape><t:BoundingBox {attributes}/><t:CenterOfGravity x="0" y="0"/></t:Shape>"#)
}

fn assert_box(metadata: &Metadata, expected: [f32; 4]) {
    let bbox = metadata.frames[0].objects[0].bbox.unwrap();
    assert_eq!([bbox.x, bbox.y, bbox.width, bbox.height], expected);
}

#[test]
fn boxes_normalize_y_up_to_top_left_without_creating_a_class() {
    let xml = object_xml(&shape(
        r#"left="-0.5" right="0.5" top="0.75" bottom="-0.25""#,
    ));
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert_box(&metadata, [0.25, 0.125, 0.5, 0.5]);
    assert_eq!(metadata.frames[0].objects[0].class, None);
    assert!(metadata.notifications.is_empty());
}

#[test]
fn frame_transform_scales_then_translates_and_resets_at_each_frame() {
    let transformed = frame(&format!(
        r#"<t:Transformation><t:Translate x="-0.5" y="0.25"/>
            <t:Scale x="0.25" y="0.5"/></t:Transformation>
            <t:Object ObjectId="1"><t:Appearance>{}</t:Appearance></t:Object>"#,
        shape(r#"left="0" right="2" top="1" bottom="0""#)
    ));
    let next = frame(&format!(
        r#"<t:Object ObjectId="1"><t:Appearance>{}</t:Appearance></t:Object>"#,
        shape(r#"left="0" right="0.5" top="0.5" bottom="0""#)
    ));
    let metadata = Metadata::parse(analytics(&format!("{transformed}{next}")).as_bytes()).unwrap();
    assert_box(&metadata, [0.25, 0.125, 0.25, 0.25]);
    let bbox = metadata.frames[1].objects[0].bbox.unwrap();
    assert_eq!(
        [bbox.x, bbox.y, bbox.width, bbox.height],
        [0.5, 0.25, 0.25, 0.25]
    );
}

#[test]
fn negative_y_scale_maps_top_left_pixel_coordinates() {
    let xml = analytics(&frame(&format!(
        r#"<t:Transformation><t:Translate x="-1" y="1"/>
            <t:Scale x="0.01" y="-0.02"/></t:Transformation>
            <t:Object ObjectId="1"><t:Appearance>{}</t:Appearance></t:Object>"#,
        shape(r#"left="50" right="150" top="25" bottom="75""#)
    )));
    assert_box(
        &Metadata::parse(xml.as_bytes()).unwrap(),
        [0.25, 0.25, 0.5, 0.5],
    );
}

#[test]
fn appearance_transform_composes_with_the_frame_transform() {
    let xml = analytics(&frame(&format!(
        r#"<t:Transformation><t:Translate x="-0.5" y="-0.5"/>
            <t:Scale x="0.5" y="0.5"/></t:Transformation>
            <t:Object ObjectId="1"><t:Appearance>
                <t:Transformation><t:Translate x="0.5" y="0.5"/>
                    <t:Scale x="0.5" y="0.5"/></t:Transformation>{}
            </t:Appearance></t:Object>"#,
        shape(r#"left="0" right="1" top="1" bottom="0""#)
    )));
    assert_box(
        &Metadata::parse(xml.as_bytes()).unwrap(),
        [0.375, 0.5, 0.125, 0.125],
    );
}

#[test]
fn invalid_degenerate_and_out_of_image_boxes_are_rejected() {
    for attributes in [
        r#"left="0.5" right="-0.5" top="0.5" bottom="-0.5""#,
        r#"left="-0.5" right="0.5" top="-0.5" bottom="0.5""#,
        r#"left="0" right="0" top="0.5" bottom="0""#,
        r#"left="0" right="0.5" top="0" bottom="0""#,
        r#"left="-1.01" right="0.5" top="0.5" bottom="-0.5""#,
        r#"left="-0.5" right="0.5" top="1.01" bottom="-0.5""#,
        r#"left="NaN" right="0.5" top="0.5" bottom="-0.5""#,
        r#"left="-0.5" right="INF" top="0.5" bottom="-0.5""#,
        r#"left="-0.5" right="0.5" top="1e400" bottom="-0.5""#,
        r#"left="-0.5" right="0.5" top="0.5""#,
        r#"v:left="-0.5" right="0.5" top="0.5" bottom="-0.5""#,
    ] {
        assert!(Metadata::parse(object_xml(&shape(attributes)).as_bytes()).is_err());
    }
    let xml = object_xml(
        r#"<t:Shape><t:BoundingBox left="0" right="1" top="1" bottom="0">invalid
            </t:BoundingBox></t:Shape>"#,
    );
    assert!(Metadata::parse(xml.as_bytes()).is_err());
}

#[test]
fn malformed_transforms_are_rejected_even_without_shapes() {
    for contents in [
        r#"<t:Translate x="NaN" y="0"/>"#,
        r#"<t:Scale x="0" y="1"/>"#,
        r#"<t:Scale x="1" y="0"/>"#,
        r#"<t:Scale x="inf" y="1"/>"#,
        r#"<t:Scale x="1"/>"#,
        r#"<t:Translate v:x="0" y="0"/>"#,
        r#"<t:Translate x="0" y="0"/><t:Translate x="1" y="1"/>"#,
    ] {
        let transformation = format!("<t:Transformation>{contents}</t:Transformation>");
        assert!(Metadata::parse(analytics(&frame(&transformation)).as_bytes()).is_err());
        assert!(Metadata::parse(object_xml(&transformation).as_bytes()).is_err());
    }
}

#[test]
fn arbitrary_prefixes_and_a_default_schema_namespace_are_equivalent() {
    let xml = document(&format!(
        r#"<t:VideoAnalytics>{}</t:VideoAnalytics><t:Event>{}</t:Event>"#,
        frame(r#"<t:Object ObjectId="1"><t:Appearance><t:HumanBody/></t:Appearance></t:Object>"#),
        notification("2026-09-05T12:00:00Z")
    ));
    let expected = Metadata::parse(xml.as_bytes()).unwrap();
    let renamed = xml
        .replace("xmlns:t=", "xmlns:scene=")
        .replace("<t:", "<scene:")
        .replace("</t:", "</scene:")
        .replace("xmlns:n=", "xmlns:notice=")
        .replace("<n:", "<notice:")
        .replace("</n:", "</notice:");
    assert_eq!(Metadata::parse(renamed.as_bytes()).unwrap(), expected);
    let default = xml
        .replace("xmlns:t=", "xmlns=")
        .replace("<t:", "<")
        .replace("</t:", "</");
    assert_eq!(Metadata::parse(default.as_bytes()).unwrap(), expected);
}

#[test]
fn structured_notification_text_and_canonical_attributes_survive() {
    let valid = notification("2026-09-05T12:00:00Z").replace(
        r#"<v:Details v:kind="opaque"><v:Value>private-detail</v:Value></v:Details>"#,
        r#"<v:Details v:kind="opaque" xml:space="preserve">
                <v:Value xmlns:v="urn:inner">  private text  </v:Value>
                <Plain xmlns="">unqualified</Plain>
            </v:Details>"#,
    );
    let xml = document(&format!("<t:Event>{valid}</t:Event>"));
    let mut expected = parse_notifications(xml.as_bytes()).unwrap();
    expected[0].data.element[0].value.attributes.swap(0, 1);
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert_eq!(metadata.notifications, expected);
    assert_eq!(metadata.invalid_messages, 0);
    for _ in 0..16 {
        assert_eq!(Metadata::parse(xml.as_bytes()).unwrap(), metadata);
    }
}

fn padded_document(byte_len: usize) -> String {
    let remaining = byte_len - document("").len() - 16 * 7;
    let mut contents = String::with_capacity(byte_len);
    for index in 0..16 {
        let length = remaining / 16 + usize::from(index < remaining % 16);
        contents.push_str("<!--");
        contents.push_str(&"X".repeat(length));
        contents.push_str("-->");
    }
    document(&contents)
}

#[test]
fn metadata_byte_limit_is_one_mib_inclusive() {
    let mut xml = padded_document(1024 * 1024);
    assert_eq!(xml.len(), 1024 * 1024);
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert!(metadata.frames.is_empty());
    assert!(metadata.notifications.is_empty());
    xml.push(' ');
    assert!(Metadata::parse(xml.as_bytes()).is_err());
}

#[test]
fn metadata_depth_and_node_limits_apply_to_ignored_content() {
    for depth in [31, 32] {
        let contents = format!(
            "{}{}",
            "<v:Layer>".repeat(depth),
            "</v:Layer>".repeat(depth)
        );
        assert_eq!(
            Metadata::parse(document(&contents).as_bytes()).is_ok(),
            depth == 31
        );
    }
    for count in [8191, 8192] {
        let xml = document(&"<v:Ignored/>".repeat(count));
        assert_eq!(Metadata::parse(xml.as_bytes()).is_ok(), count == 8191);
    }
}

#[test]
fn metadata_rejects_malformed_xml_gzip_and_excessive_scalars() {
    let valid = document("");
    for contents in [
        format!("<v:Ignored>{}</v:Ignored>", "X".repeat(65536)),
        format!(r#"<v:Ignored value="{}"/>"#, "X".repeat(4096)),
    ] {
        assert!(Metadata::parse(document(&contents).as_bytes()).is_ok());
    }
    let mut accepted = Vec::new();
    for (case, xml) in [
        ("multiple roots", format!("{valid}{valid}")),
        (
            "trailing payload",
            format!("{valid}private-trailing-payload"),
        ),
        (
            "document type",
            format!(r#"<!DOCTYPE t:MetadataStream [<!ENTITY payload "expansion">]>{valid}"#),
        ),
        (
            "text length",
            document(&format!("<v:Ignored>{}</v:Ignored>", "X".repeat(65537))),
        ),
        (
            "attribute length",
            document(&format!(r#"<v:Ignored value="{}"/>"#, "X".repeat(4097))),
        ),
        (
            "attribute count",
            document(&format!(
                "<v:Ignored {}/>",
                (0..33)
                    .map(|index| format!(r#"attribute{index}="0" "#))
                    .collect::<String>()
            )),
        ),
    ] {
        match Metadata::parse(xml.as_bytes()) {
            Ok(_) => accepted.push(case),
            Err(error) => assert!(!error.to_string().contains("private-trailing-payload")),
        }
    }
    assert!(accepted.is_empty(), "unexpectedly accepted: {accepted:?}");
    assert!(Metadata::parse(&[0x1f, 0x8b, 8, 0, 0]).is_err());
    assert!(Metadata::parse(b"<invalid>\xff</invalid>").is_err());
}

#[test]
fn notification_limit_counts_invalid_messages_across_all_event_streams() {
    let bad = notification("bad");
    let valid = notification("2026-09-05T12:00:00Z");
    let contents = format!(
        "<t:Event>{}</t:Event><t:Event>{}{valid}</t:Event>",
        bad.repeat(128),
        bad.repeat(127)
    );
    let metadata = Metadata::parse(document(&contents).as_bytes()).unwrap();
    assert_eq!(metadata.notifications.len(), 1);
    assert_eq!(metadata.invalid_messages, 255);
    let excessive = document(&format!("{contents}<t:Event>{bad}</t:Event>"));
    assert!(Metadata::parse(excessive.as_bytes()).is_err());
}

#[test]
fn an_oversized_notification_does_not_hide_valid_neighbors() {
    let item = format!(
        r#"<t:SimpleItem Name="Opaque" Value="{}"/>"#,
        "X".repeat(4090)
    );
    let large = notification("2026-09-05T12:00:00Z").replace(
        "<t:Data><t:ElementItem",
        &format!("<t:Data>{}<t:ElementItem", item.repeat(80)),
    );
    let valid = notification("2026-09-05T12:00:00Z");
    let xml = document(&format!("<t:Event>{valid}{large}{valid}</t:Event>"));
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert_eq!(metadata.notifications.len(), 2);
    assert_eq!(metadata.invalid_messages, 1);
}

#[test]
fn duplicate_known_fields_and_incomplete_candidates_are_rejected() {
    for appearance in [
        "<t:Class/><t:Class/>",
        "<t:HumanBody/><t:HumanBody/>",
        "<t:Shape/><t:Shape/>",
        "<t:Class><t:ClassCandidate><t:Type>Human</t:Type></t:ClassCandidate></t:Class>",
        "<t:Class><t:ClassCandidate><t:Likelihood>0.5</t:Likelihood></t:ClassCandidate></t:Class>",
        "<t:Class><t:ClassCandidate><t:Type>Human</t:Type><t:Type>Animal</t:Type>
            <t:Likelihood>0.5</t:Likelihood></t:ClassCandidate></t:Class>",
        "<t:Class><t:Type><t:Human/></t:Type></t:Class>",
        "<t:LicensePlateInfo><t:PlateNumber>ONE</t:PlateNumber>
            <t:PlateNumber>TWO</t:PlateNumber></t:LicensePlateInfo>",
    ] {
        assert!(Metadata::parse(object_xml(appearance).as_bytes()).is_err());
    }
}

#[test]
fn source_deleted_ids_and_class_lists_are_bounded() {
    for length in [256, 257] {
        let value = "X".repeat(length);
        for contents in [
            format!(r#"<t:Frame UtcTime="2026-09-05T12:00:00Z" Source="{value}"/>"#),
            frame(&format!(
                r#"<t:ObjectTree><t:Delete ObjectId="{value}"/></t:ObjectTree>"#
            )),
        ] {
            assert_eq!(
                Metadata::parse(analytics(&contents).as_bytes()).is_ok(),
                length == 256
            );
        }
    }
    for count in [32, 33] {
        let xml = object_xml(&format!(
            "<t:Class>{}</t:Class>",
            "<t:Type>Human</t:Type>".repeat(count)
        ));
        assert_eq!(Metadata::parse(xml.as_bytes()).is_ok(), count == 32);
    }
    for count in [128, 129] {
        let contents = format!(r#"<t:Object ObjectId="{}"/>"#, "\u{e9}".repeat(count));
        assert_eq!(
            Metadata::parse(analytics(&frame(&contents)).as_bytes()).is_ok(),
            count == 128
        );
    }
}

#[test]
fn deletions_are_attribute_only_and_removed_behaviour_is_not_a_delete() {
    for deletion in [
        r#"<t:Delete ObjectId="1"><t:ObjectId>2</t:ObjectId></t:Delete>"#,
        r#"<t:Delete ObjectId="1">2</t:Delete>"#,
    ] {
        let xml = analytics(&frame(&format!("<t:ObjectTree>{deletion}</t:ObjectTree>")));
        assert!(Metadata::parse(xml.as_bytes()).is_err());
    }
    let xml = analytics(&frame(
        r#"<t:Object ObjectId="1"><t:Behaviour><t:Removed/></t:Behaviour></t:Object>"#,
    ));
    let metadata = Metadata::parse(xml.as_bytes()).unwrap();
    assert_eq!(metadata.frames[0].objects.len(), 1);
    assert!(metadata.frames[0].deleted_ids.is_empty());
}
