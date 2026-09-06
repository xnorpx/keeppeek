use std::time::Duration;

use digest_auth::{AuthContext, WwwAuthenticateHeader};
use xml::reader::{EventReader, XmlEvent};

use super::super::FakeOnvif;

pub(super) const DEVICE: &str = "/onvif/device_service";
pub(super) const EVENTS: &str = "/onvif/events_service";
pub(super) const GET_SERVICES: &str =
    "<tds:GetServices><tds:IncludeCapability>false</tds:IncludeCapability></tds:GetServices>";

pub(super) struct Client {
    origin: String,
    agent: ureq::Agent,
    challenge: WwwAuthenticateHeader,
    cnonce: String,
}

impl Client {
    pub(super) fn new(fake: &FakeOnvif) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(5)))
            .max_redirects(0)
            .build()
            .new_agent();
        let response = agent
            .post(format!("{}{DEVICE}", fake.origin()))
            .send("")
            .unwrap();
        assert_eq!(response.status().as_u16(), 401);
        let mut challenge = digest_auth::parse(
            response
                .headers()
                .get("www-authenticate")
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        let cnonce = challenge
            .respond(&AuthContext::new("test", "test", DEVICE))
            .unwrap()
            .cnonce
            .unwrap();
        Self {
            origin: fake.origin(),
            agent,
            challenge,
            cnonce,
        }
    }

    pub(super) fn authorization(&mut self, target: &str, body: &str) -> String {
        let mut context = AuthContext::new_post("test", "test", target, Some(body.as_bytes()));
        context.set_custom_cnonce(self.cnonce.as_str());
        self.challenge.respond(&context).unwrap().to_string()
    }

    pub(super) fn send(&self, target: &str, body: &str, auth: Option<&str>) -> (u16, String) {
        self.try_send(target, body, auth).unwrap()
    }

    pub(super) fn try_send(
        &self,
        target: &str,
        body: &str,
        auth: Option<&str>,
    ) -> Result<(u16, String), ureq::Error> {
        let request = self
            .agent
            .post(format!("{}{target}", self.origin))
            .header("Content-Type", "application/soap+xml; charset=utf-8");
        let request = if let Some(auth) = auth {
            request.header("Authorization", auth)
        } else {
            request
        };
        let mut response = request.send(body)?;
        let status = response.status().as_u16();
        Ok((status, response.body_mut().read_to_string()?))
    }

    pub(super) fn post(&mut self, target: &str, body: &str) -> (u16, String) {
        let auth = self.authorization(target, body);
        self.send(target, body, Some(&auth))
    }
}

pub(super) fn envelope(operation: &str, header: &str) -> String {
    format!(
        r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
        xmlns:tds="http://www.onvif.org/ver10/device/wsdl"
        xmlns:tev="http://www.onvif.org/ver10/events/wsdl"
        xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"
        xmlns:wsa="http://www.w3.org/2005/08/addressing">
        <s:Header>{header}</s:Header><s:Body>{operation}</s:Body></s:Envelope>"#
    )
}

pub(super) fn texts(body: &str, namespace: &str, local_name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = None;
    for event in EventReader::from_str(body) {
        match event.unwrap() {
            XmlEvent::StartElement { name, .. }
                if name.namespace.as_deref() == Some(namespace)
                    && name.local_name == local_name =>
            {
                current = Some(String::new());
            }
            XmlEvent::Characters(text) | XmlEvent::CData(text) => {
                if let Some(current) = &mut current {
                    current.push_str(&text);
                }
            }
            XmlEvent::EndElement { name }
                if name.namespace.as_deref() == Some(namespace)
                    && name.local_name == local_name =>
            {
                found.push(current.take().unwrap());
            }
            _ => {}
        }
    }
    found
}

#[test]
fn discovery_requires_digest_and_preserves_authority() {
    let fake = FakeOnvif::builder()
        .credentials("test", "test")
        .start()
        .unwrap();
    assert!(fake.address().ip().is_loopback());
    assert_eq!(fake.events_endpoint(), format!("{}{EVENTS}", fake.origin()));
    let mut client = Client::new(&fake);
    let body = envelope(GET_SERVICES, "");
    let (status, response) = client.post(DEVICE, &body);
    assert_eq!(status, 200);
    let namespace = "http://www.onvif.org/ver10/device/wsdl";
    let addresses = texts(&response, namespace, "XAddr");
    assert_eq!(addresses.len(), 2);
    assert!(addresses.contains(&fake.events_endpoint()));
    for address in addresses {
        let address = url::Url::parse(&address).unwrap();
        assert_eq!(
            address.origin(),
            url::Url::parse(&fake.origin()).unwrap().origin()
        );
    }
    assert_eq!(
        texts(&response, namespace, "Namespace"),
        [
            "http://www.onvif.org/ver10/events/wsdl",
            "http://www.onvif.org/ver10/media/wsdl",
        ]
    );
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].authenticated());
    assert!(requests[1].authenticated());
    assert_eq!(requests[1].method(), "POST");
    assert_eq!(requests[1].target(), DEVICE);
    assert_eq!(requests[1].body(), body.as_bytes());
}

#[test]
fn forged_digest_and_target_mismatch_are_rejected_without_leaking_captures() {
    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    let body = envelope(GET_SERVICES, "");
    let auth = client.authorization("/onvif/device_service?wrong=secret", &body);
    assert_eq!(client.send(DEVICE, &body, Some(&auth)).0, 401);
    let valid = client.authorization(DEVICE, &body);
    let mut forged = digest_auth::AuthorizationHeader::parse(&valid).unwrap();
    forged.response = "00000000000000000000000000000000".to_owned();
    assert_eq!(client.send(DEVICE, &body, Some(&forged.to_string())).0, 401);
    assert_eq!(client.send(DEVICE, &body, Some(&valid)).0, 200);
    assert_eq!(client.send(DEVICE, &body, Some(&valid)).0, 401);
    let debug = format!(
        "{:?} {:?} {:?}",
        fake,
        fake.requests(),
        FakeOnvif::builder().credentials("secret-user", "secret-password")
    );
    for secret in ["secret", "Digest", "GetServices", DEVICE] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn an_idle_accepted_socket_waits_for_its_request() {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let fake = FakeOnvif::builder().start().unwrap();
    let mut socket = TcpStream::connect(fake.address()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (state, _) = fake
        .shared
        .changed
        .wait_timeout_while(
            fake.shared.state.lock().unwrap(),
            Duration::from_millis(100),
            |state| state.transport_errors == 0,
        )
        .unwrap();
    let errors = state.transport_errors;
    let sockets = state.sockets.len();
    drop(state);
    assert_eq!(errors, 0);
    assert_eq!(sockets, 1);
    socket
        .write_all(
            b"POST /onvif/device_service HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 401"));
}

#[test]
fn event_discovery_advertises_one_pull_point_and_classified_topics() {
    use super::lifecycle::{EVENTS_NS, WSNT, assert_root};

    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    let (status, body) = client.post(EVENTS, &envelope("<tev:GetServiceCapabilities/>", ""));
    assert_eq!(status, 200);
    assert_root(&body, EVENTS_NS, "GetServiceCapabilitiesResponse");
    assert!(EventReader::from_str(&body).into_iter().any(|event| matches!(event.unwrap(),
        XmlEvent::StartElement { name, attributes, .. } if name.namespace.as_deref() == Some(EVENTS_NS)
            && name.local_name == "Capabilities"
            && attributes.iter().any(|attribute| attribute.name.local_name == "MaxPullPoints" && attribute.value == "1"))));
    let (status, body) = client.post(EVENTS, &envelope("<tev:GetEventProperties/>", ""));
    assert_eq!(status, 200);
    assert_root(&body, EVENTS_NS, "GetEventPropertiesResponse");
    assert_eq!(texts(&body, WSNT, "FixedTopicSet"), ["true"]);
    assert!(
        texts(&body, WSNT, "TopicExpressionDialect")
            .contains(&"http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet".to_owned())
    );
    let mut topics = Vec::new();
    for event in EventReader::from_str(&body) {
        if let XmlEvent::StartElement { name, .. } = event.unwrap()
            && name.namespace.as_deref() == Some("http://www.onvif.org/ver10/topics")
        {
            topics.push(name.local_name);
        }
    }
    for topic in ["MotionAlarm", "Tamper", "Motion", "Person", "Vehicle"] {
        assert!(topics.iter().any(|name| name == topic));
    }
}

#[test]
fn digest_uses_post_and_the_entire_original_subscription_query() {
    use super::lifecycle::{create, pull};

    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    let body = pull("PT0S", 1, &header);
    let signed_path = client.authorization("/onvif/subscription", &body);
    let mut wrong_query = digest_auth::AuthorizationHeader::parse(&signed_path).unwrap();
    wrong_query.uri.clone_from(&target);
    assert_eq!(
        client
            .send(&target, &body, Some(&wrong_query.to_string()))
            .0,
        401
    );
    let mut get_context = AuthContext::new("test", "test", target.as_str());
    get_context.set_custom_cnonce(client.cnonce.as_str());
    let wrong_method = client.challenge.respond(&get_context).unwrap().to_string();
    assert_eq!(client.send(&target, &body, Some(&wrong_method)).0, 401);
    assert_eq!(client.post(&target, &body).0, 200);
    assert_eq!(client.post("/onvif/subscription?key=%31", &body).0, 200);
    assert_eq!(
        fake.requests().last().unwrap().target(),
        "/onvif/subscription?key=%31"
    );
    assert_eq!(client.post("/onvif/subscription?key=1&key=1", &body).0, 500);
}

#[test]
fn custom_credentials_are_verified_and_wrong_passwords_never_authenticate() {
    let fake = FakeOnvif::builder()
        .credentials("fixture-user", "fixture-password")
        .start()
        .unwrap();
    let mut client = Client::new(&fake);
    let body = envelope(GET_SERVICES, "");
    for (password, expected) in [("wrong-password", 401), ("fixture-password", 200)] {
        let mut context =
            AuthContext::new_post("fixture-user", password, DEVICE, Some(body.as_bytes()));
        context.set_custom_cnonce(client.cnonce.as_str());
        let auth = client.challenge.respond(&context).unwrap().to_string();
        assert_eq!(client.send(DEVICE, &body, Some(&auth)).0, expected);
    }
    assert!(!format!("{:?} {:?}", fake, fake.requests()).contains("fixture-password"));
}

#[test]
fn wrong_soap_namespaces_duplicate_bodies_and_excessive_xml_are_faulted() {
    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    let valid = envelope(GET_SERVICES, "");
    let requests = [
        valid.replace("http://www.w3.org/2003/05/soap-envelope", "urn:wrong"),
        valid.replace("http://www.onvif.org/ver10/device/wsdl", "urn:wrong"),
        envelope(&format!("{GET_SERVICES}{GET_SERVICES}"), ""),
        valid.replace(
            "</s:Envelope>",
            &format!("<s:Body>{GET_SERVICES}</s:Body></s:Envelope>"),
        ),
        envelope(
            GET_SERVICES,
            &format!("{}{}", "<deep>".repeat(20), "</deep>".repeat(20)),
        ),
        format!(
            "<!DOCTYPE s:Envelope [<!ENTITY fixture 'expanded'>]>{}",
            envelope(GET_SERVICES, "<value>&fixture;</value>")
        ),
    ];
    for request in requests {
        let (status, body) = client.post(DEVICE, &request);
        assert_eq!(status, 500);
        super::lifecycle::assert_root(&body, "http://www.w3.org/2003/05/soap-envelope", "Fault");
    }
    assert_eq!(client.post(DEVICE, &valid).0, 200);
}
