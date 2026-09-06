use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use xmltree::{Element, Namespace, XMLNode};

use super::{
    Endpoint, NOTIFICATION_XML_SIZE_BYTES_MAX, Notification, parse_notifications_with_time, xml,
};
use crate::soap::auth::username_token::UsernameToken;

/// A payload-safe failure while interpreting ONVIF event protocol data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolError(pub(super) &'static str);

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ONVIF event protocol: {}", self.0)
    }
}
impl std::error::Error for ProtocolError {}

/// Camera-relative lifetime of a subscription, independent of host clock skew.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Lease {
    remaining: Duration,
}

impl Lease {
    /// Parses a create, pull or renew response's current and termination times.
    ///
    /// # Errors
    /// Rejects missing timestamps, expired leases and lifetimes over one day.
    pub fn parse(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let root = xml::parse(bytes, NOTIFICATION_XML_SIZE_BYTES_MAX)?;
        Self::from_element(xml::payload(&root)?)
    }

    fn from_element(root: &Element) -> Result<Self, ProtocolError> {
        let namespace = if xml::matches(root, xml::EVENTS, "PullMessagesResponse") {
            xml::EVENTS
        } else {
            xml::NOTIFY
        };
        let current = DateTime::parse_from_rfc3339(&xml::field(root, namespace, "CurrentTime")?)
            .map_err(|_| ProtocolError("invalid current time"))?;
        let termination =
            DateTime::parse_from_rfc3339(&xml::field(root, namespace, "TerminationTime")?)
                .map_err(|_| ProtocolError("invalid termination time"))?;
        let remaining = (termination - current)
            .to_std()
            .map_err(|_| ProtocolError("expired subscription"))?;
        if remaining < Duration::from_millis(100) || remaining > Duration::from_secs(86400) {
            return Err(ProtocolError("invalid subscription lifetime"));
        }
        Ok(Self { remaining })
    }

    /// Returns the lifetime reported relative to the camera's current time.
    pub const fn remaining(self) -> Duration {
        self.remaining
    }
    /// Returns an early renewal interval that leaves one third of the lease unused.
    pub fn renew_after(self) -> Duration {
        self.remaining
            .mul_f64(2.0 / 3.0)
            .min(Duration::from_secs(300))
    }
}

/// One bounded subscription operation, constructed without network I/O.
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    /// Requests the camera's initialized property-state baseline.
    Synchronize,
    /// Long-polls at most 256 messages for at most ten seconds.
    Pull { timeout: Duration, limit: u32 },
    /// Renews a lease for a requested duration of at most five minutes.
    Renew { lifetime: Duration },
    /// Releases this subscription without changing camera rules.
    Unsubscribe,
}

/// A subscription endpoint, reference parameters and relative lease.
#[derive(Clone)]
pub struct Subscription {
    endpoint: Endpoint,
    parameters: Vec<Element>,
    lease: Lease,
}

impl Subscription {
    /// Parses a camera-bound subscription and retains its opaque reference headers.
    ///
    /// # Errors
    /// Rejects unsafe endpoints, missing leases, duplicate fields and forbidden headers.
    pub fn parse(camera: &Endpoint, bytes: &[u8]) -> Result<Self, ProtocolError> {
        let root = xml::parse(bytes, NOTIFICATION_XML_SIZE_BYTES_MAX)?;
        let root = xml::payload(&root)?;
        if !xml::matches(root, xml::EVENTS, "CreatePullPointSubscriptionResponse") {
            return Err(ProtocolError("wrong subscription response"));
        }
        let reference = xml::required(root, xml::EVENTS, "SubscriptionReference")?;
        let endpoint = camera
            .resolve(xml::field(reference, xml::ADDRESS, "Address")?)
            .map_err(|_| ProtocolError("unapproved subscription endpoint"))?;
        let parameters = reference_parameters(reference)?;
        Ok(Self {
            endpoint,
            parameters,
            lease: Lease::from_element(root)?,
        })
    }

    /// Returns the private camera-bound endpoint for authenticated requests.
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
    /// Returns the last camera-relative lease.
    pub const fn lease(&self) -> Lease {
        self.lease
    }

    /// Creates a request with this subscription's exact reference parameters.
    ///
    /// # Errors
    /// Rejects invalid pull limits or requested lifetimes.
    pub fn request(&self, operation: Operation) -> Result<Request, ProtocolError> {
        let (name, namespace, action) = match operation {
            Operation::Synchronize => (
                "SetSynchronizationPoint",
                xml::EVENTS,
                "http://www.onvif.org/ver10/events/wsdl/PullPointSubscription/SetSynchronizationPointRequest",
            ),
            Operation::Pull { .. } => (
                "PullMessages",
                xml::EVENTS,
                "http://www.onvif.org/ver10/events/wsdl/PullPointSubscription/PullMessagesRequest",
            ),
            Operation::Renew { .. } => (
                "Renew",
                xml::NOTIFY,
                "http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/RenewRequest",
            ),
            Operation::Unsubscribe => (
                "Unsubscribe",
                xml::NOTIFY,
                "http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/UnsubscribeRequest",
            ),
        };
        let mut body = xml::element(namespace, "e", name, None);
        match operation {
            Operation::Pull { timeout, limit } => {
                if limit == 0
                    || limit > 256
                    || timeout.is_zero()
                    || timeout > Duration::from_secs(10)
                {
                    return Err(ProtocolError("invalid pull limits"));
                }
                body.children.push(XMLNode::Element(xml::element(
                    namespace,
                    "e",
                    "Timeout",
                    Some(&duration_text(timeout)),
                )));
                body.children.push(XMLNode::Element(xml::element(
                    namespace,
                    "e",
                    "MessageLimit",
                    Some(&limit.to_string()),
                )));
            }
            Operation::Renew { lifetime } => {
                validate_lifetime(lifetime)?;
                body.children.push(XMLNode::Element(xml::element(
                    namespace,
                    "e",
                    "TerminationTime",
                    Some(&duration_text(lifetime)),
                )));
            }
            _ => {}
        }
        Ok(Request {
            endpoint: self.endpoint.clone(),
            parameters: self.parameters.clone(),
            body,
            action,
        })
    }
}

/// An event-service request that owns private WS-Addressing data.
#[derive(Clone)]
pub struct Request {
    pub(super) endpoint: Endpoint,
    parameters: Vec<Element>,
    body: Element,
    pub(super) action: &'static str,
}

impl Request {
    pub(super) fn discovery(endpoint: &Endpoint, name: &'static str) -> Self {
        let (namespace, action) = match name {
            "GetServices" => (
                "http://www.onvif.org/ver10/device/wsdl",
                "http://www.onvif.org/ver10/device/wsdl/GetServices",
            ),
            "GetServiceCapabilities" => (
                xml::EVENTS,
                "http://www.onvif.org/ver10/events/wsdl/EventPortType/GetServiceCapabilitiesRequest",
            ),
            "GetEventProperties" => (
                xml::EVENTS,
                "http://www.onvif.org/ver10/events/wsdl/EventPortType/GetEventPropertiesRequest",
            ),
            _ => unreachable!("discovery operation is selected internally"),
        };
        let mut body = xml::element(namespace, "e", name, None);
        if name == "GetServices" {
            body.children.push(XMLNode::Element(xml::element(
                namespace,
                "e",
                "IncludeCapability",
                Some("true"),
            )));
        }
        Self {
            endpoint: endpoint.clone(),
            parameters: Vec::new(),
            body,
            action,
        }
    }

    /// Builds a create operation without opening a subscription.
    ///
    /// # Errors
    /// Rejects zero lifetimes and lifetimes over five minutes.
    pub fn create(endpoint: &Endpoint, lifetime: Duration) -> Result<Self, ProtocolError> {
        validate_lifetime(lifetime)?;
        let mut body = xml::element(xml::EVENTS, "e", "CreatePullPointSubscription", None);
        body.children.push(XMLNode::Element(xml::element(
            xml::EVENTS,
            "e",
            "InitialTerminationTime",
            Some(&duration_text(lifetime)),
        )));
        Ok(Self {
            endpoint: endpoint.clone(),
            parameters: Vec::new(),
            body,
            action: "http://www.onvif.org/ver10/events/wsdl/EventPortType/CreatePullPointSubscriptionRequest",
        })
    }

    /// Serializes an envelope with optional credentials supplied by the transport.
    ///
    /// # Errors
    /// Rejects empty or excessive message IDs and oversized request envelopes.
    pub fn envelope(
        &self,
        token: Option<&UsernameToken>,
        message_id: &str,
    ) -> Result<String, ProtocolError> {
        if message_id.is_empty()
            || message_id.len() > 256
            || message_id.chars().any(char::is_control)
        {
            return Err(ProtocolError("invalid request message ID"));
        }
        let mut header = xml::element(xml::SOAP, "s", "Header", None);
        for (name, text) in [
            ("To", self.endpoint.as_str()),
            ("Action", self.action),
            ("MessageID", message_id),
        ] {
            header.children.push(XMLNode::Element(xml::element(
                xml::ADDRESS,
                "a",
                name,
                Some(text),
            )));
        }
        header
            .children
            .extend(self.parameters.iter().cloned().map(XMLNode::Element));
        if let Some(token) = token {
            header
                .children
                .push(XMLNode::Element(security_header(token)));
        }
        let mut body = xml::element(xml::SOAP, "s", "Body", None);
        body.children.push(XMLNode::Element(self.body.clone()));
        let mut envelope = xml::element(xml::SOAP, "s", "Envelope", None);
        envelope.children = vec![XMLNode::Element(header), XMLNode::Element(body)];
        xml::encode(&envelope)
    }
}

/// A bounded pull response with valid notifications and a malformed-message count.
#[derive(Debug)]
pub struct Pull {
    pub lease: Lease,
    pub notifications: Vec<Notification>,
    pub invalid_messages: u32,
}

impl Pull {
    /// Parses valid neighboring notifications even when one message is malformed.
    ///
    /// # Errors
    /// Rejects malformed document framing, invalid leases and excessive message counts.
    pub fn parse(bytes: &[u8]) -> Result<Self, ProtocolError> {
        Self::parse_with_time(bytes, None)
    }

    /// Parses notifications with receipt fallback while keeping subscription lease timestamps strict.
    ///
    /// Missing or invalid unqualified notification timestamps use `received_time` and
    /// do not increase `invalid_messages`. Other invalid messages remain isolated.
    ///
    /// # Errors
    /// Rejects malformed document framing, invalid leases, and the same bounds as [`Self::parse`].
    pub fn parse_at(
        bytes: impl AsRef<[u8]>,
        received_time: DateTime<Utc>,
    ) -> Result<Self, ProtocolError> {
        Self::parse_with_time(bytes.as_ref(), Some(received_time))
    }

    fn parse_with_time(
        bytes: &[u8],
        received_time: Option<DateTime<Utc>>,
    ) -> Result<Self, ProtocolError> {
        let root = xml::parse(bytes, NOTIFICATION_XML_SIZE_BYTES_MAX)?;
        let root = xml::payload(&root)?;
        if !xml::matches(root, xml::EVENTS, "PullMessagesResponse") {
            return Err(ProtocolError("wrong pull response"));
        }
        let lease = Lease::from_element(root)?;
        let mut notifications = Vec::new();
        let mut invalid_messages = 0;
        for node in root
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .filter(|node| xml::matches(node, xml::NOTIFY, "NotificationMessage"))
        {
            if notifications.len() + invalid_messages as usize >= 256 {
                return Err(ProtocolError("pull message count exceeded"));
            }
            match parse_notifications_with_time(xml::encode(node)?.as_bytes(), received_time) {
                Ok(mut parsed) if parsed.len() == 1 => notifications.push(parsed.remove(0)),
                _ => invalid_messages += 1,
            }
        }
        Ok(Self {
            lease,
            notifications,
            invalid_messages,
        })
    }
}

fn reference_parameters(reference: &Element) -> Result<Vec<Element>, ProtocolError> {
    let Some(parameters) = xml::child(reference, xml::ADDRESS, "ReferenceParameters")? else {
        return Ok(Vec::new());
    };
    if xml::encode(parameters)?.len() > 16 * 1024 {
        return Err(ProtocolError("reference parameters exceed byte limit"));
    }
    let mut retained = Vec::new();
    for parameter in parameters.children.iter().filter_map(XMLNode::as_element) {
        if retained.len() >= 16
            || matches!(
                parameter.namespace.as_deref(),
                Some(
                    xml::SOAP
                        | xml::ADDRESS
                        | "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd"
                )
            )
        {
            return Err(ProtocolError("forbidden subscription reference parameter"));
        }
        let marker = parameter.attributes.iter().find(|(key, _)| {
            key.split_once(':').is_some_and(|(prefix, name)| {
                name == "IsReferenceParameter"
                    && parameter
                        .namespaces
                        .as_ref()
                        .and_then(|scope| scope.get(prefix))
                        == Some(xml::ADDRESS)
            })
        });
        if let Some((_, value)) = marker {
            if matches!(value.as_str(), "true" | "1") {
                retained.push(parameter.clone());
                continue;
            }
            return Err(ProtocolError("conflicting reference parameter attribute"));
        }
        let mut parameter = parameter.clone();
        let namespaces = parameter.namespaces.get_or_insert_with(Namespace::empty);
        if namespaces
            .get("kpwsa")
            .is_some_and(|namespace| namespace != xml::ADDRESS)
        {
            return Err(ProtocolError("conflicting reference parameter namespace"));
        }
        namespaces.put("kpwsa", xml::ADDRESS);
        parameter
            .attributes
            .insert("kpwsa:IsReferenceParameter".to_owned(), "true".to_owned());
        retained.push(parameter);
    }
    Ok(retained)
}

fn validate_lifetime(lifetime: Duration) -> Result<(), ProtocolError> {
    if lifetime.is_zero() || lifetime > Duration::from_secs(300) {
        return Err(ProtocolError("invalid requested lifetime"));
    }
    Ok(())
}

fn duration_text(duration: Duration) -> String {
    format!("PT{}.{:03}S", duration.as_secs(), duration.subsec_millis())
}

fn security_header(token: &UsernameToken) -> Element {
    const SECURITY: &str =
        "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
    const UTILITY: &str =
        "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";
    let mut value = xml::element(SECURITY, "wsse", "UsernameToken", None);
    value.children.push(XMLNode::Element(xml::element(
        SECURITY,
        "wsse",
        "Username",
        Some(&token.username),
    )));
    let mut password = xml::element(SECURITY, "wsse", "Password", Some(&token.digest));
    password.attributes.insert("Type".to_owned(), "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest".to_owned());
    value.children.push(XMLNode::Element(password));
    let mut nonce = xml::element(SECURITY, "wsse", "Nonce", Some(&token.nonce));
    nonce.attributes.insert("EncodingType".to_owned(), "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary".to_owned());
    value.children.push(XMLNode::Element(nonce));
    value.children.push(XMLNode::Element(xml::element(
        UTILITY,
        "wsu",
        "Created",
        Some(&token.created),
    )));
    let mut security = xml::element(SECURITY, "wsse", "Security", None);
    security.children.push(XMLNode::Element(value));
    security
}

impl fmt::Debug for Subscription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Subscription")
            .field("lease", &self.lease)
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Request")
            .field("action", &self.action)
            .finish_non_exhaustive()
    }
}
