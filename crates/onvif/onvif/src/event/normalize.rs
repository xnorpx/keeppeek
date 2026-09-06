//! Pure, bounded topic and item normalization without lifecycle or storage state.

mod classification;
mod details;
mod identity;
mod items;
mod topics;

use std::fmt;

use chrono::{DateTime, Utc};

use super::{BoundingBox, Notification, ObjectClass, PropertyOperation, ProtocolError};

const TOPICS: &str = "http://www.onvif.org/ver10/topics";

pub(super) fn topic_kind(topic: &super::Topic) -> Result<Option<Kind>, ProtocolError> {
    Ok(topics::lookup(topic)?.and_then(|mapping| mapping.classified_kind(None)))
}

/// A recognized camera event category with a stable native-event spelling.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Kind {
    /// Motion explicitly reported by a recognized detector.
    Motion,
    /// Tampering with the video source.
    Tamper,
    /// A digital input, without assuming which camera channel it controls.
    DigitalInput,
    /// Audio detected by the source.
    AudioDetected,
    /// Loss of the video signal.
    VideoLoss,
    /// An object crossed a configured line.
    LineCrossing,
    /// An object is inside a configured field.
    Intrusion,
    /// An object entered a configured region.
    RegionEntry,
    /// An object left a configured region.
    RegionExit,
    /// A loitering rule reported an object.
    Loitering,
    /// Explicit person or human classification.
    Person,
    /// Explicit vehicle classification.
    Vehicle,
    /// Explicit animal classification.
    Animal,
    /// Explicit face detection or recognition.
    Face,
    /// Explicit license plate detection or recognition.
    LicensePlate,
    /// Explicit package classification.
    Package,
    /// An object count, without an inferred Boolean alarm state.
    ObjectCount,
    /// A press reported by a recognized doorbell topic.
    DoorbellPress,
}

impl Kind {
    /// Returns the stable snake-case native-event name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Motion => "motion",
            Self::Tamper => "tamper",
            Self::DigitalInput => "digital_input",
            Self::AudioDetected => "audio_detected",
            Self::VideoLoss => "video_loss",
            Self::LineCrossing => "line_crossing",
            Self::Intrusion => "intrusion",
            Self::RegionEntry => "region_entry",
            Self::RegionExit => "region_exit",
            Self::Loitering => "loitering",
            Self::Person => "person",
            Self::Vehicle => "vehicle",
            Self::Animal => "animal",
            Self::Face => "face",
            Self::LicensePlate => "license_plate",
            Self::Package => "package",
            Self::ObjectCount => "object_count",
            Self::DoorbellPress => "doorbell_press",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<ObjectClass> for Kind {
    fn from(class: ObjectClass) -> Self {
        match class {
            ObjectClass::Person => Self::Person,
            ObjectClass::Vehicle => Self::Vehicle,
            ObjectClass::Animal => Self::Animal,
            ObjectClass::Face => Self::Face,
            ObjectClass::LicensePlate => Self::LicensePlate,
            ObjectClass::Package => Self::Package,
        }
    }
}

/// One explicitly reported detection, independent of transport and host state.
///
/// `Debug` redacts source, rule, identity, text, and opaque property operations.
/// Fields describe only this notification. They do not establish a tracked object,
/// an interval, a deduplication decision, or a camera-to-channel association.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct Detection {
    /// The recognized topic category, refined only by explicit object classification.
    pub kind: Kind,
    /// Explicit property state, or no long-lived state for a point event.
    ///
    /// Deleted properties return `Some(false)` even without a state item. Line
    /// crossing, region entry/exit, recognition, visitor, standard object-detection,
    /// and count notifications return `None`. Classification preserves the topic's
    /// point/property semantics; a person classification does not open a point event.
    pub active: Option<bool>,
    /// An explicit camera source token from Source, without channel inference.
    ///
    /// The first nonempty field wins in this order: VideoSourceConfigurationToken,
    /// VideoSourceToken, VideoSource, Source, ChannelID, ChannelId, channelID, Channel.
    /// These identifiers can name different ONVIF resources. The caller must resolve
    /// the token against camera configuration. InputToken and audio tokens are not
    /// camera source tokens. Key and Data never supply this field.
    pub source: Option<String>,
    /// An explicit Rule, RuleName, RuleToken, RuleId, or RuleID from Source or Key.
    ///
    /// Multiple spellings must agree. Empty values do not provide a rule.
    pub rule: Option<String>,
    /// A canonical topic plus Source/Key identity of at most 4096 UTF-8 bytes.
    ///
    /// Items and XML attributes are sorted. XML child order is retained. Topic
    /// children without a namespace inherit the root namespace. Data, timestamp,
    /// operation, and inferred kind are excluded. Compare this opaque value only
    /// within one camera; it is not a global identifier and must not be logged.
    /// The versioned, length-prefixed encoding preserves sections and item types.
    /// XML text and other values are retained verbatim, including whitespace.
    pub identity: String,
    /// The notification's camera-supplied UTC time, without clock access.
    pub utc_time: DateTime<Utc>,
    /// The original property operation, including Initialized, Deleted, and Other.
    pub operation: Option<PropertyOperation>,
    /// An explicit finite confidence in the inclusive range `0..=1`.
    ///
    /// Reads Data Confidence/Likelihood or the selected class candidate score.
    /// Unknown and tied classes can retain their score. Conflicting explicit scores
    /// are errors. A plate-text likelihood is validated but does not fill this field.
    pub confidence: Option<f32>,
    /// An explicit nonempty bounding box in normalized top-left image coordinates.
    ///
    /// Reads a schema-qualified Data BoundingBox/Rectangle element item or
    /// Object/Appearance/Shape/BoundingBox. ONVIF Y-up edges in `-1..=1` become
    /// top-left `x/y/width/height` in `0..=1`. Out-of-image and collapsed boxes are
    /// rejected, not clamped. Appearance transformations are not applied; a box
    /// that requires one is rejected instead of guessing its coordinate system.
    pub bbox: Option<BoundingBox>,
    /// Explicit recognition text, bounded to 256 UTF-8 bytes.
    ///
    /// Reads Data Text/PlateNumber/LicensePlate or schema-qualified
    /// LicensePlateInfo/PlateNumber, also under Object/Appearance. Images, labels,
    /// enrollment identifiers, and unknown extension text are not projected.
    pub text: Option<String>,
    /// An explicitly reported nonnegative object count.
    ///
    /// Reads Data Count/ObjectCount. A zero count is retained. No count is inferred
    /// from alarm state, ActivePostCount, an object ID, or a list of class labels.
    pub count: Option<u64>,
}

/// Normalizes one recognized ONVIF notification without I/O or lifecycle state.
///
/// Unknown topics and property messages without a recognized state return `None`.
/// Boolean values accept true/false, 1/0, on/off, and active/inactive without case
/// sensitivity. Point notifications have no long-lived state. Explicit false or
/// unknown state suppresses a point notification. Deleted retains its identity
/// without a state item and closes a property with `Some(false)`. Deleted with
/// explicit true state is contradictory. The original notification is not changed.
///
/// Topic matching uses complete, case-sensitive paths in the ONVIF topics namespace.
/// Unknown namespaces, roots, children, and suffixes do not imply motion. The
/// [ONVIF Analytics specification](https://github.com/onvif/specs/blob/development/doc/Analytics.xml)
/// defines the standard rule paths. Exact FieldDetector region/loitering,
/// ObjectDetector category, VideoSource/Tamper, and AudioSource/AudioDetection
/// compatibility paths are also accepted. The Reolink compatibility path
/// RuleEngine/MyRuleDetector/Visitor is a point event, not a general Visitor alias.
/// No vendor namespace aliases are enabled.
///
/// # Topics
/// Paths are relative to `http://www.onvif.org/ver10/topics`.
/// Property paths accept the listed Data state names; aliases must not contradict.
///
/// | Property topic | State names |
/// | --- | --- |
/// | RuleEngine/CellMotionDetector/Motion | IsMotion, State |
/// | RuleEngine/MotionRegionDetector/Motion | State, IsMotion |
/// | VideoSource/MotionAlarm | State, IsMotion |
/// | VideoSource/Tamper | State, IsTamper |
/// | Device/Trigger/DigitalInput | LogicalState, State |
/// | AudioSource/AudioDetection | State, IsSoundDetected, IsSound |
/// | VideoSource/SignalLoss | State, IsSignalLoss |
/// | RuleEngine/FieldDetector/ObjectsInside | IsInside, State |
/// | RuleEngine/FieldDetector/Loitering | State, IsLoitering |
/// | RuleEngine/LoiteringDetector/ObjectIsLoitering | State, IsLoitering |
/// | RuleEngine/ObjectDetector/{category} | State, IsDetected |
///
/// ObjectDetector categories are Person, Human, Vehicle, Animal, Face, LicensePlate,
/// and Package. The separate ObjectDetector/Count path is a count observation.
/// Point paths accept optional State/IsDetected, never a long-lived Boolean state:
/// RuleEngine/LineDetector/Crossed; RuleEngine/FieldDetector/RegionEntrance and
/// RegionExit; RuleEngine/Recognition/Face and LicensePlate;
/// RuleEngine/MyRuleDetector/Visitor; RuleEngine/ObjectDetection/Object; and
/// RuleEngine/CountAggregation/Counter and OccupancyCounter.
/// Count observations require Count/ObjectCount unless Deleted. ObjectDetection/Object
/// requires a recognized, unambiguous class even when Deleted.
///
/// # Classification
/// Reads Data Class/ObjectClass/ClassType, whitespace-separated ClassTypes, or
/// schema-qualified Class and Object/Appearance/Class element items. Class/Type
/// and legacy Class/ClassCandidate are supported. The highest explicit likelihood
/// wins, including unknown labels. Scored candidates rank above unscored ones.
/// Ties between different categories leave the notification unclassified.
/// Explicit Human/HumanBody/Person, Vehicle/Vehical/Car/Bus/Truck/Bicycle/Motorcycle/Bike,
/// Animal, HumanFace/Face, LicensePlate, and Package labels are case-insensitive.
/// Only motion, line crossing, intrusion, region entry/exit, and loitering kinds
/// can be refined. Unknown or ambiguous classes retain those topic-specific kinds.
/// Other kinds are not replaced. Type, geometry, recognition text, and source/key
/// items alone never supply classification evidence.
///
/// # Limits
/// Work accepts at most 32 topic segments, 256 combined Source/Key/Data items,
/// 256 combined XML nodes, 32 XML levels, and 32 attributes per element.
/// Names, class labels, recognition text, and Other operations are at most 256
/// UTF-8 bytes. Item values, attributes, namespaces, and dialects are at most
/// 4 KiB each; XML text is at most 64 KiB per element. Combined input strings
/// are at most 256 KiB. Classification accepts at most 32 candidates.
/// Identity output is at most 4 KiB and is never truncated or hashed.
/// Unknown topics return before item inspection. There is no I/O, clock access,
/// global state, or allocation proportional to unchecked collection lengths.
///
/// # Examples
/// ```
/// use onvif::event::{Notification, ProtocolError, normalize};
///
/// fn category(message: &Notification) -> Result<Option<&'static str>, ProtocolError> {
///     Ok(normalize(message)?.map(|detection| detection.kind.as_str()))
/// }
/// ```
///
/// # Errors
/// Rejects duplicate or empty item names, duplicate XML attributes or interpreted
/// fields, contradictory states/rules/attributes, invalid confidence/count/geometry,
/// structured scalars, transformed object boxes, and any exceeded limit.
/// Error messages contain no peer-supplied values. No partial detection is returned.
pub fn normalize(notification: &Notification) -> Result<Option<Detection>, ProtocolError> {
    let Some(mapping) = topics::lookup(&notification.topic)? else {
        return Ok(None);
    };
    items::validate(notification)?;
    let details = details::parse(&notification.data)?;
    let state = items::state(&notification.data, mapping.states)?;
    let deleted = notification.property_operation == Some(PropertyOperation::Deleted);
    if deleted && state.value == Some(true) {
        return Err(ProtocolError("deleted normalization state is active"));
    }
    let active = if mapping.point {
        if !deleted && (state.unknown || state.value == Some(false)) {
            return Ok(None);
        }
        None
    } else if deleted {
        Some(false)
    } else {
        if state.unknown || state.value.is_none() {
            return Ok(None);
        }
        state.value
    };
    let Some(kind) = mapping.classified_kind(details.class) else {
        return Ok(None);
    };
    if kind == Kind::ObjectCount && details.count.is_none() && !deleted {
        return Ok(None);
    }
    Ok(Some(Detection {
        kind,
        active,
        source: items::source(&notification.source),
        rule: items::rule(notification)?,
        identity: identity::canonical(notification)?,
        utc_time: notification.utc_time,
        operation: notification.property_operation.clone(),
        confidence: details.confidence,
        bbox: details.bbox,
        text: details.text,
        count: details.count,
    }))
}

impl fmt::Debug for Detection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = self.operation.as_ref().map(|operation| match operation {
            PropertyOperation::Initialized => "Initialized",
            PropertyOperation::Changed => "Changed",
            PropertyOperation::Deleted => "Deleted",
            PropertyOperation::Other(_) => "Other([REDACTED])",
        });
        formatter
            .debug_struct("Detection")
            .field("kind", &self.kind)
            .field("active", &self.active)
            .field("source", &self.source.as_ref().map(|_| "[REDACTED]"))
            .field("rule", &self.rule.as_ref().map(|_| "[REDACTED]"))
            .field("identity", &"[REDACTED]")
            .field("utc_time", &self.utc_time)
            .field("operation", &operation)
            .field("confidence", &self.confidence)
            .field("bbox", &self.bbox)
            .field("text", &self.text.as_ref().map(|_| "[REDACTED]"))
            .field("count", &self.count)
            .finish()
    }
}
