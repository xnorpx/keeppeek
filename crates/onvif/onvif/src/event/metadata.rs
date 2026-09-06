//! Bounded ONVIF metadata parsing without transport or host state.

mod classification;
mod frame;
mod geometry;
mod recognition;
mod tree;

use std::fmt;

use chrono::{DateTime, Utc};
use xmltree::{Element, XMLNode};

use super::ONVIF_SCHEMA_NAMESPACE as SCHEMA;
use super::{Notification, ProtocolError, parse_notifications_with_time, xml};

const XML_SIZE_BYTES_MAX: usize = 1024 * 1024;
const STRING_SIZE_BYTES_MAX: usize = 256;

/// Notifications and analytics updates from one plain XML ONVIF metadata document.
///
/// Frames are partial updates, not complete snapshots. Missing objects do not imply
/// deletion. Use explicit deletes and application-owned timeouts for disappearance.
/// `Debug` omits notification payloads and recognition text.
///
/// See the [ONVIF metadata schema](https://www.onvif.org/ver10/schema/metadatastream.xsd).
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct Metadata {
    /// Valid notifications in document order, with at most 256 messages per document.
    ///
    /// Structured values use the original event [`super::XmlElement`] type. Attributes
    /// are sorted by expanded name; XML attribute order has no semantic meaning.
    pub notifications: Vec<Notification>,
    /// Invalid individual notifications omitted from an otherwise valid document.
    pub invalid_messages: u32,
    /// At most 64 partial analytics frames in document order.
    pub frames: Vec<Frame>,
}

/// A partial analytics update at a peer-supplied UTC timestamp.
///
/// An empty frame does not clear tracked objects. No completeness or timeout is
/// inferred. The caller owns tracking state and the lifetime of missing objects.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct Frame {
    /// Explicit frame time, converted to UTC without consulting the host clock.
    ///
    /// A missing timezone suffix means UTC, as in ONVIF's `UtcTime` examples.
    pub utc_time: DateTime<Utc>,
    /// At most 128 explicitly reported object updates.
    pub objects: Vec<Object>,
    /// At most 128 IDs from `ObjectTree/Delete` attributes, never inferred from absence.
    pub deleted_ids: Vec<String>,
    /// Explicit `Frame/@Source`, at most 256 UTF-8 bytes, naming the analytics module.
    pub source: Option<String>,
}

/// An explicitly reported object with optional classification and appearance data.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct Object {
    /// Opaque `ObjectId` attribute, nonempty and at most 256 UTF-8 bytes.
    pub id: String,
    /// Explicit recognized classification; unknown values stay unclassified.
    ///
    /// The highest-likelihood class wins, including unknown classes. Scored classes
    /// rank above unscored classes. Equal scores for different categories produce
    /// no classification. Typed appearance fields apply only without class candidates.
    pub class: Option<ObjectClass>,
    /// Optional finite classification likelihood in the inclusive range `0..=1`.
    ///
    /// The winning score is retained even for unknown or tied classes. Recognition
    /// likelihoods and typed appearance fields do not fabricate a class confidence.
    pub confidence: Option<f32>,
    /// Optional bounding box in normalized top-left image coordinates.
    pub bbox: Option<BoundingBox>,
    /// Explicit recognition text, at most 256 UTF-8 bytes and redacted from `Debug`.
    ///
    /// Reads `Appearance/LicensePlateInfo/PlateNumber` or `Appearance/BarcodeInfo/Data`.
    /// Plate text takes precedence when both exist. Other text and images are omitted.
    pub text: Option<String>,
}

/// A recognized object category, never inferred from motion or geometry alone.
///
/// Supports legacy `ClassCandidate`, modern `Class/Type`, and explicit typed
/// appearance fields. `Package`, `Person`, and `HumanBody` class labels are explicit
/// compatibility values, not members of the current ONVIF `ObjectType` enumeration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ObjectClass {
    Person,
    Vehicle,
    Animal,
    Face,
    LicensePlate,
    Package,
}

/// A finite, nonempty rectangle normalized to top-left image coordinates.
///
/// Parsed components are in `0..=1`, with `x + width <= 1` and `y + height <= 1`.
/// Frame and appearance transforms map coordinates by `parent = local * scale + translate`.
/// Missing translation is zero; missing scale is one. Each frame starts with identity.
/// After these transforms, ONVIF Y-up coordinates become `x = (left + 1) / 2`,
/// `y = (1 - top) / 2`, `width = (right - left) / 2`, `height = (top - bottom) / 2`.
/// Edge ordering is checked after transformation, including negative scale factors.
/// Values outside the image and boxes that collapse at `f32` precision are rejected.
///
/// See [Spatial Relation in the ONVIF specification](https://github.com/onvif/specs/blob/development/doc/Analytics.xml)
/// and the [coordinate schema](https://www.onvif.org/ver10/schema/common.xsd).
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Metadata {
    /// Parses one plain XML metadata document without network or clock access.
    ///
    /// Decompression and stream reassembly belong to the caller. Namespace prefixes
    /// do not affect parsing. Invalid individual notifications do not hide neighbors.
    /// Only direct `VideoAnalytics/Frame` and `Event/NotificationMessage` paths are
    /// interpreted. `Event` has the ONVIF `EventStream` schema type. PTZ streams,
    /// aspect-ratio overrides, object-tree merges or renames, and other extensions
    /// are not interpreted. Unknown analytics fields are not retained.
    ///
    /// XML work is bounded by 1 MiB, 32 levels, and 8192 elements. Decoded attributes
    /// are limited to 4 KiB and direct text per element to 64 KiB. Each notification
    /// uses the existing parser's 256 KiB limit. The 256-message count includes
    /// invalid messages. Each object accepts at most 32 class candidates and 32
    /// vehicle descriptors. Resource-limit failures never truncate analytics lists.
    ///
    /// # Errors
    /// Rejects documents over 1 MiB, DTDs, invalid XML, excessive depth or node counts,
    /// unrelated roots or namespaces, and more than 256 notification messages.
    /// Invalid analytics data or exceeded frame, object, deletion, or string limits
    /// reject the document. Only individual notification failures are recoverable.
    /// Error text never includes peer payloads.
    pub fn parse(bytes: &[u8]) -> Result<Self, ProtocolError> {
        Self::parse_with_time(bytes, None)
    }

    /// Parses metadata with receipt-time fallback for notification timestamps only.
    ///
    /// Missing or invalid unqualified notification timestamps use `received_time` and
    /// do not increase `invalid_messages`. Analytics frame timestamps remain strict.
    /// This function performs no network or clock access.
    ///
    /// # Errors
    /// Rejects the same document, analytics, and resource-limit failures as [`Self::parse`].
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
        let root = tree::parse(bytes)?;
        if !xml::matches(&root, SCHEMA, "MetadataStream") {
            return Err(ProtocolError("wrong metadata root"));
        }
        let mut metadata = Self {
            notifications: Vec::new(),
            invalid_messages: 0,
            frames: frame::parse(&root)?,
        };
        let messages = root
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .filter(|node| xml::matches(node, SCHEMA, "Event"))
            .flat_map(|stream| stream.children.iter().filter_map(XMLNode::as_element))
            .filter(|node| xml::matches(node, xml::NOTIFY, "NotificationMessage"));
        for (index, message) in messages.enumerate() {
            if index >= super::NOTIFICATION_COUNT_MAX {
                return Err(ProtocolError("metadata message count exceeded"));
            }
            match parse_message(message, received_time) {
                Ok(notification) => metadata.notifications.push(notification),
                Err(_) => metadata.invalid_messages += 1,
            }
        }
        Ok(metadata)
    }
}

fn parse_message(
    element: &Element,
    received_time: Option<DateTime<Utc>>,
) -> Result<Notification, ProtocolError> {
    let mut messages =
        parse_notifications_with_time(xml::encode(element)?.as_bytes(), received_time)
            .map_err(|_| ProtocolError("invalid metadata notification"))?;
    if messages.len() != 1 {
        return Err(ProtocolError("invalid metadata notification count"));
    }
    let mut notification = messages
        .pop()
        .ok_or(ProtocolError("missing metadata notification"))?;
    canonical_attributes(&mut notification);
    Ok(notification)
}

fn canonical_attributes(notification: &mut Notification) {
    let mut pending: Vec<_> = [
        &mut notification.source,
        &mut notification.key,
        &mut notification.data,
    ]
    .into_iter()
    .flat_map(|section| section.element.iter_mut().map(|item| &mut item.value))
    .collect();
    while let Some(element) = pending.pop() {
        element.attributes.sort_unstable_by(|left, right| {
            (&left.name.namespace_uri, &left.name.local_name)
                .cmp(&(&right.name.namespace_uri, &right.name.local_name))
        });
        pending.extend(&mut element.children);
    }
}

impl fmt::Debug for Metadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Metadata")
            .field("notification_count", &self.notifications.len())
            .field("invalid_messages", &self.invalid_messages)
            .field("frames", &self.frames)
            .finish()
    }
}

impl fmt::Debug for Frame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Frame")
            .field("utc_time", &self.utc_time)
            .field("objects", &self.objects)
            .field("deleted_count", &self.deleted_ids.len())
            .field("source", &self.source.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl fmt::Debug for Object {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Object")
            .field("id", &"[REDACTED]")
            .field("class", &self.class)
            .field("confidence", &self.confidence)
            .field("bbox", &self.bbox)
            .field("text", &self.text.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}
