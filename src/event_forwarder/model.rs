use super::config::MqttForwarderConfig;
use crate::{
    operational_events::{OperationalTransition, OperationalTransitionKind},
    storage::metadata::TimelineEvent,
};
use serde::Serialize;
use serde_json::{Value, json};
use url::form_urlencoded::byte_serialize;

const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventTransition {
    Created,
    Updated,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct NormalizedEvent {
    pub schema_version: u8,
    pub instance_id: String,
    pub event_id: String,
    pub revision: u64,
    pub transition: EventTransition,
    pub source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_kind: Option<String>,
    pub origin: String,
    pub event_type: String,
    pub timestamp_ms: i64,
    pub start_timestamp_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_timestamp_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounding_box: Option<BoundingBox>,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<NormalizedAttachment>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct NormalizedAttachment {
    pub attachment_id: String,
    pub attachment_type: String,
    pub content_type: String,
    pub ordinal: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<i64>,
    pub status: AttachmentStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AttachmentStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Publication {
    pub dedup_key: String,
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: u8,
    pub retain: bool,
    pub event_timestamp_ms: i64,
    pub content_type: String,
    pub payload_format_indicator: Option<u8>,
    pub correlation_data: Vec<u8>,
}

impl Publication {
    pub(super) fn event(
        config: &MqttForwarderConfig,
        event: &NormalizedEvent,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            dedup_key: format!(
                "event:{}:{}:{}",
                event.instance_id, event.event_id, event.revision
            ),
            topic: event_topic(config, &event.source_id, &event.event_type),
            payload: serde_json::to_vec(event)?,
            qos: config.qos,
            retain: config.retain_events,
            event_timestamp_ms: event.timestamp_ms,
            content_type: "application/json".to_owned(),
            payload_format_indicator: Some(1),
            correlation_data: event.event_id.as_bytes().to_vec(),
        })
    }
}

pub(super) fn normalize_timeline_event(
    config: &MqttForwarderConfig,
    event: &TimelineEvent,
    transition: EventTransition,
    timestamp_ms: i64,
) -> NormalizedEvent {
    let canonical_attachment_id = event.canonical_attachment_id.as_deref();
    let mut payload = event.payload.clone().unwrap_or_default();
    payload.insert("icon_key".to_owned(), json!(event.icon_key));
    payload.insert(
        "rejected_icon_key".to_owned(),
        json!(event.rejected_icon_key),
    );
    payload.insert(
        "bounding_box_attachment_id".to_owned(),
        json!(event.bbox_attachment_id),
    );
    payload.insert(
        "canonical_attachment_id".to_owned(),
        json!(event.canonical_attachment_id),
    );
    NormalizedEvent {
        schema_version: SCHEMA_VERSION,
        instance_id: config.instance_id.clone(),
        event_id: event.id.clone(),
        revision: event.revision,
        transition,
        source_id: event.camera_id.clone(),
        media_kind: event.stream.clone(),
        origin: event.source.as_str().to_owned(),
        event_type: event.kind.clone(),
        timestamp_ms,
        start_timestamp_ms: event.start_time_ms,
        end_timestamp_ms: event.end_time_ms,
        confidence: event.confidence,
        zone: event.zone.clone(),
        text: event.text.clone(),
        bounding_box: event.bbox.map(|[x, y, width, height]| BoundingBox {
            x,
            y,
            width,
            height,
        }),
        payload: Value::Object(payload),
        attachments: event
            .attachments
            .iter()
            .map(|attachment| NormalizedAttachment {
                attachment_id: attachment.id.clone(),
                attachment_type: attachment.attachment_type.clone(),
                content_type: attachment.content_type.clone(),
                ordinal: attachment.ordinal,
                byte_len: attachment.byte_len,
                timestamp_ms: attachment.timestamp_ms,
                status: if Some(attachment.id.as_str()) == canonical_attachment_id
                    && event.thumbnail_filename.is_some()
                {
                    AttachmentStatus::Available
                } else {
                    AttachmentStatus::Unavailable
                },
            })
            .collect(),
    }
}

pub(super) fn normalize_operational_event(
    config: &MqttForwarderConfig,
    transition: &OperationalTransition,
) -> NormalizedEvent {
    let event = &transition.event;
    NormalizedEvent {
        schema_version: SCHEMA_VERSION,
        instance_id: config.instance_id.clone(),
        event_id: event.id.clone(),
        revision: event.revision,
        transition: match transition.kind {
            OperationalTransitionKind::Started => EventTransition::Created,
            OperationalTransitionKind::Updated | OperationalTransitionKind::Flap => {
                EventTransition::Updated
            }
            OperationalTransitionKind::Recovered => EventTransition::Ended,
        },
        source_id: event.key.camera_id.clone(),
        media_kind: event.key.stream_id.clone(),
        origin: "keeppeek".to_owned(),
        event_type: event.key.kind.as_str().to_owned(),
        timestamp_ms: transition.occurred_at_ms,
        start_timestamp_ms: event.start_time_ms,
        end_timestamp_ms: event.end_time_ms,
        confidence: None,
        zone: None,
        text: None,
        bounding_box: None,
        payload: json!({
            "severity": event.severity.as_str(),
            "cause": event.evidence.cause,
            "explanation": event.evidence.explanation,
            "affected_streams": event.evidence.affected_streams,
            "recording_interrupted": event.evidence.recording_interrupted,
            "evidence_source": event.evidence.source,
            "duration_ms": event.duration_ms,
        }),
        attachments: Vec::new(),
    }
}

pub(super) fn event_topic(
    config: &MqttForwarderConfig,
    source_id: &str,
    event_type: &str,
) -> String {
    format!(
        "{}/{}/sources/{}/events/{}",
        config.topic_prefix,
        encode_topic_segment(&config.instance_id),
        encode_topic_segment(source_id),
        encode_topic_segment(event_type)
    )
}

pub(super) fn status_topic(config: &MqttForwarderConfig) -> String {
    format!(
        "{}/{}/forwarders/{}/status",
        config.topic_prefix,
        encode_topic_segment(&config.instance_id),
        encode_topic_segment(&config.forwarder_id)
    )
}

fn encode_topic_segment(value: &str) -> String {
    byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::metadata::{EventAttachment, EventSource};

    fn timeline_event() -> TimelineEvent {
        TimelineEvent {
            id: "motion-42".to_owned(),
            revision: 3,
            camera_id: "front-door".to_owned(),
            stream: Some("sub".to_owned()),
            source: EventSource::Camera,
            kind: "motion".to_owned(),
            start_time_ms: 1_786_800_000_000,
            end_time_ms: Some(1_786_800_006_500),
            confidence: Some(0.94),
            bbox: Some([0.31, 0.16, 0.22, 0.71]),
            bbox_attachment_id: Some("snapshot-0".to_owned()),
            zone: Some("porch".to_owned()),
            text: Some("Person waiting at the porch".to_owned()),
            payload: Some(serde_json::Map::from_iter([
                (
                    "object_class".to_owned(),
                    Value::String("person".to_owned()),
                ),
                (
                    "icon_key".to_owned(),
                    Value::String("producer-value".to_owned()),
                ),
            ])),
            attachments: vec![EventAttachment {
                id: "snapshot-0".to_owned(),
                attachment_type: "thumbnail".to_owned(),
                content_type: "image/jpeg".to_owned(),
                byte_len: Some(1_024),
                ordinal: 0,
                timestamp_ms: Some(1_786_800_000_000),
                text: None,
            }],
            canonical_attachment_id: Some("snapshot-0".to_owned()),
            icon_key: "motion".to_owned(),
            rejected_icon_key: None,
            thumbnail_filename: Some("motion-42.jpg".to_owned()),
        }
    }

    #[test]
    fn normalizes_stable_event_identity_and_revision() {
        let config = MqttForwarderConfig::default();
        let event = normalize_timeline_event(
            &config,
            &timeline_event(),
            EventTransition::Ended,
            1_786_800_006_500,
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["instance_id"], "home-nvr");
        assert_eq!(value["event_id"], "motion-42");
        assert_eq!(value["revision"], 3);
        assert_eq!(value["transition"], "ended");
        assert_eq!(value["source_id"], "front-door");
        assert_eq!(value["timestamp_ms"], 1_786_800_006_500_i64);
        assert_eq!(value["text"], "Person waiting at the porch");
        assert_eq!(value["payload"]["object_class"], "person");
        assert_eq!(value["payload"]["icon_key"], "motion");
        assert_eq!(value["attachments"][0]["status"], "available");
    }

    #[test]
    fn publication_key_preserves_revision_for_redelivery() {
        let config = MqttForwarderConfig::default();
        let event = normalize_timeline_event(
            &config,
            &timeline_event(),
            EventTransition::Updated,
            1_786_800_001_000,
        );
        let publication = Publication::event(&config, &event).unwrap();
        assert_eq!(publication.dedup_key, "event:home-nvr:motion-42:3");
        assert_eq!(publication.content_type, "application/json");
        assert_eq!(publication.payload_format_indicator, Some(1));
        assert_eq!(publication.correlation_data, b"motion-42");
        assert_eq!(
            publication.topic,
            "keeppeek/home-nvr/sources/front-door/events/motion"
        );
    }

    #[test]
    fn topic_segments_cannot_inject_hierarchy_or_wildcards() {
        let config = MqttForwarderConfig::default();
        assert_eq!(
            event_topic(&config, "front/door+#", "motion/person"),
            "keeppeek/home-nvr/sources/front%2Fdoor%2B%23/events/motion%2Fperson"
        );
    }
}
