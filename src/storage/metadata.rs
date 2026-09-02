use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::HashSet, fmt};

const MAX_ICON_DIAGNOSTIC_BYTES: usize = 64;

/// Metadata for one binary or textual event attachment.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EventAttachment {
    pub id: String,
    pub attachment_type: String,
    pub content_type: String,
    pub byte_len: Option<u64>,
    pub ordinal: u32,
    pub timestamp_ms: Option<i64>,
    pub text: Option<String>,
}

/// Explains why an explicit canonical attachment reference was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalAttachmentError {
    NotFound,
    DuplicateId,
    UnsupportedImage,
}

impl fmt::Display for CanonicalAttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotFound => "canonical attachment was not found in the event revision",
            Self::DuplicateId => "canonical attachment ID is not unique in the event revision",
            Self::UnsupportedImage => "canonical attachment is not a supported image",
        })
    }
}

impl std::error::Error for CanonicalAttachmentError {}

/// A normalized icon and optional bounded rejected producer value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventIcon {
    pub key: &'static str,
    pub rejected: Option<String>,
}

/// Selects one representative image for a complete event revision.
///
/// An explicit reference wins when it names one unique supported image. Otherwise,
/// snapshots precede story frames and retained thumbnails. Candidates within a type
/// are ordered by ordinal, capture timestamp, and stable attachment ID. Missing
/// timestamps sort after known timestamps.
pub fn canonical_event_attachment<'a>(
    attachments: &'a [EventAttachment],
    explicit_id: Option<&str>,
) -> Result<Option<&'a EventAttachment>, CanonicalAttachmentError> {
    let mut attachment_ids = HashSet::with_capacity(attachments.len());
    if attachments
        .iter()
        .any(|attachment| !attachment_ids.insert(attachment.id.as_str()))
    {
        return Err(CanonicalAttachmentError::DuplicateId);
    }
    if let Some(explicit_id) = explicit_id {
        let mut matches = attachments
            .iter()
            .filter(|attachment| attachment.id == explicit_id);
        let attachment = matches.next().ok_or(CanonicalAttachmentError::NotFound)?;
        if matches.next().is_some() {
            return Err(CanonicalAttachmentError::DuplicateId);
        }
        if !is_supported_event_image(attachment) {
            return Err(CanonicalAttachmentError::UnsupportedImage);
        }
        return Ok(Some(attachment));
    }

    for attachment_type in ["snapshot", "story-frame", "thumbnail"] {
        if let Some(attachment) = attachments
            .iter()
            .filter(|attachment| {
                attachment.attachment_type == attachment_type
                    && is_supported_event_image(attachment)
            })
            .min_by(|left, right| compare_attachments(left, right))
        {
            return Ok(Some(attachment));
        }
    }
    Ok(None)
}

/// Returns whether an attachment is eligible as an event preview image.
pub fn is_supported_event_image(attachment: &EventAttachment) -> bool {
    matches!(
        attachment.attachment_type.as_str(),
        "snapshot" | "story-frame" | "thumbnail"
    ) && matches!(
        attachment.content_type.as_str(),
        "image/jpeg" | "image/png" | "image/webp"
    )
}

/// Normalizes an untrusted producer icon key to the documented allowlist.
pub fn event_icon(producer_key: Option<&str>, event_type: &str) -> EventIcon {
    let fallback = fallback_event_icon(event_type);
    producer_key.map_or(
        EventIcon {
            key: fallback,
            rejected: None,
        },
        |key| {
            allowed_icon(key).map_or_else(
                || EventIcon {
                    key: fallback,
                    rejected: Some(icon_diagnostic(key)),
                },
                |key| EventIcon {
                    key,
                    rejected: None,
                },
            )
        },
    )
}

fn compare_attachments(left: &EventAttachment, right: &EventAttachment) -> Ordering {
    left.ordinal
        .cmp(&right.ordinal)
        .then_with(|| {
            left.timestamp_ms
                .unwrap_or(i64::MAX)
                .cmp(&right.timestamp_ms.unwrap_or(i64::MAX))
        })
        .then_with(|| left.id.cmp(&right.id))
}

fn allowed_icon(key: &str) -> Option<&'static str> {
    match key {
        "event" => Some("event"),
        "person" => Some("person"),
        "vehicle" => Some("vehicle"),
        "animal" => Some("animal"),
        "package" => Some("package"),
        "motion" => Some("motion"),
        "doorbell" => Some("doorbell"),
        "sound" => Some("sound"),
        "story" => Some("story"),
        "alert" => Some("alert"),
        _ => None,
    }
}

fn fallback_event_icon(event_type: &str) -> &'static str {
    let event_type = event_type.trim();
    if matches_ignore_ascii_case(event_type, &["person", "human", "face"]) {
        "person"
    } else if matches_ignore_ascii_case(event_type, &["vehicle", "car", "truck"]) {
        "vehicle"
    } else if matches_ignore_ascii_case(event_type, &["animal", "pet"]) {
        "animal"
    } else if event_type.eq_ignore_ascii_case("package") {
        "package"
    } else if event_type.eq_ignore_ascii_case("motion") {
        "motion"
    } else if event_type.eq_ignore_ascii_case("doorbell") {
        "doorbell"
    } else if matches_ignore_ascii_case(event_type, &["sound", "audio"]) {
        "sound"
    } else if event_type.eq_ignore_ascii_case("story") {
        "story"
    } else if event_type.contains("outage") || event_type.contains("unavailable") {
        "alert"
    } else {
        "event"
    }
}

fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn icon_diagnostic(value: &str) -> String {
    value
        .chars()
        .take(MAX_ICON_DIAGNOSTIC_BYTES)
        .map(|character| {
            if character.is_ascii_graphic() {
                character
            } else {
                '?'
            }
        })
        .collect()
}

/// Origin of an event shown in recorded-video review.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Camera,
    KeepPeek,
}

impl EventSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::KeepPeek => "keeppeek",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "camera" => Some(Self::Camera),
            "keeppeek" => Some(Self::KeepPeek),
            _ => None,
        }
    }
}

/// A camera or KeepPeek detection event aligned to the recording clock.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TimelineEvent {
    pub id: String,
    pub revision: u64,
    pub camera_id: String,
    pub stream: Option<String>,
    pub source: EventSource,
    pub kind: String,
    pub start_time_ms: i64,
    pub end_time_ms: Option<i64>,
    pub confidence: Option<f64>,
    pub bbox: Option<[f32; 4]>,
    pub bbox_attachment_id: Option<String>,
    pub zone: Option<String>,
    pub text: Option<String>,
    pub payload: Option<serde_json::Map<String, serde_json::Value>>,
    pub attachments: Vec<EventAttachment>,
    pub canonical_attachment_id: Option<String>,
    pub icon_key: String,
    pub rejected_icon_key: Option<String>,
    pub thumbnail_filename: Option<String>,
}

impl TimelineEvent {
    /// Returns the validated canonical descriptor for this revision.
    pub fn canonical_attachment(&self) -> Option<&EventAttachment> {
        let canonical_id = self.canonical_attachment_id.as_deref()?;
        self.attachments
            .iter()
            .find(|attachment| attachment.id == canonical_id)
    }

    /// Returns whether the canonical image bytes are retained locally.
    pub fn canonical_image_available(&self) -> bool {
        self.canonical_attachment().is_some_and(|attachment| {
            is_supported_event_image(attachment) && self.thumbnail_filename.is_some()
        })
    }

    /// Returns whether the bounding box belongs to the canonical image coordinates.
    pub fn canonical_image_owns_bbox(&self) -> bool {
        self.bbox.is_some()
            && self.bbox_attachment_id.as_deref() == self.canonical_attachment_id.as_deref()
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;

    fn attachment(
        id: &str,
        attachment_type: &str,
        content_type: &str,
        ordinal: u32,
        timestamp_ms: Option<i64>,
    ) -> EventAttachment {
        EventAttachment {
            id: id.to_owned(),
            attachment_type: attachment_type.to_owned(),
            content_type: content_type.to_owned(),
            byte_len: Some(12),
            ordinal,
            timestamp_ms,
            text: None,
        }
    }

    #[test]
    fn canonical_policy_handles_absent_and_single_images() {
        assert_eq!(canonical_event_attachment(&[], None), Ok(None));
        let attachments = [attachment(
            "snapshot-0",
            "snapshot",
            "image/jpeg",
            0,
            Some(100),
        )];
        assert_eq!(
            canonical_event_attachment(&attachments, None)
                .unwrap()
                .map(|attachment| attachment.id.as_str()),
            Some("snapshot-0")
        );
    }

    #[test]
    fn canonical_policy_applies_type_and_stable_tie_precedence() {
        let attachments = [
            attachment("story", "story-frame", "image/jpeg", 0, Some(1)),
            attachment("late", "snapshot", "image/jpeg", 0, Some(20)),
            attachment("z-stable", "snapshot", "image/jpeg", 0, Some(10)),
            attachment("a-stable", "snapshot", "image/jpeg", 0, Some(10)),
            attachment("first-ordinal", "snapshot", "image/jpeg", 1, Some(0)),
        ];
        assert_eq!(
            canonical_event_attachment(&attachments, None)
                .unwrap()
                .map(|attachment| attachment.id.as_str()),
            Some("a-stable")
        );
    }

    #[test]
    fn canonical_policy_honors_valid_explicit_references() {
        let attachments = [
            attachment("snapshot", "snapshot", "image/jpeg", 0, Some(10)),
            attachment("story", "story-frame", "image/webp", 3, Some(30)),
        ];
        assert_eq!(
            canonical_event_attachment(&attachments, Some("story"))
                .unwrap()
                .map(|attachment| attachment.id.as_str()),
            Some("story")
        );
    }

    #[test]
    fn canonical_policy_rejects_invalid_explicit_references_and_mime_types() {
        let unsupported = [attachment("text", "snapshot", "text/plain", 0, None)];
        assert_eq!(
            canonical_event_attachment(&unsupported, Some("missing")),
            Err(CanonicalAttachmentError::NotFound)
        );
        assert_eq!(
            canonical_event_attachment(&unsupported, Some("text")),
            Err(CanonicalAttachmentError::UnsupportedImage)
        );
        assert_eq!(canonical_event_attachment(&unsupported, None), Ok(None));

        let duplicates = [
            attachment("duplicate", "snapshot", "image/jpeg", 1, None),
            attachment("duplicate", "snapshot", "image/jpeg", 2, None),
        ];
        assert_eq!(
            canonical_event_attachment(&duplicates, Some("duplicate")),
            Err(CanonicalAttachmentError::DuplicateId)
        );
        assert_eq!(
            canonical_event_attachment(&duplicates, None),
            Err(CanonicalAttachmentError::DuplicateId)
        );
    }

    #[test]
    fn icon_keys_are_allowlisted_and_rejections_are_bounded() {
        assert_eq!(event_icon(Some("vehicle"), "person").key, "vehicle");
        assert_eq!(event_icon(None, "person").key, "person");
        assert_eq!(
            event_icon(Some("<svg onload=alert(1)>"), "story").key,
            "story"
        );

        for key in [
            "javascript:alert(1)",
            "https://example.com/icon.svg",
            "class-name text-red-500",
            "\n\r\0",
            &"x".repeat(256),
            "person\u{202e}gpj",
        ] {
            let icon = event_icon(Some(key), "unknown");
            assert_eq!(icon.key, "event");
            let rejected = icon.rejected.unwrap();
            assert!(rejected.len() <= MAX_ICON_DIAGNOSTIC_BYTES);
            assert!(rejected.bytes().all(|byte| byte.is_ascii_graphic()));
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CameraMetadata {
    pub camera_id: String,

    #[serde(default)]
    pub zones: Vec<Zone>,

    #[serde(default)]
    pub static_objects: Vec<StaticObject>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Zone {
    pub name: String,
    pub polygon: Vec<[f32; 2]>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StaticObject {
    pub name: String,
    pub bbox: [f32; 4],
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Event {
    pub event_type: String,
    pub confidence: u8,
    pub bbox: [f32; 4],

    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u16>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<u16>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Keyframe {
    pub timestamp: u64,
    pub file_offset: u64,

    #[serde(default)]
    pub events: Vec<Event>,
}
