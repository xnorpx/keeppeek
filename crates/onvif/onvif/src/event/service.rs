use std::fmt;
use std::time::{Duration, Instant};

use xmltree::{Element, XMLNode};

use super::client::Failure;
use super::{
    Client, ClientError, Endpoint, ExpandedName, Kind, NOTIFICATION_XML_SIZE_BYTES_MAX,
    ProtocolError, Request, Topic, xml,
};

const DEVICE: &str = "http://www.onvif.org/ver10/device/wsdl";
const TOPIC_SET: &str = "http://docs.oasis-open.org/wsn/t-1";

/// Private service metadata with positive topic and capacity evidence.
#[derive(Clone)]
pub struct Service {
    endpoint: Endpoint,
    max_pull_points: Option<u32>,
    max_producers: Option<u32>,
    persistent: Option<bool>,
    topics: Vec<Topic>,
    dialects: Vec<String>,
    kinds: Vec<Kind>,
}

impl Service {
    /// Returns the private validated event service endpoint.
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
    /// Returns the advertised subscription capacity, if available.
    pub const fn max_pull_points(&self) -> Option<u32> {
        self.max_pull_points
    }
    /// Returns the advertised producer limit, if available.
    pub const fn max_producers(&self) -> Option<u32> {
        self.max_producers
    }
    /// Returns the advertised persistent-notification storage support.
    pub const fn persistent(&self) -> Option<bool> {
        self.persistent
    }
    /// Reports whether a subscription may be attempted; missing limits are unknown.
    pub fn pull_supported(&self) -> bool {
        self.max_pull_points != Some(0)
    }
    /// Returns bounded namespace-expanded topic evidence.
    pub fn topics(&self) -> &[Topic] {
        &self.topics
    }
    /// Returns advertised topic-expression dialects without fetching their URLs.
    pub fn topic_dialects(&self) -> &[String] {
        &self.dialects
    }
    /// Returns known event kinds derived from the same table as event normalization.
    pub fn kinds(&self) -> &[Kind] {
        &self.kinds
    }

    const fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            max_pull_points: None,
            max_producers: None,
            persistent: None,
            topics: Vec::new(),
            dialects: Vec::new(),
            kinds: Vec::new(),
        }
    }

    fn capabilities(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        let root = xml::parse(bytes, NOTIFICATION_XML_SIZE_BYTES_MAX)?;
        let root = xml::payload(&root)?;
        if !xml::matches(root, xml::EVENTS, "GetServiceCapabilitiesResponse") {
            return Err(ProtocolError("wrong capability response"));
        }
        let cap = xml::required(root, xml::EVENTS, "Capabilities")?;
        let number = |name| {
            cap.attributes
                .get(name)
                .map(|value| {
                    value
                        .parse::<u32>()
                        .map_err(|_| ProtocolError("invalid capability limit"))
                })
                .transpose()
        };
        self.max_pull_points = number("MaxPullPoints")?;
        self.max_producers = number("MaxNotificationProducers")?;
        self.persistent = cap
            .attributes
            .get("PersistentNotificationStorage")
            .map(|value| match value.as_str() {
                "true" | "1" => Ok(true),
                "false" | "0" => Ok(false),
                _ => Err(ProtocolError("invalid capability Boolean")),
            })
            .transpose()?;
        Ok(())
    }

    fn properties(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        let root = xml::parse(bytes, NOTIFICATION_XML_SIZE_BYTES_MAX)?;
        let root = xml::payload(&root)?;
        if !xml::matches(root, xml::EVENTS, "GetEventPropertiesResponse") {
            return Err(ProtocolError("wrong properties response"));
        }
        let mut dialects = Vec::new();
        for node in root
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .filter(|node| xml::matches(node, xml::NOTIFY, "TopicExpressionDialect"))
        {
            if dialects.len() >= 16 {
                return Err(ProtocolError("too many topic dialects"));
            }
            dialects.push(xml::text(node)?);
        }
        let topics = xml::child(root, TOPIC_SET, "TopicSet")?
            .map(topic_paths)
            .transpose()?
            .unwrap_or_default();
        let mut kinds = Vec::new();
        for topic in &topics {
            if let Some(kind) = super::normalize::topic_kind(topic)?
                && !kinds.contains(&kind)
            {
                kinds.push(kind);
            }
        }
        self.topics = topics;
        self.kinds = kinds;
        self.dialects = dialects;
        Ok(())
    }
}

impl Client {
    /// Discovers a camera's event service and its bounded optional capability evidence.
    ///
    /// # Errors
    /// Returns authentication, unsafe-endpoint, malformed-service and network errors.
    /// Optional unavailable capability/property responses leave those fields unknown.
    pub fn discover(&mut self, timeout: Duration) -> Result<Option<Service>, ClientError> {
        let deadline = Instant::now() + timeout;
        let bytes = self.execute(&Request::discovery(&self.camera, "GetServices"), timeout)?;
        let root = xml::parse(&bytes, NOTIFICATION_XML_SIZE_BYTES_MAX)?;
        let root = xml::payload(&root)?;
        if !xml::matches(root, DEVICE, "GetServicesResponse") {
            return Err(ClientError(Failure::Protocol));
        }
        let mut endpoint = None;
        for service in root
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .filter(|service| xml::matches(service, DEVICE, "Service"))
        {
            if xml::field(service, DEVICE, "Namespace")? != xml::EVENTS {
                continue;
            }
            if endpoint.is_some() {
                return Err(ClientError(Failure::Protocol));
            }
            endpoint = Some(
                self.camera
                    .resolve(xml::field(service, DEVICE, "XAddr")?)
                    .map_err(|_| ClientError(Failure::Protocol))?,
            );
        }
        endpoint
            .map(|endpoint| {
                self.event_service(endpoint, deadline.saturating_duration_since(Instant::now()))
            })
            .transpose()
    }

    /// Queries an explicitly selected event endpoint without creating a subscription.
    ///
    /// # Errors
    /// Rejects foreign endpoints, authentication failures and invalid time budgets.
    pub fn event_service(
        &mut self,
        endpoint: Endpoint,
        timeout: Duration,
    ) -> Result<Service, ClientError> {
        let deadline = Instant::now() + timeout;
        let mut service = Service::new(endpoint);
        for operation in ["GetServiceCapabilities", "GetEventProperties"] {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.execute(&Request::discovery(&service.endpoint, operation), remaining) {
                Ok(bytes) => {
                    let parsed = if operation == "GetServiceCapabilities" {
                        service.capabilities(&bytes)
                    } else {
                        service.properties(&bytes)
                    };
                    if parsed.is_err() {
                        tracing::debug!(operation, "ONVIF event evidence is malformed");
                    }
                }
                Err(error) if error.is_authentication() => return Err(error),
                Err(_) => tracing::debug!(operation, "ONVIF event evidence is unavailable"),
            }
        }
        Ok(service)
    }
}

fn topic_paths(root: &Element) -> Result<Vec<Topic>, ProtocolError> {
    let mut topics = Vec::new();
    let mut pending = root
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .map(|node| (node, Vec::new()))
        .collect::<Vec<_>>();
    while let Some((node, mut path)) = pending.pop() {
        if path.len() >= 32 || topics.len() >= 256 {
            return Err(ProtocolError("topic set limit exceeded"));
        }
        path.push(ExpandedName {
            namespace_uri: node.namespace.clone(),
            local_name: node.name.clone(),
        });
        if node.attributes.iter().any(|(name, value)| {
            name.rsplit(':').next() == Some("topic") && matches!(value.as_str(), "true" | "1")
        }) {
            topics.push(Topic {
                dialect: String::new(),
                path: path.clone(),
            });
        }
        pending.extend(
            node.children
                .iter()
                .filter_map(XMLNode::as_element)
                .filter(|child| {
                    child.namespace.as_deref() != Some("http://www.onvif.org/ver10/schema")
                })
                .map(|child| (child, path.clone())),
        );
    }
    Ok(topics)
}

impl fmt::Debug for Service {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Service")
            .field("max_pull_points", &self.max_pull_points)
            .field("kinds", &self.kinds)
            .finish_non_exhaustive()
    }
}
