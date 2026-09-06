use xml::name::OwnedName;
use xml::reader::{ParserConfig, XmlEvent};

use super::{CapturedRequest, Shared, subscriptions, wire::BODY_BYTES_MAX};
use crate::Reply;

pub(super) const SOAP: &str = "http://www.w3.org/2003/05/soap-envelope";
pub(super) const DEVICE: &str = "http://www.onvif.org/ver10/device/wsdl";
pub(super) const EVENTS: &str = "http://www.onvif.org/ver10/events/wsdl";
pub(super) const WSNT: &str = "http://docs.oasis-open.org/wsn/b-2";
pub(super) const VENDOR: &str = "urn:test-hikvision:onvif";

pub(super) struct Element {
    name: OwnedName,
    pub(super) text: String,
    pub(super) children: Vec<Self>,
}

impl Element {
    pub(super) fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        anyhow::ensure!(
            bytes.len() <= BODY_BYTES_MAX,
            "fake ONVIF XML byte limit exceeded"
        );
        let parser = ParserConfig::new()
            .max_name_length(256)
            .max_attributes(32)
            .max_attribute_length(4096)
            .max_data_length(BODY_BYTES_MAX)
            .max_entity_expansion_length(0)
            .max_entity_expansion_depth(0)
            .allow_multiple_root_elements(false)
            .create_reader(bytes);
        let mut stack: Vec<Self> = Vec::with_capacity(16);
        let mut root = None;
        let mut nodes = 0;
        for (count, event) in parser.into_iter().enumerate() {
            anyhow::ensure!(count < 16_384, "fake ONVIF XML event limit exceeded");
            match event? {
                XmlEvent::StartElement { name, .. } => {
                    anyhow::ensure!(
                        stack.len() < 16 && nodes < 4096,
                        "fake ONVIF XML tree limit exceeded"
                    );
                    nodes += 1;
                    stack.push(Self {
                        name,
                        text: String::new(),
                        children: Vec::new(),
                    });
                }
                XmlEvent::Characters(text) | XmlEvent::CData(text) | XmlEvent::Whitespace(text) => {
                    if let Some(node) = stack.last_mut() {
                        node.text.push_str(&text);
                    }
                }
                XmlEvent::EndElement { .. } => {
                    let node = stack.pop().expect("XML parser pairs element boundaries");
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    } else {
                        anyhow::ensure!(
                            root.replace(node).is_none(),
                            "multiple fake ONVIF XML roots"
                        );
                    }
                }
                _ => {}
            }
        }
        root.ok_or_else(|| anyhow::anyhow!("missing fake ONVIF XML root"))
    }

    pub(super) fn is(&self, namespace: &str, local_name: &str) -> bool {
        self.name.namespace.as_deref() == Some(namespace) && self.name.local_name == local_name
    }

    pub(super) fn child(&self, namespace: &str, local_name: &str) -> anyhow::Result<&Self> {
        let mut children = self
            .children
            .iter()
            .filter(|child| child.is(namespace, local_name));
        let child = children
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing fake ONVIF XML field"))?;
        anyhow::ensure!(children.next().is_none(), "duplicate fake ONVIF XML field");
        Ok(child)
    }

    pub(super) fn operation(&self) -> anyhow::Result<&Self> {
        anyhow::ensure!(self.is(SOAP, "Envelope"), "invalid SOAP envelope namespace");
        let body = self.child(SOAP, "Body")?;
        anyhow::ensure!(
            body.children.len() == 1 && body.text.trim().is_empty(),
            "invalid SOAP body"
        );
        anyhow::ensure!(
            self.children
                .iter()
                .filter(|child| child.is(SOAP, "Header"))
                .count()
                <= 1,
            "duplicate SOAP header"
        );
        Ok(&body.children[0])
    }
}

pub(super) fn route(request: &CapturedRequest, shared: &Shared) -> Reply {
    let Ok(document) = Element::parse(&request.body) else {
        return fault("ter:InvalidArgVal", "Invalid SOAP request", "");
    };
    let Ok(operation) = document.operation() else {
        return fault("ter:InvalidArgVal", "Invalid SOAP request", "");
    };
    if request.target == "/onvif/device_service" && operation.is(DEVICE, "GetServices") {
        return services(shared);
    }
    if request.target == "/onvif/events_service" {
        if operation.is(EVENTS, "GetServiceCapabilities") {
            return capabilities();
        }
        if operation.is(EVENTS, "GetEventProperties") {
            return properties();
        }
        if operation.is(EVENTS, "CreatePullPointSubscription") {
            return subscriptions::create(operation, shared);
        }
    }
    if request.target.starts_with("/onvif/subscription?") {
        return subscriptions::route(&document, operation, &request.target, shared);
    }
    fault(
        "ter:ActionNotSupported",
        "Unsupported fixture operation",
        "",
    )
}

pub(super) fn envelope(body: &str) -> String {
    format!(
        r#"<s:Envelope xmlns:s="{SOAP}" xmlns:tds="{DEVICE}" xmlns:tev="{EVENTS}"
        xmlns:tt="http://www.onvif.org/ver10/schema" xmlns:ter="http://www.onvif.org/ver10/error"
        xmlns:wsnt="{WSNT}" xmlns:v="{VENDOR}" xmlns:wsa="http://www.w3.org/2005/08/addressing"
        xmlns:wsrf-r="http://docs.oasis-open.org/wsrf/r-2" xmlns:wstop="http://docs.oasis-open.org/wsn/t-1"
        xmlns:tns1="http://www.onvif.org/ver10/topics" xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <s:Body>{body}</s:Body></s:Envelope>"#
    )
}

pub(super) fn reply(body: &str) -> Reply {
    Reply::http(200, "application/soap+xml; charset=utf-8", envelope(body))
}

pub(super) fn fault(code: &str, reason: &str, detail: &str) -> Reply {
    let code = xml::escape::escape_str_pcdata(code);
    let reason = xml::escape::escape_str_pcdata(reason);
    let body = envelope(&format!(
        r#"<s:Fault><s:Code><s:Value>s:Sender</s:Value>
        <s:Subcode><s:Value>{code}</s:Value></s:Subcode></s:Code>
        <s:Reason><s:Text xml:lang="en">{reason}</s:Text></s:Reason>
        <s:Detail>{detail}</s:Detail></s:Fault>"#
    ));
    Reply::http(500, "application/soap+xml; charset=utf-8", body)
}

fn services(shared: &Shared) -> Reply {
    let address = shared.address;
    reply(&format!(
        r#"<tds:GetServicesResponse>
        <tds:Service><tds:Namespace>{EVENTS}</tds:Namespace>
        <tds:XAddr>http://{address}/onvif/events_service</tds:XAddr>
        <tds:Version><tt:Major>2</tt:Major><tt:Minor>0</tt:Minor></tds:Version></tds:Service>
        <tds:Service><tds:Namespace>http://www.onvif.org/ver10/media/wsdl</tds:Namespace>
        <tds:XAddr>http://{address}/onvif/media_service</tds:XAddr>
        <tds:Version><tt:Major>2</tt:Major><tt:Minor>0</tt:Minor></tds:Version></tds:Service>
        </tds:GetServicesResponse>"#
    ))
}

fn capabilities() -> Reply {
    reply(
        r#"<tev:GetServiceCapabilitiesResponse><tev:Capabilities
        WSSubscriptionPolicySupport="false" WSPausableSubscriptionManagerInterfaceSupport="false"
        MaxNotificationProducers="1" MaxPullPoints="1" PersistentNotificationStorage="false"/>
        </tev:GetServiceCapabilitiesResponse>"#,
    )
}

fn properties() -> Reply {
    let motion_alarm = property("MotionAlarm", "State");
    let tamper = property("Tamper", "State");
    let motion = property("Motion", "IsMotion");
    let person = property("Person", "State");
    let vehicle = property("Vehicle", "State");
    reply(&format!(
        r#"<tev:GetEventPropertiesResponse>
        <tev:TopicNamespaceLocation>http://www.onvif.org/onvif/ver10/topics/topicns.xml</tev:TopicNamespaceLocation>
        <wsnt:FixedTopicSet>true</wsnt:FixedTopicSet><wstop:TopicSet>
        <tns1:VideoSource>{motion_alarm}{tamper}</tns1:VideoSource>
        <tns1:RuleEngine><tns1:CellMotionDetector>{motion}</tns1:CellMotionDetector>
        <tns1:ObjectDetector>{person}{vehicle}</tns1:ObjectDetector></tns1:RuleEngine></wstop:TopicSet>
        <wsnt:TopicExpressionDialect>http://docs.oasis-open.org/wsn/t-1/TopicExpression/Concrete</wsnt:TopicExpressionDialect>
        <wsnt:TopicExpressionDialect>http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet</wsnt:TopicExpressionDialect>
        <tev:MessageContentFilterDialect/>
        <tev:MessageContentSchemaLocation>http://www.onvif.org/ver10/schema/onvif.xsd</tev:MessageContentSchemaLocation>
        </tev:GetEventPropertiesResponse>"#
    ))
}

fn property(name: &str, data_name: &str) -> String {
    format!(
        r#"<tns1:{name} wstop:topic="true"><tt:MessageDescription IsProperty="true">
        <tt:Source><tt:SimpleItemDescription Name="VideoSourceConfigurationToken" Type="tt:ReferenceToken"/></tt:Source>
        <tt:Data><tt:SimpleItemDescription Name="{data_name}" Type="xs:boolean"/></tt:Data>
        </tt:MessageDescription></tns1:{name}>"#
    )
}
