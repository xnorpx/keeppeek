use onvif::event::{Endpoint, Metadata, Operation, ProtocolError, Pull, Subscription};
use xml::reader::{ParserConfig, XmlEvent};

fn subscription_xml(parameters: &str) -> String {
    format!(
        r#"<e:CreatePullPointSubscriptionResponse
            xmlns:e="http://www.onvif.org/ver10/events/wsdl"
            xmlns:a="http://www.w3.org/2005/08/addressing"
            xmlns:b="http://docs.oasis-open.org/wsn/b-2"
            xmlns:v="urn:camera" xmlns:w="urn:other">
            <e:SubscriptionReference>
                <a:Address>http://192.0.2.20/events/subscription</a:Address>
                <a:ReferenceParameters>{parameters}</a:ReferenceParameters>
            </e:SubscriptionReference>
            <b:CurrentTime>2020-01-01T00:00:00Z</b:CurrentTime>
            <b:TerminationTime>2020-01-01T00:01:30Z</b:TerminationTime>
        </e:CreatePullPointSubscriptionResponse>"#
    )
}

fn subscription_envelope(parameters: &str) -> String {
    let endpoint = Endpoint::new("http://192.0.2.20/events").unwrap();
    Subscription::parse(&endpoint, subscription_xml(parameters).as_bytes())
        .unwrap()
        .request(Operation::Unsubscribe)
        .unwrap()
        .envelope(None, "urn:uuid:xml-safety")
        .unwrap()
}

#[test]
fn subscription_round_trips_qualified_reference_attributes() {
    let envelope =
        subscription_envelope(r#"<v:Identifier v:mode="exact">opaque&amp;secret</v:Identifier>"#);
    let mut identifiers = 0;
    for event in ParserConfig::new().create_reader(envelope.as_bytes()) {
        let XmlEvent::StartElement {
            name, attributes, ..
        } = event.unwrap()
        else {
            continue;
        };
        if name.local_name != "Identifier" {
            continue;
        }
        identifiers += 1;
        assert_eq!(name.namespace.as_deref(), Some("urn:camera"));
        let mode = attributes
            .iter()
            .find(|attribute| attribute.name.local_name == "mode")
            .unwrap();
        assert_eq!(mode.name.namespace.as_deref(), Some("urn:camera"));
        assert_eq!(mode.name.prefix.as_deref(), Some("v"));
        assert_eq!(mode.value, "exact");
    }
    assert_eq!(identifiers, 1);
}

#[test]
fn subscription_preserves_nested_whitespace_and_distinct_attributes() {
    let envelope = subscription_envelope(
        "<v:Identifier v:mode=\"exact\" w:mode=\"other\"> \t<v:Part> \n </v:Part>\n</v:Identifier>",
    );
    let mut text = String::new();
    let mut inside = false;
    let mut modes = Vec::new();
    for event in ParserConfig::new().create_reader(envelope.as_bytes()) {
        match event.unwrap() {
            XmlEvent::StartElement {
                name, attributes, ..
            } if name.local_name == "Identifier" => {
                inside = true;
                modes = attributes
                    .into_iter()
                    .filter(|attribute| attribute.name.local_name == "mode")
                    .collect();
            }
            XmlEvent::EndElement { name } if name.local_name == "Identifier" => inside = false,
            XmlEvent::Characters(value) | XmlEvent::Whitespace(value) if inside => {
                text.push_str(&value);
            }
            _ => {}
        }
    }
    assert_eq!(text, " \t \n \n");
    assert_eq!(modes.len(), 2);
    assert!(
        modes.iter().any(
            |attribute| attribute.name.namespace.as_deref() == Some("urn:camera")
                && attribute.value == "exact"
        )
    );
    assert!(modes.iter().any(
        |attribute| attribute.name.namespace.as_deref() == Some("urn:other")
            && attribute.value == "other"
    ));
}

fn namespace_document(count: usize) -> String {
    let value = format!("urn:namespace-private-{}", "x".repeat(4096 - 22));
    assert_eq!(value.len(), 4096);
    let declarations = (0..32)
        .map(|index| format!(r#" xmlns:p{index}="{value}""#))
        .collect::<String>();
    format!(
        r#"<e:PullMessagesResponse xmlns:e="http://www.onvif.org/ver10/events/wsdl"{declarations}>
            <e:CurrentTime>2020-01-01T00:00:00Z</e:CurrentTime>
            <e:TerminationTime>2020-01-01T00:01:30Z</e:TerminationTime>
            {}</e:PullMessagesResponse>"#,
        "<ignored/>".repeat(count)
    )
}

#[test]
fn pull_and_metadata_reject_aggregate_namespace_amplification() {
    assert!(
        Pull::parse(namespace_document(0).as_bytes())
            .unwrap()
            .notifications
            .is_empty()
    );
    for count in [32, 8000] {
        let document = namespace_document(count);
        assert!(document.len() <= 256 * 1024);
        let error = Pull::parse(document.as_bytes()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "ONVIF event protocol: XML namespace storage limit exceeded"
        );
        assert_payload_safe(error);
        let document = document
            .replace("PullMessagesResponse", "MetadataStream")
            .replace(
                "http://www.onvif.org/ver10/events/wsdl",
                "http://www.onvif.org/ver10/schema",
            );
        let error = Metadata::parse(document.as_bytes()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "ONVIF event protocol: XML namespace storage limit exceeded"
        );
        assert_payload_safe(error);
    }
}

#[test]
fn subscription_preserves_nested_namespace_scopes_and_resets() {
    let envelope = subscription_envelope(
        r#"<v:Identifier xmlns="urn:default">
            <v:Nested xmlns:v="urn:nested" v:mode="inner">
                <v:Reset xmlns:v="urn:camera" v:mode="outer"/>
                <Unqualified xmlns=""/>
            </v:Nested>
            <v:Sibling v:mode="sibling"/>
        </v:Identifier>"#,
    );
    let mut scopes = Vec::new();
    for event in ParserConfig::new().create_reader(envelope.as_bytes()) {
        let XmlEvent::StartElement {
            name,
            attributes,
            namespace,
        } = event.unwrap()
        else {
            continue;
        };
        if !matches!(
            name.local_name.as_str(),
            "Nested" | "Reset" | "Unqualified" | "Sibling"
        ) {
            continue;
        }
        for attribute in &attributes {
            assert_eq!(attribute.name.prefix.as_deref(), Some("v"));
            assert_eq!(attribute.name.namespace, name.namespace);
        }
        scopes.push((
            name.local_name,
            name.namespace,
            namespace.get("v").unwrap().to_owned(),
        ));
    }
    assert_eq!(
        scopes,
        [
            (
                "Nested".to_owned(),
                Some("urn:nested".to_owned()),
                "urn:nested".to_owned()
            ),
            (
                "Reset".to_owned(),
                Some("urn:camera".to_owned()),
                "urn:camera".to_owned()
            ),
            ("Unqualified".to_owned(), None, "urn:nested".to_owned()),
            (
                "Sibling".to_owned(),
                Some("urn:camera".to_owned()),
                "urn:camera".to_owned()
            ),
        ]
    );
}

#[test]
fn subscription_rejects_duplicate_addressing_reference_markers() {
    let endpoint = Endpoint::new("http://192.0.2.20/events").unwrap();
    for attributes in [
        r#"a:IsReferenceParameter="false""#,
        r#"xmlns:kpwsa="http://www.w3.org/2005/08/addressing" kpwsa:IsReferenceParameter="private-marker""#,
    ] {
        let document = subscription_xml(&format!(
            "<v:Identifier {attributes}>opaque-private</v:Identifier>"
        ));
        let error = Subscription::parse(&endpoint, document.as_bytes()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "ONVIF event protocol: conflicting reference parameter attribute"
        );
        assert!(!format!("{error:?}").contains("private"));
    }
}

fn notification_xml(items: &str) -> String {
    format!(
        r#"<n:NotificationMessage>
            <n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">q:VideoSource/MotionAlarm</n:Topic>
            <n:Message><t:Message UtcTime="2020-01-01T00:00:00Z" PropertyOperation="Changed">
                <t:Data>{items}</t:Data>
            </t:Message></n:Message>
        </n:NotificationMessage>"#
    )
}

fn event_document(root: &str, contents: &str) -> String {
    format!(
        r#"<{root} xmlns:e="http://www.onvif.org/ver10/events/wsdl"
            xmlns:n="http://docs.oasis-open.org/wsn/b-2"
            xmlns:t="http://www.onvif.org/ver10/schema"
            xmlns:q="http://www.onvif.org/ver10/topics"
            xmlns:v="urn:camera" xmlns:w="urn:other">{contents}</{root}>"#
    )
}

fn pull_xml(contents: &str) -> String {
    event_document(
        "e:PullMessagesResponse",
        &format!(
            "<e:CurrentTime>2020-01-01T00:00:00Z</e:CurrentTime><e:TerminationTime>2020-01-01T00:01:30Z</e:TerminationTime>{contents}"
        ),
    )
}

fn metadata_xml(contents: &str) -> String {
    event_document(
        "t:MetadataStream",
        &format!("<t:Event>{contents}</t:Event>"),
    )
}

#[test]
fn pull_and_metadata_do_not_promote_qualified_simple_item_fields() {
    let valid = notification_xml(r#"<t:SimpleItem Name="State" Value="false"/>"#);
    for attributes in [
        r#"Name="State" v:Value="true""#,
        r#"v:Name="State" Value="true""#,
    ] {
        let invalid = notification_xml(&format!("<t:SimpleItem {attributes}/>"));
        let contents = format!("{invalid}{valid}{invalid}");
        let pull = Pull::parse(pull_xml(&contents).as_bytes()).unwrap();
        let metadata = Metadata::parse(metadata_xml(&contents).as_bytes()).unwrap();
        assert_eq!(pull.invalid_messages, 2);
        assert_eq!(metadata.invalid_messages, 2);
        for notifications in [&pull.notifications, &metadata.notifications] {
            assert_eq!(notifications.len(), 1);
            assert_eq!(notifications[0].data.simple[0].name, "State");
            assert_eq!(notifications[0].data.simple[0].value, "false");
        }
    }
}

#[test]
fn pull_and_metadata_preserve_opaque_attributes_and_normalized_text() {
    let contents = notification_xml(
        "<t:ElementItem Name=\"Details\"><v:Details v:mode=\"exact\" w:mode=\"other\"> \t<v:Part> \n </v:Part>\n<![CDATA[<&opaque-private>]]></v:Details></t:ElementItem>",
    );
    let pull = Pull::parse(pull_xml(&contents).as_bytes()).unwrap();
    let metadata = Metadata::parse(metadata_xml(&contents).as_bytes()).unwrap();
    assert_eq!(pull.invalid_messages, 0);
    assert_eq!(metadata.invalid_messages, 0);
    for notifications in [&pull.notifications, &metadata.notifications] {
        assert_eq!(notifications.len(), 1);
        let details = &notifications[0].data.element[0].value;
        assert_eq!(details.name.namespace_uri.as_deref(), Some("urn:camera"));
        assert_eq!(details.text, "<&opaque-private>");
        assert_eq!(details.children.len(), 1);
        assert_eq!(details.children[0].text, "");
        let mut attributes: Vec<_> = details
            .attributes
            .iter()
            .map(|attribute| {
                (
                    attribute.name.namespace_uri.as_deref(),
                    attribute.name.local_name.as_str(),
                    attribute.value.as_str(),
                )
            })
            .collect();
        attributes.sort_unstable();
        assert_eq!(
            attributes,
            [
                (Some("urn:camera"), "mode", "exact"),
                (Some("urn:other"), "mode", "other")
            ]
        );
    }
    assert!(!format!("{metadata:?}").contains("opaque-private"));
}

#[test]
fn pull_and_metadata_preserve_independent_topic_scopes() {
    let notification = notification_xml(r#"<t:SimpleItem Name="State" Value="true"/>"#);
    let first = notification.replace("<n:Topic ", "<n:Topic xmlns:q=\"urn:first\" ");
    let second = notification.replace(
        "<n:NotificationMessage>",
        "<n:NotificationMessage xmlns:q=\"urn:second\">",
    );
    let contents = format!("{first}{second}{notification}");
    let pull = Pull::parse(pull_xml(&contents).as_bytes()).unwrap();
    let metadata = Metadata::parse(metadata_xml(&contents).as_bytes()).unwrap();
    for notifications in [&pull.notifications, &metadata.notifications] {
        let namespaces: Vec<_> = notifications
            .iter()
            .map(|message| message.topic.path[0].namespace_uri.as_deref())
            .collect();
        assert_eq!(
            namespaces,
            [
                Some("urn:first"),
                Some("urn:second"),
                Some("http://www.onvif.org/ver10/topics")
            ]
        );
    }
}

#[test]
fn subscription_annotation_preserves_vendor_and_unqualified_markers() {
    let envelope = subscription_envelope(
        r#"<v:Identifier IsReferenceParameter="plain" v:IsReferenceParameter="vendor">
            <v:Nested xmlns:kpwsa="urn:inner" kpwsa:mode="inner"/>
        </v:Identifier>"#,
    );
    let mut markers = Vec::new();
    let mut nested = 0;
    for event in ParserConfig::new().create_reader(envelope.as_bytes()) {
        let XmlEvent::StartElement {
            name, attributes, ..
        } = event.unwrap()
        else {
            continue;
        };
        if name.local_name == "Identifier" {
            markers = attributes
                .into_iter()
                .map(|attribute| (attribute.name.namespace, attribute.value))
                .collect();
        } else if name.local_name == "Nested" {
            nested += 1;
            assert_eq!(attributes[0].name.namespace.as_deref(), Some("urn:inner"));
            assert_eq!(attributes[0].value, "inner");
        }
    }
    markers.sort_unstable();
    assert_eq!(
        markers,
        [
            (None, "plain".to_owned()),
            (
                Some("http://www.w3.org/2005/08/addressing".to_owned()),
                "true".to_owned()
            ),
            (Some("urn:camera".to_owned()), "vendor".to_owned()),
        ]
    );
    assert_eq!(nested, 1);
}

fn assert_payload_safe(error: ProtocolError) {
    assert!(error.to_string().starts_with("ONVIF event protocol: "));
    assert!(!error.to_string().contains("private"));
    assert!(!format!("{error:?}").contains("private"));
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn subscription_rejects_ambiguous_attributes_and_namespace_collisions_safely() {
    let endpoint = Endpoint::new("http://192.0.2.20/events").unwrap();
    for parameter in [
        r#"<v:Identifier xmlns:w="urn:camera" v:mode="private-first" w:mode="private-second"/>"#,
        r#"<v:Identifier v:mode="private-first" v:mode="private-second"/>"#,
        r#"<v:Identifier xmlns:kpwsa="urn:private" kpwsa:mode="private-mode"/>"#,
        r#"<v:Identifier missing:mode="private-mode"/>"#,
        r#"<v:Identifier>&private-entity;</v:Identifier>"#,
    ] {
        assert_payload_safe(
            Subscription::parse(&endpoint, subscription_xml(parameter).as_bytes()).unwrap_err(),
        );
    }
}

#[test]
fn malformed_xml_and_document_types_return_payload_safe_errors() {
    let endpoint = Endpoint::new("http://192.0.2.20/events").unwrap();
    for document in [
        "<private-payload>",
        "<!DOCTYPE private-payload [<!ENTITY private-entity 'private-value'>]><private-payload>&private-entity;</private-payload>",
        "<!DOCTYPE private-payload SYSTEM 'file:///private-payload'><private-payload/>",
    ] {
        assert_payload_safe(Subscription::parse(&endpoint, document.as_bytes()).unwrap_err());
        assert_payload_safe(Pull::parse(document.as_bytes()).unwrap_err());
        assert_payload_safe(Metadata::parse(document.as_bytes()).unwrap_err());
    }
}

#[test]
fn pull_bounds_escaped_serialization_before_accepting_notifications() {
    for count in [3, 4] {
        let contents = notification_xml(&format!(
            "<t:ElementItem Name=\"Large\"><v:Value>{}</v:Value></t:ElementItem>",
            format!("<v:Part>{}</v:Part>", ">".repeat(20 * 1024)).repeat(count)
        ));
        let document = pull_xml(&contents);
        assert!(document.len() < 256 * 1024);
        let parsed = Pull::parse(document.as_bytes());
        if count == 3 {
            assert_eq!(parsed.unwrap().notifications.len(), 1);
        } else {
            let error = parsed.unwrap_err();
            assert_eq!(
                error.to_string(),
                "ONVIF event protocol: serialized XML exceeds byte limit"
            );
            assert_payload_safe(error);
        }
    }
}

#[test]
fn subscription_keeps_mixed_content_in_original_order() {
    let envelope = subscription_envelope(
        "<v:Identifier v:mode=\"exact\">opaque&amp;secret \t<v:Part> \n </v:Part>\n<![CDATA[<&literal>]]><!--note--><?keep value?> tail</v:Identifier>",
    );
    let mut inside = false;
    let mut contents = Vec::new();
    let reader = ParserConfig::new()
        .ignore_comments(false)
        .cdata_to_characters(false)
        .create_reader(envelope.as_bytes());
    for event in reader {
        let event = event.unwrap();
        match &event {
            XmlEvent::StartElement { name, .. } if name.local_name == "Identifier" => {
                inside = true;
                continue;
            }
            XmlEvent::EndElement { name } if name.local_name == "Identifier" => {
                inside = false;
                continue;
            }
            _ => {}
        }
        if !inside {
            continue;
        }
        contents.push(match event {
            XmlEvent::StartElement { name, .. } => format!("start:{}", name.local_name),
            XmlEvent::EndElement { name } => format!("end:{}", name.local_name),
            XmlEvent::Characters(text) | XmlEvent::Whitespace(text) => format!("text:{text}"),
            XmlEvent::CData(text) => format!("cdata:{text}"),
            XmlEvent::Comment(text) => format!("comment:{text}"),
            XmlEvent::ProcessingInstruction { name, data } => {
                format!("pi:{name}:{}", data.unwrap())
            }
            _ => panic!("unexpected event inside reference parameter"),
        });
    }
    assert_eq!(
        contents,
        [
            "text:opaque&secret \t",
            "start:Part",
            "text: \n ",
            "end:Part",
            "text:\n",
            "cdata:<&literal>",
            "comment:note",
            "pi:keep:value",
            "text: tail",
        ]
    );
}

#[test]
fn subscription_bounds_namespace_declaration_lengths() {
    let endpoint = Endpoint::new("http://192.0.2.20/events").unwrap();
    for length in [4096, 4097] {
        let namespace = format!("urn:private:{}", "x".repeat(length - 12));
        assert_eq!(namespace.len(), length);
        let document = subscription_xml(&format!("<v:Identifier xmlns:v=\"{namespace}\"/>"));
        let parsed = Subscription::parse(&endpoint, document.as_bytes());
        if length == 4096 {
            parsed.unwrap();
        } else {
            assert_payload_safe(parsed.unwrap_err());
        }
    }
}

#[test]
fn subscription_does_not_repeat_inherited_declarations_inside_parameter_budget() {
    let endpoint = Endpoint::new("http://192.0.2.20/events").unwrap();
    let namespace = format!("urn:private:{}", "x".repeat(2048));
    let parameters = r#"<v:Identifier v:binding="w:exact">opaque</v:Identifier>"#.repeat(16);
    let document = subscription_xml(&parameters)
        .replace("xmlns:w=\"urn:other\"", &format!("xmlns:w=\"{namespace}\""));
    assert!(document.len() < 16 * 1024);
    let subscription = Subscription::parse(&endpoint, document.as_bytes()).unwrap();
    let envelope = subscription
        .request(Operation::Unsubscribe)
        .unwrap()
        .envelope(None, "urn:uuid:namespace-scope")
        .unwrap();
    let mut identifiers = 0;
    for event in ParserConfig::new().create_reader(envelope.as_bytes()) {
        let XmlEvent::StartElement {
            name,
            namespace: scope,
            ..
        } = event.unwrap()
        else {
            continue;
        };
        if name.local_name == "Identifier" {
            identifiers += 1;
            assert_eq!(scope.get("w"), Some(namespace.as_str()));
        }
    }
    assert_eq!(identifiers, 16);
}

#[test]
fn pull_bounds_depth_and_all_retained_nodes() {
    let document = pull_xml(&format!(
        "{}{}",
        "<ignored>".repeat(31),
        "</ignored>".repeat(31)
    ));
    assert!(
        Pull::parse(document.as_bytes())
            .unwrap()
            .notifications
            .is_empty()
    );
    let document = pull_xml(&"<!--private-->".repeat(8000));
    assert!(
        Pull::parse(document.as_bytes())
            .unwrap()
            .notifications
            .is_empty()
    );
    for contents in [
        format!("{}{}", "<ignored>".repeat(32), "</ignored>".repeat(32)),
        "<!--private-->".repeat(8192),
    ] {
        let error = Pull::parse(pull_xml(&contents).as_bytes()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "ONVIF event protocol: XML structure limit exceeded"
        );
        assert_payload_safe(error);
    }
}

#[test]
fn pull_accepts_the_byte_limit_and_rejects_one_extra_byte() {
    let chunk = format!("<ignored>{}</ignored>", "x".repeat(60 * 1024));
    let mut contents = chunk.repeat(4);
    let remaining = 256 * 1024 - pull_xml(&format!("{contents}<ignored></ignored>")).len();
    contents.push_str(&format!("<ignored>{}</ignored>", "x".repeat(remaining)));
    let mut document = pull_xml(&contents);
    assert_eq!(document.len(), 256 * 1024);
    assert!(
        Pull::parse(document.as_bytes())
            .unwrap()
            .notifications
            .is_empty()
    );
    document.push(' ');
    let error = Pull::parse(document.as_bytes()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "ONVIF event protocol: XML byte limit exceeded"
    );
    assert_payload_safe(error);
}

#[test]
fn pull_rejects_ambiguous_framing_and_ignores_nested_notifications() {
    let response = pull_xml("");
    for body in [
        format!("<s:Body>{response}{response}</s:Body>"),
        format!("<s:Body>{response}</s:Body><s:Body>{response}</s:Body>"),
    ] {
        let document = format!(
            "<s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\">{body}</s:Envelope>"
        );
        assert_payload_safe(Pull::parse(document.as_bytes()).unwrap_err());
    }
    assert_payload_safe(Pull::parse(format!("{response}<private/>").as_bytes()).unwrap_err());
    let contents = format!(
        "<v:Extension>{}</v:Extension>",
        notification_xml(r#"<t:SimpleItem Name="State" Value="true"/>"#)
    );
    let pull = Pull::parse(pull_xml(&contents).as_bytes()).unwrap();
    assert!(pull.notifications.is_empty());
    assert_eq!(pull.invalid_messages, 0);
}
