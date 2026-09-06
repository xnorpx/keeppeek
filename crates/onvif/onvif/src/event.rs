//! Bounded parsing for ONVIF WS-Notification event messages.

mod client;
mod endpoint;
mod metadata;
mod normalize;
mod protocol;
mod service;
mod snapshot;
mod xml;
#[doc(inline)]
pub use client::{Client, ClientError, PullLimits};
#[doc(inline)]
pub use endpoint::{Endpoint, EndpointError};
#[doc(inline)]
pub use metadata::{BoundingBox, Frame, Metadata, Object, ObjectClass};
#[doc(inline)]
pub use normalize::{Detection, Kind, normalize};
#[doc(inline)]
pub use protocol::{Lease, Operation, ProtocolError, Pull, Request, Subscription};
#[doc(inline)]
pub use service::Service;

use std::io::BufReader;

use ::xml::{
    attribute::OwnedAttribute,
    name::OwnedName,
    namespace::Namespace,
    reader::{ParserConfig, XmlEvent},
};
use chrono::{DateTime, Utc};
use thiserror::Error;

const ONVIF_SCHEMA_NAMESPACE: &str = "http://www.onvif.org/ver10/schema";
const WS_NOTIFICATION_NAMESPACE: &str = "http://docs.oasis-open.org/wsn/b-2";
const XML_DEPTH_MAX: usize = 32;
const NOTIFICATION_COUNT_MAX: usize = 256;
const TOPIC_SEGMENT_COUNT_MAX: usize = 32;
const ITEM_COUNT_MAX: usize = 256;
const ELEMENT_NODE_COUNT_MAX: usize = 256;

/// Maximum accepted size of one notification XML document.
pub const NOTIFICATION_XML_SIZE_BYTES_MAX: usize = 256 * 1024;

/// A normalized ONVIF notification independent of document namespace prefixes.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Notification {
    pub topic: Topic,
    /// The camera time, or the supplied receipt time when parsing permits fallback.
    pub utc_time: DateTime<Utc>,
    /// Identifies whether `utc_time` came from the camera or receipt clock.
    pub timestamp_source: TimestampSource,
    pub property_operation: Option<PropertyOperation>,
    pub source: ItemList,
    pub key: ItemList,
    pub data: ItemList,
}

/// Identifies the clock used for a parsed notification or metadata frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum TimestampSource {
    /// The unqualified `UtcTime` attribute contains a valid RFC3339 timestamp.
    #[default]
    Camera,
    /// The caller supplied the receipt time because camera time was unavailable.
    Received { reason: TimestampReason },
}

/// Explains why parsing used receipt time instead of camera time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TimestampReason {
    /// The message has no `UtcTime` attribute.
    Missing,
    /// The unqualified `UtcTime` attribute is not a valid RFC3339 timestamp.
    Invalid,
}

/// A topic path qualified by the namespace of its root segment.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Topic {
    pub dialect: String,
    pub path: Vec<ExpandedName>,
}

/// The operation associated with an ONVIF property notification.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PropertyOperation {
    Initialized,
    Changed,
    Deleted,
    Other(String),
}

/// Simple and structured values from an ONVIF message section.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ItemList {
    pub simple: Vec<SimpleItem>,
    pub element: Vec<ElementItem>,
}

/// A name-value item from an ONVIF message.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct SimpleItem {
    pub name: String,
    pub value: String,
}

impl SimpleItem {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// A named ONVIF item containing one structured XML value.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ElementItem {
    pub name: String,
    pub value: XmlElement,
}

/// A namespace-expanded XML element retained from an ONVIF item.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct XmlElement {
    pub name: ExpandedName,
    pub attributes: Vec<XmlAttribute>,
    pub text: String,
    pub children: Vec<Self>,
}

/// An XML name represented without its document-specific prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ExpandedName {
    pub namespace_uri: Option<String>,
    pub local_name: String,
}

/// A namespace-expanded XML attribute retained from an ONVIF item.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct XmlAttribute {
    pub name: ExpandedName,
    pub value: String,
}

/// An error produced while parsing untrusted ONVIF notification XML.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NotificationParseError {
    #[error("notification XML is {actual} bytes; maximum is {maximum} bytes")]
    PayloadTooLarge { actual: usize, maximum: usize },
    #[error("notification XML exceeds the {maximum}-level depth limit")]
    DepthExceeded { maximum: usize },
    #[error("notification exceeds the {maximum} {kind} limit")]
    CountExceeded { kind: &'static str, maximum: usize },
    #[error("notification is missing required {0}")]
    Missing(&'static str),
    #[error("notification has duplicate {0} attributes")]
    DuplicateAttribute(&'static str),
    #[error("notification has invalid structure: {0}")]
    InvalidStructure(&'static str),
    #[error("notification contains a forbidden document type declaration")]
    DocumentType,
    #[error("notification UTC time is invalid: {0}")]
    UtcTime(#[from] chrono::ParseError),
    #[error("notification XML is invalid: {0}")]
    Xml(#[from] ::xml::reader::Error),
}

/// Parses all WS-Notification messages in one PullMessages or metadata document.
///
/// # Errors
/// Rejects missing or invalid camera timestamps, malformed messages, and exceeded XML bounds.
pub fn parse_notifications(xml: &[u8]) -> Result<Vec<Notification>, NotificationParseError> {
    parse_notifications_with_time(xml, None)
}

/// Parses notifications with explicit receipt-time fallback for missing or invalid camera timestamps.
///
/// Valid camera timestamps retain their value and camera provenance. Missing or invalid
/// unqualified `UtcTime` values use `received_time`; parsing never reads a system clock.
///
/// # Errors
/// Rejects qualified-only timestamps, malformed messages, and the same XML bounds as strict parsing.
pub fn parse_notifications_at(
    xml: impl AsRef<[u8]>,
    received_time: DateTime<Utc>,
) -> Result<Vec<Notification>, NotificationParseError> {
    parse_notifications_with_time(xml.as_ref(), Some(received_time))
}

fn parse_notifications_with_time(
    xml: &[u8],
    received_time: Option<DateTime<Utc>>,
) -> Result<Vec<Notification>, NotificationParseError> {
    if xml.len() > NOTIFICATION_XML_SIZE_BYTES_MAX {
        return Err(NotificationParseError::PayloadTooLarge {
            actual: xml.len(),
            maximum: NOTIFICATION_XML_SIZE_BYTES_MAX,
        });
    }

    let reader = ParserConfig::new()
        .whitespace_to_characters(true)
        .cdata_to_characters(true)
        .coalesce_characters(true)
        .max_entity_expansion_length(4 * 1024)
        .max_entity_expansion_depth(4)
        .max_name_length(256)
        .max_attributes(32)
        .max_attribute_length(4 * 1024)
        .max_data_length(64 * 1024)
        .create_reader(BufReader::new(xml));
    let mut parser = NotificationParser {
        received_time,
        ..NotificationParser::default()
    };

    for event in reader {
        match event? {
            XmlEvent::StartElement {
                name,
                attributes,
                namespace,
            } => parser.start_element(name, attributes, namespace)?,
            XmlEvent::EndElement { name } => parser.end_element(&name)?,
            XmlEvent::Characters(text) | XmlEvent::CData(text) => parser.text(&text),
            XmlEvent::Doctype { .. } => return Err(NotificationParseError::DocumentType),
            XmlEvent::StartDocument { .. }
            | XmlEvent::EndDocument
            | XmlEvent::ProcessingInstruction { .. }
            | XmlEvent::Comment(_)
            | XmlEvent::Whitespace(_) => {}
        }
    }

    if parser.current.is_some() {
        return Err(NotificationParseError::InvalidStructure(
            "notification ended before its closing element",
        ));
    }
    Ok(parser.notifications)
}

#[derive(Default)]
struct NotificationParser {
    depth: usize,
    current: Option<NotificationBuilder>,
    notifications: Vec<Notification>,
    received_time: Option<DateTime<Utc>>,
}

impl NotificationParser {
    fn start_element(
        &mut self,
        name: OwnedName,
        attributes: Vec<OwnedAttribute>,
        namespace: Namespace,
    ) -> Result<(), NotificationParseError> {
        self.depth = self
            .depth
            .checked_add(1)
            .ok_or(NotificationParseError::DepthExceeded {
                maximum: XML_DEPTH_MAX,
            })?;
        if self.depth > XML_DEPTH_MAX {
            return Err(NotificationParseError::DepthExceeded {
                maximum: XML_DEPTH_MAX,
            });
        }
        validate_unique_attributes(&attributes)?;

        if self.current.is_none()
            && is_element(&name, WS_NOTIFICATION_NAMESPACE, "NotificationMessage")
        {
            if self.notifications.len() >= NOTIFICATION_COUNT_MAX {
                return Err(NotificationParseError::CountExceeded {
                    kind: "notification count",
                    maximum: NOTIFICATION_COUNT_MAX,
                });
            }
            self.current = Some(NotificationBuilder::new(self.depth, self.received_time));
            return Ok(());
        }

        if let Some(current) = self.current.as_mut() {
            current.start_element(name, attributes, namespace, self.depth)?;
        }
        Ok(())
    }

    fn end_element(&mut self, name: &OwnedName) -> Result<(), NotificationParseError> {
        let finishes_notification = self
            .current
            .as_mut()
            .map(|current| current.end_element(name, self.depth))
            .transpose()?
            .unwrap_or(false);

        if finishes_notification {
            let notification = self
                .current
                .take()
                .ok_or(NotificationParseError::InvalidStructure(
                    "notification parser lost its active message",
                ))?
                .finish()?;
            self.notifications.push(notification);
        }
        self.depth = self
            .depth
            .checked_sub(1)
            .ok_or(NotificationParseError::InvalidStructure(
                "unexpected closing element",
            ))?;
        Ok(())
    }

    fn text(&mut self, text: &str) {
        if let Some(current) = self.current.as_mut() {
            current.text(text);
        }
    }
}

#[derive(Clone, Copy)]
enum ItemSection {
    Source,
    Key,
    Data,
}

struct NotificationBuilder {
    start_depth: usize,
    message_wrapper_depth: Option<usize>,
    message_wrapper_seen: bool,
    message_depth: Option<usize>,
    message_seen: bool,
    section: Option<(ItemSection, usize)>,
    sections_seen: u8,
    topic_capture: Option<TopicCapture>,
    element_capture: Option<ElementCapture>,
    topic: Option<Topic>,
    timestamp: Option<(DateTime<Utc>, TimestampSource)>,
    received_time: Option<DateTime<Utc>>,
    property_operation: Option<PropertyOperation>,
    source: ItemList,
    key: ItemList,
    data: ItemList,
    item_count: usize,
}

impl NotificationBuilder {
    fn new(start_depth: usize, received_time: Option<DateTime<Utc>>) -> Self {
        Self {
            start_depth,
            message_wrapper_depth: None,
            message_wrapper_seen: false,
            message_depth: None,
            message_seen: false,
            section: None,
            sections_seen: 0,
            topic_capture: None,
            element_capture: None,
            topic: None,
            timestamp: None,
            received_time,
            property_operation: None,
            source: ItemList::default(),
            key: ItemList::default(),
            data: ItemList::default(),
            item_count: 0,
        }
    }

    fn start_element(
        &mut self,
        name: OwnedName,
        attributes: Vec<OwnedAttribute>,
        namespace: Namespace,
        depth: usize,
    ) -> Result<(), NotificationParseError> {
        if let Some(capture) = self.element_capture.as_mut() {
            capture.start_node(name, attributes)?;
            return Ok(());
        }
        if self.topic_capture.is_some() {
            return Err(NotificationParseError::InvalidStructure(
                "Topic must contain text only",
            ));
        }
        if is_element(&name, WS_NOTIFICATION_NAMESPACE, "Topic") {
            if depth != self.start_depth + 1 {
                return Ok(());
            }
            if self.topic.is_some() {
                return Err(NotificationParseError::InvalidStructure(
                    "notification contains multiple Topic values",
                ));
            }
            self.topic_capture = Some(TopicCapture {
                depth,
                dialect: required_non_empty_attribute(&attributes, "Dialect")?.to_owned(),
                namespace,
                text: String::new(),
            });
            return Ok(());
        }
        if is_element(&name, WS_NOTIFICATION_NAMESPACE, "Message") && depth == self.start_depth + 1
        {
            if self.message_wrapper_seen {
                return Err(NotificationParseError::InvalidStructure(
                    "notification contains multiple Message wrappers",
                ));
            }
            self.message_wrapper_depth = Some(depth);
            self.message_wrapper_seen = true;
            return Ok(());
        }
        if is_element(&name, ONVIF_SCHEMA_NAMESPACE, "Message")
            && self.message_wrapper_depth == Some(depth.saturating_sub(1))
        {
            self.start_message(&attributes, depth)?;
            return Ok(());
        }
        if self.start_section(&name, depth)? {
            return Ok(());
        }
        self.start_item(name, attributes, depth)
    }

    fn start_message(
        &mut self,
        attributes: &[OwnedAttribute],
        depth: usize,
    ) -> Result<(), NotificationParseError> {
        if self.message_seen {
            return Err(NotificationParseError::InvalidStructure(
                "notification contains multiple Message values",
            ));
        }
        let utc_time = optional_attribute(attributes, "UtcTime")?;
        if utc_time.is_none()
            && attributes
                .iter()
                .any(|attribute| attribute.name.local_name == "UtcTime")
        {
            return Err(NotificationParseError::Missing("UtcTime"));
        }
        self.timestamp = Some(parse_timestamp(utc_time, self.received_time)?);
        self.property_operation =
            optional_attribute(attributes, "PropertyOperation")?.map(PropertyOperation::from_wire);
        self.message_depth = Some(depth);
        self.message_seen = true;
        Ok(())
    }

    fn start_section(
        &mut self,
        name: &OwnedName,
        depth: usize,
    ) -> Result<bool, NotificationParseError> {
        if self.message_depth != Some(depth.saturating_sub(1)) {
            return Ok(false);
        }
        let section = match name.local_name.as_str() {
            "Source" if name.namespace.as_deref() == Some(ONVIF_SCHEMA_NAMESPACE) => {
                ItemSection::Source
            }
            "Key" if name.namespace.as_deref() == Some(ONVIF_SCHEMA_NAMESPACE) => ItemSection::Key,
            "Data" if name.namespace.as_deref() == Some(ONVIF_SCHEMA_NAMESPACE) => {
                ItemSection::Data
            }
            _ => return Ok(false),
        };
        if self.sections_seen & section.bit() != 0 {
            return Err(NotificationParseError::InvalidStructure(
                "notification contains a repeated item section",
            ));
        }
        self.sections_seen |= section.bit();
        self.section = Some((section, depth));
        Ok(true)
    }

    fn start_item(
        &mut self,
        name: OwnedName,
        attributes: Vec<OwnedAttribute>,
        depth: usize,
    ) -> Result<(), NotificationParseError> {
        let Some((section, section_depth)) = self.section else {
            return Ok(());
        };
        if depth != section_depth + 1 || name.namespace.as_deref() != Some(ONVIF_SCHEMA_NAMESPACE) {
            return Ok(());
        }
        match name.local_name.as_str() {
            "SimpleItem" => {
                let item_name = required_non_empty_attribute(&attributes, "Name")?;
                if self.items(section).contains_name(item_name) {
                    return Err(NotificationParseError::InvalidStructure(
                        "item names must be unique within a section",
                    ));
                }
                self.reserve_item()?;
                let item = SimpleItem::new(item_name, required_attribute(&attributes, "Value")?);
                self.items_mut(section).simple.push(item);
            }
            "ElementItem" => {
                let item_name = required_non_empty_attribute(&attributes, "Name")?;
                if self.items(section).contains_name(item_name) {
                    return Err(NotificationParseError::InvalidStructure(
                        "item names must be unique within a section",
                    ));
                }
                self.reserve_item()?;
                self.element_capture = Some(ElementCapture::new(item_name.to_owned(), depth));
            }
            _ => {}
        }
        Ok(())
    }

    const fn reserve_item(&mut self) -> Result<(), NotificationParseError> {
        if self.item_count >= ITEM_COUNT_MAX {
            return Err(NotificationParseError::CountExceeded {
                kind: "item count",
                maximum: ITEM_COUNT_MAX,
            });
        }
        self.item_count += 1;
        Ok(())
    }

    fn end_element(
        &mut self,
        name: &OwnedName,
        depth: usize,
    ) -> Result<bool, NotificationParseError> {
        if let Some(capture) = self.element_capture.as_mut() {
            if depth > capture.container_depth {
                capture.end_node()?;
                return Ok(false);
            }
            let element = self
                .element_capture
                .take()
                .ok_or(NotificationParseError::InvalidStructure(
                    "ElementItem capture disappeared",
                ))?
                .finish()?;
            let section = self
                .section
                .ok_or(NotificationParseError::InvalidStructure(
                    "ElementItem is outside an item section",
                ))?
                .0;
            self.items_mut(section).element.push(element);
            return Ok(false);
        }
        if self
            .topic_capture
            .as_ref()
            .is_some_and(|topic| topic.depth == depth)
        {
            let topic = self
                .topic_capture
                .take()
                .ok_or(NotificationParseError::InvalidStructure(
                    "Topic capture disappeared",
                ))?
                .finish()?;
            self.topic = Some(topic);
            return Ok(false);
        }
        if self
            .section
            .is_some_and(|(_, section_depth)| section_depth == depth)
        {
            self.section = None;
        }
        if self.message_depth == Some(depth) {
            self.message_depth = None;
        }
        if self.message_wrapper_depth == Some(depth)
            && is_element(name, WS_NOTIFICATION_NAMESPACE, "Message")
        {
            self.message_wrapper_depth = None;
        }
        Ok(depth == self.start_depth
            && is_element(name, WS_NOTIFICATION_NAMESPACE, "NotificationMessage"))
    }

    fn text(&mut self, text: &str) {
        if let Some(capture) = self.element_capture.as_mut() {
            capture.text(text);
        } else if let Some(topic) = self.topic_capture.as_mut() {
            topic.text.push_str(text);
        }
    }

    const fn items_mut(&mut self, section: ItemSection) -> &mut ItemList {
        match section {
            ItemSection::Source => &mut self.source,
            ItemSection::Key => &mut self.key,
            ItemSection::Data => &mut self.data,
        }
    }

    const fn items(&self, section: ItemSection) -> &ItemList {
        match section {
            ItemSection::Source => &self.source,
            ItemSection::Key => &self.key,
            ItemSection::Data => &self.data,
        }
    }

    fn finish(self) -> Result<Notification, NotificationParseError> {
        if !self.message_wrapper_seen {
            return Err(NotificationParseError::Missing("Message"));
        }
        if !self.message_seen {
            return Err(NotificationParseError::Missing("Message/Message"));
        }
        let topic = self.topic.ok_or(NotificationParseError::Missing("Topic"))?;
        let (utc_time, timestamp_source) = self
            .timestamp
            .ok_or(NotificationParseError::Missing("Message@UtcTime"))?;
        Ok(Notification {
            topic,
            utc_time,
            timestamp_source,
            property_operation: self.property_operation,
            source: self.source,
            key: self.key,
            data: self.data,
        })
    }
}

fn parse_timestamp(
    value: Option<&str>,
    received_time: Option<DateTime<Utc>>,
) -> Result<(DateTime<Utc>, TimestampSource), NotificationParseError> {
    let (reason, error) = match value {
        Some(value) => match DateTime::parse_from_rfc3339(value) {
            Ok(camera_time) => {
                return Ok((camera_time.with_timezone(&Utc), TimestampSource::Camera));
            }
            Err(error) => (
                TimestampReason::Invalid,
                NotificationParseError::UtcTime(error),
            ),
        },
        None => (
            TimestampReason::Missing,
            NotificationParseError::Missing("UtcTime"),
        ),
    };
    received_time
        .map(|time| (time, TimestampSource::Received { reason }))
        .ok_or(error)
}

struct TopicCapture {
    depth: usize,
    dialect: String,
    namespace: Namespace,
    text: String,
}

impl TopicCapture {
    fn finish(self) -> Result<Topic, NotificationParseError> {
        let mut path = Vec::new();
        for (index, segment) in self.text.trim().split('/').map(str::trim).enumerate() {
            if path.len() >= TOPIC_SEGMENT_COUNT_MAX {
                return Err(NotificationParseError::CountExceeded {
                    kind: "topic segment count",
                    maximum: TOPIC_SEGMENT_COUNT_MAX,
                });
            }
            let (prefix, local_name) = segment
                .split_once(':')
                .map_or((None, segment), |(prefix, local_name)| {
                    (Some(prefix), local_name)
                });
            if local_name.is_empty() {
                return Err(NotificationParseError::InvalidStructure(
                    "Topic contains an empty path segment",
                ));
            }
            if prefix.is_some_and(str::is_empty) || local_name.contains(':') {
                return Err(NotificationParseError::InvalidStructure(
                    "Topic contains an invalid qualified name",
                ));
            }
            if index == 0 && prefix.is_none() {
                return Err(NotificationParseError::InvalidStructure(
                    "Topic root must be namespace-qualified",
                ));
            }
            let namespace_uri = prefix
                .map(|prefix| {
                    self.namespace
                        .get(prefix)
                        .ok_or(NotificationParseError::InvalidStructure(
                            "Topic prefix has no namespace declaration",
                        ))
                })
                .transpose()?
                .map(str::to_owned);
            path.push(ExpandedName {
                namespace_uri,
                local_name: local_name.to_owned(),
            });
        }
        if path.is_empty() {
            return Err(NotificationParseError::Missing("Topic value"));
        }
        Ok(Topic {
            dialect: self.dialect,
            path,
        })
    }
}

struct ElementCapture {
    item_name: String,
    container_depth: usize,
    node_count: usize,
    stack: Vec<XmlElement>,
    root: Option<XmlElement>,
}

impl ElementCapture {
    const fn new(item_name: String, container_depth: usize) -> Self {
        Self {
            item_name,
            container_depth,
            node_count: 0,
            stack: Vec::new(),
            root: None,
        }
    }

    fn start_node(
        &mut self,
        name: OwnedName,
        attributes: Vec<OwnedAttribute>,
    ) -> Result<(), NotificationParseError> {
        if self.node_count >= ELEMENT_NODE_COUNT_MAX {
            return Err(NotificationParseError::CountExceeded {
                kind: "ElementItem node count",
                maximum: ELEMENT_NODE_COUNT_MAX,
            });
        }
        self.node_count += 1;
        self.stack.push(XmlElement {
            name: expand_name(name),
            attributes: attributes
                .into_iter()
                .map(|attribute| XmlAttribute {
                    name: expand_name(attribute.name),
                    value: attribute.value,
                })
                .collect(),
            text: String::new(),
            children: Vec::new(),
        });
        Ok(())
    }

    fn text(&mut self, text: &str) {
        if let Some(node) = self.stack.last_mut() {
            node.text.push_str(text);
        }
    }

    fn end_node(&mut self) -> Result<(), NotificationParseError> {
        let mut node = self
            .stack
            .pop()
            .ok_or(NotificationParseError::InvalidStructure(
                "ElementItem has an unexpected closing element",
            ))?;
        node.text = node.text.trim().to_owned();
        if let Some(parent) = self.stack.last_mut() {
            parent.children.push(node);
        } else if self.root.replace(node).is_some() {
            return Err(NotificationParseError::InvalidStructure(
                "ElementItem must contain exactly one value element",
            ));
        }
        Ok(())
    }

    fn finish(self) -> Result<ElementItem, NotificationParseError> {
        if !self.stack.is_empty() {
            return Err(NotificationParseError::InvalidStructure(
                "ElementItem value ended before its closing element",
            ));
        }
        Ok(ElementItem {
            name: self.item_name,
            value: self
                .root
                .ok_or(NotificationParseError::Missing("ElementItem value element"))?,
        })
    }
}

fn required_attribute<'a>(
    attributes: &'a [OwnedAttribute],
    local_name: &'static str,
) -> Result<&'a str, NotificationParseError> {
    optional_attribute(attributes, local_name)?.ok_or(NotificationParseError::Missing(local_name))
}

fn required_non_empty_attribute<'a>(
    attributes: &'a [OwnedAttribute],
    local_name: &'static str,
) -> Result<&'a str, NotificationParseError> {
    let value = required_attribute(attributes, local_name)?;
    if value.is_empty() {
        return Err(NotificationParseError::InvalidStructure(
            "required attribute must not be empty",
        ));
    }
    Ok(value)
}

fn optional_attribute<'a>(
    attributes: &'a [OwnedAttribute],
    local_name: &'static str,
) -> Result<Option<&'a str>, NotificationParseError> {
    let mut values = attributes.iter().filter(|attribute| {
        attribute.name.local_name == local_name && attribute.name.namespace.is_none()
    });
    let value = values.next().map(|attribute| attribute.value.as_str());
    if values.next().is_some() {
        return Err(NotificationParseError::DuplicateAttribute(local_name));
    }
    Ok(value)
}

fn validate_unique_attributes(attributes: &[OwnedAttribute]) -> Result<(), NotificationParseError> {
    for (index, attribute) in attributes.iter().enumerate() {
        if attributes[..index].iter().any(|candidate| {
            candidate.name.namespace == attribute.name.namespace
                && candidate.name.local_name == attribute.name.local_name
        }) {
            return Err(NotificationParseError::InvalidStructure(
                "element contains duplicate attributes",
            ));
        }
    }
    Ok(())
}

fn is_element(name: &OwnedName, namespace_uri: &str, local_name: &str) -> bool {
    name.namespace.as_deref() == Some(namespace_uri) && name.local_name == local_name
}

fn expand_name(name: OwnedName) -> ExpandedName {
    ExpandedName {
        namespace_uri: name.namespace,
        local_name: name.local_name,
    }
}

impl PropertyOperation {
    fn from_wire(value: &str) -> Self {
        match value {
            "Initialized" => Self::Initialized,
            "Changed" => Self::Changed,
            "Deleted" => Self::Deleted,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl ItemSection {
    const fn bit(self) -> u8 {
        match self {
            Self::Source => 1,
            Self::Key => 2,
            Self::Data => 4,
        }
    }
}

impl ItemList {
    fn contains_name(&self, name: &str) -> bool {
        self.simple.iter().any(|item| item.name == name)
            || self.element.iter().any(|item| item.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_namespace_aware_notification_content() {
        let xml = br#"
            <e:PullMessagesResponse
                xmlns:e="http://www.onvif.org/ver10/events/wsdl"
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:topics="http://www.onvif.org/ver10/topics"
                xmlns:analytics="urn:vendor:analytics">
                <n:NotificationMessage>
                    <n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">
                        topics:RuleEngine/CellMotionDetector/Motion
                    </n:Topic>
                    <n:Message>
                        <t:Message UtcTime="2026-09-04T12:34:56.789Z" PropertyOperation="Changed">
                            <t:Source>
                                <t:SimpleItem Name="VideoSourceConfigurationToken" Value="source-1"/>
                            </t:Source>
                            <t:Key>
                                <t:SimpleItem Name="ObjectId" Value="7"/>
                            </t:Key>
                            <t:Data>
                                <t:SimpleItem Name="IsMotion" Value="true"/>
                                <t:ElementItem Name="Object">
                                    <analytics:Object token="7">
                                        <analytics:Class Type="Person">person</analytics:Class>
                                    </analytics:Object>
                                </t:ElementItem>
                            </t:Data>
                        </t:Message>
                    </n:Message>
                </n:NotificationMessage>
            </e:PullMessagesResponse>
        "#;

        let notifications = parse_notifications(xml).expect("notification must parse");
        assert_eq!(notifications.len(), 1);

        let notification = &notifications[0];
        assert_eq!(
            notification.topic.path[0].namespace_uri.as_deref(),
            Some("http://www.onvif.org/ver10/topics")
        );
        assert_eq!(notification.topic.path[0].local_name, "RuleEngine");
        assert_eq!(notification.topic.path[1].namespace_uri, None);
        assert_eq!(notification.topic.path[1].local_name, "CellMotionDetector");
        assert_eq!(notification.topic.path[2].namespace_uri, None);
        assert_eq!(notification.topic.path[2].local_name, "Motion");
        assert_eq!(
            notification.utc_time.to_rfc3339(),
            "2026-09-04T12:34:56.789+00:00"
        );
        assert_eq!(
            notification.property_operation,
            Some(PropertyOperation::Changed)
        );
        assert_eq!(
            notification.source.simple,
            [SimpleItem::new("VideoSourceConfigurationToken", "source-1")]
        );
        assert_eq!(notification.key.simple, [SimpleItem::new("ObjectId", "7")]);
        assert_eq!(
            notification.data.simple,
            [SimpleItem::new("IsMotion", "true")]
        );

        let element = &notification.data.element[0];
        assert_eq!(element.name, "Object");
        assert_eq!(
            element.value.name.namespace_uri.as_deref(),
            Some("urn:vendor:analytics")
        );
        assert_eq!(element.value.name.local_name, "Object");
        assert_eq!(element.value.attributes[0].name.namespace_uri, None);
        assert_eq!(element.value.attributes[0].name.local_name, "token");
        assert_eq!(element.value.attributes[0].value, "7");
        assert_eq!(element.value.children[0].name.local_name, "Class");
        assert_eq!(element.value.children[0].attributes[0].value, "Person");
        assert_eq!(element.value.children[0].text, "person");
    }

    #[test]
    fn rejects_duplicate_topics() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Topic Dialect="concrete">a:Tamper</n:Topic>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
            </n:NotificationMessage>
        "#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::InvalidStructure(_))
        ));
    }

    #[test]
    fn rejects_duplicate_messages() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:57Z"/></n:Message>
            </n:NotificationMessage>
        "#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::InvalidStructure(_))
        ));
    }

    #[test]
    fn normalizes_qualified_topic_segments() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics:root"
                xmlns:b="urn:topics:child">
                <n:Topic Dialect="concrete">a:Root/b:Child/Leaf</n:Topic>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
            </n:NotificationMessage>
        "#;

        let topic = &parse_notifications(xml).unwrap()[0].topic;
        assert_eq!(
            topic.path[0].namespace_uri.as_deref(),
            Some("urn:topics:root")
        );
        assert_eq!(
            topic.path[1].namespace_uri.as_deref(),
            Some("urn:topics:child")
        );
        assert_eq!(topic.path[2].namespace_uri, None);
        assert_eq!(topic.path[2].local_name, "Leaf");
    }

    #[test]
    fn rejects_unbound_topic_prefix() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Root/missing:Child</n:Topic>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
            </n:NotificationMessage>
        "#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::InvalidStructure(_))
        ));
    }

    #[test]
    fn preserves_unknown_property_operation() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message>
                    <t:Message UtcTime="2026-09-04T12:34:56Z" PropertyOperation="VendorState"/>
                </n:Message>
            </n:NotificationMessage>
        "#;

        let notification = &parse_notifications(xml).unwrap()[0];
        assert_eq!(
            notification.property_operation,
            Some(PropertyOperation::Other("VendorState".to_owned()))
        );
    }

    #[test]
    fn normalizes_schema_datetime_offset_to_utc() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message><t:Message UtcTime="2026-09-04T14:34:56+02:00"/></n:Message>
            </n:NotificationMessage>
        "#;

        let notification = &parse_notifications(xml).unwrap()[0];
        assert_eq!(
            notification.utc_time.to_rfc3339(),
            "2026-09-04T12:34:56+00:00"
        );
    }

    #[test]
    fn preserves_empty_simple_item_value() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message>
                    <t:Message UtcTime="2026-09-04T12:34:56Z">
                        <t:Data><t:SimpleItem Name="State" Value=""/></t:Data>
                    </t:Message>
                </n:Message>
            </n:NotificationMessage>
        "#;

        let notification = &parse_notifications(xml).unwrap()[0];
        assert_eq!(notification.data.simple, [SimpleItem::new("State", "")]);
    }

    #[test]
    fn parses_every_notification_in_a_batch() {
        let xml = br#"
            <root xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:NotificationMessage>
                    <n:Topic Dialect="concrete">a:Motion</n:Topic>
                    <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
                </n:NotificationMessage>
                <n:NotificationMessage>
                    <n:Topic Dialect="concrete">a:Tamper</n:Topic>
                    <n:Message><t:Message UtcTime="2026-09-04T12:34:57Z"/></n:Message>
                </n:NotificationMessage>
            </root>
        "#;

        let notifications = parse_notifications(xml).unwrap();
        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0].topic.path[0].local_name, "Motion");
        assert_eq!(notifications[1].topic.path[0].local_name, "Tamper");
    }

    #[test]
    fn rejects_payload_above_limit() {
        let xml = vec![b' '; NOTIFICATION_XML_SIZE_BYTES_MAX + 1];

        assert!(matches!(
            parse_notifications(&xml),
            Err(NotificationParseError::PayloadTooLarge { actual, maximum })
                if actual == NOTIFICATION_XML_SIZE_BYTES_MAX + 1
                    && maximum == NOTIFICATION_XML_SIZE_BYTES_MAX
        ));
    }

    #[test]
    fn rejects_xml_above_depth_limit() {
        let mut xml = "<root>".repeat(XML_DEPTH_MAX + 1);
        xml.push_str(&"</root>".repeat(XML_DEPTH_MAX + 1));

        assert!(matches!(
            parse_notifications(xml.as_bytes()),
            Err(NotificationParseError::DepthExceeded {
                maximum: XML_DEPTH_MAX
            })
        ));
    }

    #[test]
    fn rejects_document_type_declarations() {
        let xml = br#"<!DOCTYPE root><root/>"#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::DocumentType)
        ));
    }

    #[test]
    fn rejects_item_count_above_limit() {
        let items = (0..=ITEM_COUNT_MAX)
            .map(|index| format!(r#"<t:SimpleItem Name="item-{index}" Value="value"/>"#))
            .collect::<String>();
        let xml = format!(
            r#"<n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message>
                    <t:Message UtcTime="2026-09-04T12:34:56Z">
                        <t:Data>{items}</t:Data>
                    </t:Message>
                </n:Message>
            </n:NotificationMessage>"#
        );

        assert!(matches!(
            parse_notifications(xml.as_bytes()),
            Err(NotificationParseError::CountExceeded {
                kind: "item count",
                maximum: ITEM_COUNT_MAX
            })
        ));
    }

    #[test]
    fn rejects_misnested_topic() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <wrapper><n:Topic Dialect="concrete">a:Motion</n:Topic></wrapper>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
            </n:NotificationMessage>
        "#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::Missing("Topic"))
        ));
    }

    #[test]
    fn rejects_misnested_message() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <wrapper><t:Message UtcTime="2026-09-04T12:34:56Z"/></wrapper>
            </n:NotificationMessage>
        "#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::Missing("Message"))
        ));
    }

    #[test]
    fn trims_topic_segment_whitespace() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete"> a:Root / Child / Motion </n:Topic>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
            </n:NotificationMessage>
        "#;

        let topic = &parse_notifications(xml).unwrap()[0].topic;
        assert_eq!(topic.path[0].local_name, "Root");
        assert_eq!(topic.path[1].local_name, "Child");
        assert_eq!(topic.path[2].local_name, "Motion");
    }

    #[test]
    fn rejects_empty_item_names() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message>
                    <t:Message UtcTime="2026-09-04T12:34:56Z">
                        <t:Data><t:SimpleItem Name="" Value="true"/></t:Data>
                    </t:Message>
                </n:Message>
            </n:NotificationMessage>
        "#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::InvalidStructure(_))
        ));
    }

    #[test]
    fn parses_message_without_optional_item_sections() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
            </n:NotificationMessage>
        "#;

        let notification = &parse_notifications(xml).unwrap()[0];
        assert_eq!(notification.source, ItemList::default());
        assert_eq!(notification.key, ItemList::default());
        assert_eq!(notification.data, ItemList::default());
    }

    #[test]
    fn rejects_notification_count_above_limit() {
        let notification = r#"
            <n:NotificationMessage>
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
            </n:NotificationMessage>
        "#;
        let xml = format!(
            r#"<root xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">{}{}</root>"#,
            notification.repeat(NOTIFICATION_COUNT_MAX),
            notification
        );

        assert!(matches!(
            parse_notifications(xml.as_bytes()),
            Err(NotificationParseError::CountExceeded {
                kind: "notification count",
                maximum: NOTIFICATION_COUNT_MAX
            })
        ));
    }

    #[test]
    fn rejects_topic_segment_count_above_limit() {
        let topic = format!("a:Root{}", "/Child".repeat(TOPIC_SEGMENT_COUNT_MAX));
        let xml = format!(
            r#"<n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">{topic}</n:Topic>
                <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z"/></n:Message>
            </n:NotificationMessage>"#
        );

        assert!(matches!(
            parse_notifications(xml.as_bytes()),
            Err(NotificationParseError::CountExceeded {
                kind: "topic segment count",
                maximum: TOPIC_SEGMENT_COUNT_MAX
            })
        ));
    }

    #[test]
    fn rejects_element_node_count_above_limit() {
        let children = "<v:Child/>".repeat(ELEMENT_NODE_COUNT_MAX);
        let xml = format!(
            r#"<n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics"
                xmlns:v="urn:values">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message>
                    <t:Message UtcTime="2026-09-04T12:34:56Z">
                        <t:Data>
                            <t:ElementItem Name="Value"><v:Root>{children}</v:Root></t:ElementItem>
                        </t:Data>
                    </t:Message>
                </n:Message>
            </n:NotificationMessage>"#
        );

        assert!(matches!(
            parse_notifications(xml.as_bytes()),
            Err(NotificationParseError::CountExceeded {
                kind: "ElementItem node count",
                maximum: ELEMENT_NODE_COUNT_MAX
            })
        ));
    }

    #[test]
    fn rejects_malformed_xml() {
        let xml = br#"<n:NotificationMessage xmlns:n="http://docs.oasis-open.org/wsn/b-2">"#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::Xml(_))
        ));
    }

    #[test]
    fn rejects_duplicate_item_sections() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message>
                    <t:Message UtcTime="2026-09-04T12:34:56Z">
                        <t:Data><t:SimpleItem Name="State" Value="true"/></t:Data>
                        <t:Data><t:SimpleItem Name="State" Value="false"/></t:Data>
                    </t:Message>
                </n:Message>
            </n:NotificationMessage>
        "#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::InvalidStructure(_))
        ));
    }

    #[test]
    fn rejects_duplicate_item_names() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message>
                    <t:Message UtcTime="2026-09-04T12:34:56Z">
                        <t:Data>
                            <t:SimpleItem Name="State" Value="true"/>
                            <t:SimpleItem Name="State" Value="false"/>
                        </t:Data>
                    </t:Message>
                </n:Message>
            </n:NotificationMessage>
        "#;

        assert!(matches!(
            parse_notifications(xml),
            Err(NotificationParseError::InvalidStructure(_))
        ));
    }

    #[test]
    fn rejects_duplicate_nested_attributes() {
        let xml = br#"
            <n:NotificationMessage
                xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:a="urn:topics"
                xmlns:v="urn:values">
                <n:Topic Dialect="concrete">a:Motion</n:Topic>
                <n:Message>
                    <t:Message UtcTime="2026-09-04T12:34:56Z">
                        <t:Data>
                            <t:ElementItem Name="Value"><v:Object token="1" token="2"/></t:ElementItem>
                        </t:Data>
                    </t:Message>
                </n:Message>
            </n:NotificationMessage>
        "#;

        assert!(parse_notifications(xml).is_err());
    }
}
