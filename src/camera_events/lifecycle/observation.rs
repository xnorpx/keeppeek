use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, SecondsFormat, Utc};
use onvif::event::{
    BoundingBox, Detection, Kind, Notification, PropertyOperation, TimestampReason, TimestampSource,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Event payloads stay below the native-event metadata budget.
const PAYLOAD_BYTES_MAX: usize = 16 * 1024;
/// Camera clocks outside this receipt window cannot establish physical event times.
const CLOCK_SKEW_MS_MAX: u64 = 300_000;

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) enum Identity {
    Property(Arc<str>),
    Object {
        module: Option<Arc<str>>,
        id: Arc<str>,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Action {
    Initialized(bool),
    Active,
    End,
    Point,
}

#[derive(Clone, PartialEq)]
pub(super) struct Details {
    pub confidence: Option<f64>,
    pub bbox: Option<[f32; 4]>,
    pub text: Option<String>,
}

pub(super) struct Clock {
    /// Orders observations by camera time, or by receipt time when camera time is unavailable.
    pub camera: DateTime<Utc>,
    pub received: Instant,
    pub time_ms: i64,
    pub trusted: bool,
    timestamp_source: TimestampSource,
    reason: Option<&'static str>,
}

pub(super) struct Observation {
    pub key: Identity,
    pub kind: Option<Kind>,
    pub action: Action,
    pub clock: Clock,
    pub details: Details,
    pub payload: Map<String, Value>,
}

pub(super) const fn known_operation(notification: &Notification) -> bool {
    matches!(
        notification.property_operation.as_ref(),
        None | Some(
            PropertyOperation::Initialized
                | PropertyOperation::Changed
                | PropertyOperation::Deleted
        )
    )
}

pub(super) fn notification(
    notification: &Notification,
    detection: Detection,
    received: Instant,
    received_ms: i64,
) -> anyhow::Result<Option<Observation>> {
    let action = match detection.operation {
        Some(PropertyOperation::Initialized) => {
            Action::Initialized(detection.active.unwrap_or(false))
        }
        Some(PropertyOperation::Deleted) => Action::End,
        Some(PropertyOperation::Changed) => match detection.active {
            Some(true) => Action::Active,
            Some(false) => Action::End,
            None => Action::Point,
        },
        None if detection.active.is_none() => Action::Point,
        _ => return Ok(None),
    };
    let clock = Clock::notification(notification, received, received_ms);
    let details = Details {
        confidence: detection.confidence.map(f64::from),
        bbox: detection.bbox.map(box_coordinates),
        text: detection.text,
    };
    let mut payload = clock.payload();
    payload.extend([
        ("origin".to_owned(), json!("notification")),
        ("topic".to_owned(), json!(topic(notification))),
        ("source_token".to_owned(), json!(detection.source)),
        ("rule".to_owned(), json!(detection.rule)),
        ("identity".to_owned(), json!(detection.identity)),
        ("object_box".to_owned(), json!(details.bbox)),
        ("count".to_owned(), json!(detection.count)),
    ]);
    validate_payload(&payload)?;
    Ok(Some(Observation {
        key: Identity::Property(Arc::from(detection.identity)),
        kind: Some(detection.kind),
        action,
        clock,
        details,
        payload,
    }))
}

fn topic(notification: &Notification) -> String {
    let root = &notification.topic.path[0];
    let mut topic = format!(
        "{{{}}}{}",
        root.namespace_uri.as_deref().unwrap_or_default(),
        root.local_name
    );
    for segment in &notification.topic.path[1..] {
        topic.push('/');
        topic.push_str(&segment.local_name);
    }
    topic
}

pub(super) fn validate_payload(payload: &Map<String, Value>) -> anyhow::Result<()> {
    anyhow::ensure!(
        serde_json::to_vec(payload)?.len() <= PAYLOAD_BYTES_MAX,
        "camera event payload exceeds limit"
    );
    Ok(())
}

pub(super) const fn box_coordinates(bbox: BoundingBox) -> [f32; 4] {
    [bbox.x, bbox.y, bbox.width, bbox.height]
}

impl Details {
    pub(super) fn merge(&mut self, update: &Self) {
        if let Some(confidence) = update.confidence {
            self.confidence = Some(confidence);
        }
        if let Some(bbox) = update.bbox {
            self.bbox = Some(bbox);
        }
        if update.text.is_some() {
            self.text.clone_from(&update.text);
        }
    }
}

impl Clock {
    pub(super) const fn has_camera_time(&self) -> bool {
        matches!(self.timestamp_source, TimestampSource::Camera)
    }

    pub(super) const fn camera_time_in_window(&self) -> bool {
        self.has_camera_time()
            && self.camera.timestamp_millis().abs_diff(self.time_ms) <= CLOCK_SKEW_MS_MAX
    }

    pub(super) const fn notification(
        notification: &Notification,
        received: Instant,
        received_ms: i64,
    ) -> Self {
        let reason = match notification.timestamp_source {
            TimestampSource::Camera => {
                return Self::new(notification.utc_time, received, received_ms);
            }
            TimestampSource::Received {
                reason: TimestampReason::Missing,
            } => "camera_time_missing",
            _ => "camera_time_invalid",
        };
        Self {
            camera: notification.utc_time,
            received,
            time_ms: received_ms,
            trusted: false,
            timestamp_source: notification.timestamp_source,
            reason: Some(reason),
        }
    }

    pub(super) const fn new(camera: DateTime<Utc>, received: Instant, received_ms: i64) -> Self {
        let camera_ms = camera.timestamp_millis();
        let reason = if camera_ms.abs_diff(received_ms) > CLOCK_SKEW_MS_MAX {
            Some("camera_time_out_of_window")
        } else if camera_ms > received_ms {
            Some("camera_time_in_future")
        } else {
            None
        };
        Self {
            camera,
            received,
            time_ms: if reason.is_some() {
                received_ms
            } else {
                camera_ms
            },
            trusted: reason.is_none(),
            timestamp_source: TimestampSource::Camera,
            reason,
        }
    }

    pub(super) fn payload(&self) -> Map<String, Value> {
        let camera_time = matches!(self.timestamp_source, TimestampSource::Camera)
            .then(|| self.camera.to_rfc3339_opts(SecondsFormat::AutoSi, true));
        Map::from_iter([
            ("protocol".to_owned(), json!("onvif")),
            ("observation_time_ms".to_owned(), json!(self.time_ms)),
            ("cameraTime".to_owned(), json!(camera_time)),
            (
                "timestamp_source".to_owned(),
                json!(if self.trusted { "camera" } else { "received" }),
            ),
            ("timestamp_reason".to_owned(), json!(self.reason)),
        ])
    }
}

impl Observation {
    pub(super) const fn state(&self) -> bool {
        matches!(self.action, Action::Active | Action::Initialized(true))
    }

    pub(super) fn fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update([u8::from(self.clock.has_camera_time())]);
        match &self.key {
            Identity::Property(identity) => {
                digest.update([0]);
                hash_text(&mut digest, identity);
            }
            Identity::Object { module, id } => {
                digest.update([1, u8::from(module.is_some())]);
                hash_text(&mut digest, module.as_deref().unwrap_or_default());
                hash_text(&mut digest, id);
            }
        }
        digest.update(self.clock.camera.timestamp().to_le_bytes());
        digest.update(self.clock.camera.timestamp_subsec_nanos().to_le_bytes());
        digest.update([match self.action {
            Action::Initialized(false) => 0,
            Action::Initialized(true) => 1,
            Action::Active => 2,
            Action::End => 3,
            Action::Point => 4,
        }]);
        digest.finalize().into()
    }
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use onvif::event::{Metadata, SimpleItem, parse_notifications};

    use super::super::Tracker;
    use super::super::tests::{BASE_MS, message, notification, notification_xml, started};
    use crate::cameras::events::EventConfig;
    use crate::keeppeek::KeepPeekEvent;
    use crate::storage::metadata::EventSource;

    const ACTIVE: &str = r#"<tt:SimpleItem Name="State" Value="true"/>"#;
    const PERSON: &str =
        r#"<tt:SimpleItem Name="State" Value="true"/><tt:SimpleItem Name="Class" Value="Human"/>"#;

    #[test]
    fn native_ids_are_random_and_opaque_values_stay_out_of_debug() {
        let mut input = message("VideoSource/MotionAlarm", Some("Changed"), PERSON, 0);
        input.source.simple[0].value = "../../source-secret".to_owned();
        input.source.simple[1].value = "../rule-secret".to_owned();
        input.key.simple[0].value = "../key-secret".to_owned();
        input
            .data
            .simple
            .push(SimpleItem::new("Text", "private-recognition"));
        input
            .data
            .simple
            .push(SimpleItem::new("Confidence", "0.75"));
        let mut tracker = Tracker::new("private-owner".to_owned(), EventConfig::default(), true);
        let changes = tracker.apply(&input, Instant::now(), BASE_MS).unwrap();
        let event = started(&changes);
        assert_eq!(
            uuid::Uuid::parse_str(&event.id).unwrap().get_version_num(),
            4
        );
        assert_eq!(event.source, EventSource::Camera);
        assert_eq!(event.camera_id, "private-owner");
        assert_eq!(event.kind, "person");
        assert_eq!(event.icon_key, "person");
        assert_eq!(event.confidence, Some(0.75));
        assert_eq!(event.text.as_deref(), Some("private-recognition"));
        assert!(event.bbox.is_none() && event.bbox_attachment_id.is_none());
        assert!(event.attachments.is_empty());
        let payload = event.payload.as_ref().unwrap();
        assert_eq!(payload["source_token"], "../../source-secret");
        assert_eq!(payload["rule"], "../rule-secret");
        assert_eq!(
            payload["topic"],
            "{http://www.onvif.org/ver10/topics}VideoSource/MotionAlarm"
        );
        assert!(
            payload["identity"]
                .as_str()
                .unwrap()
                .contains("../key-secret")
        );
        let diagnostic = format!("{tracker:?}");
        for value in [
            "source-secret",
            "rule-secret",
            "key-secret",
            "private-recognition",
            "private-owner",
            "<tt:",
        ] {
            assert!(!diagnostic.contains(value));
            assert!(!event.id.contains(value));
        }
    }

    #[test]
    fn explicit_source_filters_are_exact_and_never_use_key_or_data_tokens() {
        let policy = EventConfig {
            source_tokens: vec!["source-token".to_owned()],
            ..EventConfig::default()
        };
        let mut accepted = Tracker::new("camera".to_owned(), policy.clone(), true);
        assert_eq!(
            accepted
                .apply(&notification("Changed", true, 0), Instant::now(), BASE_MS)
                .unwrap()
                .len(),
            1
        );
        for source in ["source-token-extra", "SOURCE-TOKEN", ""] {
            let mut input = notification("Changed", true, 0);
            input.source.simple[0].value = source.to_owned();
            input
                .key
                .simple
                .push(SimpleItem::new("VideoSourceToken", "source-token"));
            input
                .data
                .simple
                .push(SimpleItem::new("VideoSourceToken", "source-token"));
            let mut tracker = Tracker::new("camera".to_owned(), policy.clone(), true);
            assert!(
                tracker
                    .apply(&input, Instant::now(), BASE_MS)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(tracker.active_count(), 0);
        }
    }

    #[test]
    fn topic_filters_are_exact_namespace_expanded_paths_with_exclusion_precedence() {
        let root = "{http://www.onvif.org/ver10/topics}VideoSource";
        let exact = format!("{root}/MotionAlarm");
        for (include, exclude, expected) in [
            (vec![root.to_owned()], vec![], 0),
            (vec![exact.clone()], vec![], 1),
            (vec![exact.clone()], vec![exact], 0),
            (vec![], vec![root.to_owned()], 1),
        ] {
            let policy = EventConfig {
                include_topics: include,
                exclude_topics: exclude,
                ..EventConfig::default()
            };
            let mut tracker = Tracker::new("camera".to_owned(), policy, true);
            assert_eq!(
                tracker
                    .apply(&notification("Changed", true, 0), Instant::now(), BASE_MS)
                    .unwrap()
                    .len(),
                expected
            );
        }
    }

    #[test]
    fn unknown_topics_and_namespaces_do_not_imply_motion() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        for topic in [
            "VideoSource/MotionAlarm/Unknown",
            "Unknown/MotionAlarm",
            "RuleEngine/Unknown/Motion",
        ] {
            assert!(
                tracker
                    .apply(
                        &message(topic, Some("Changed"), ACTIVE, 0),
                        received,
                        BASE_MS
                    )
                    .unwrap()
                    .is_empty()
            );
        }
        let xml = notification_xml("VideoSource/MotionAlarm", Some("Changed"), ACTIVE, 0).replace(
            "http://www.onvif.org/ver10/topics",
            "urn:unrecognized:topics",
        );
        let unknown = parse_notifications(xml.as_bytes()).unwrap().remove(0);
        assert!(
            tracker
                .apply(&unknown, received, BASE_MS)
                .unwrap()
                .is_empty()
        );
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn motion_opt_out_keeps_specific_types_and_classless_endings() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), false);
        assert!(
            tracker
                .apply(&notification("Changed", true, 0), received, BASE_MS)
                .unwrap()
                .is_empty()
        );
        let person = tracker
            .apply(
                &message("VideoSource/MotionAlarm", Some("Changed"), PERSON, 100),
                received,
                BASE_MS + 100,
            )
            .unwrap();
        assert_eq!(started(&person).kind, "person");
        assert!(
            tracker
                .apply(&notification("Changed", true, 200), received, BASE_MS + 200)
                .unwrap()
                .is_empty()
        );
        assert_eq!(tracker.kinds(), vec!["person"]);
        let ended = tracker
            .apply(
                &notification("Changed", false, 300),
                received,
                BASE_MS + 300,
            )
            .unwrap();
        assert!(
            matches!(ended.as_slice(), [KeepPeekEvent::TimelineEventEnded { id, .. }] if id == &started(&person).id)
        );
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn last_kind_is_kept_independently_for_source_and_key_identities() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), false);
        let person = message("VideoSource/MotionAlarm", Some("Changed"), PERSON, 0);
        let mut vehicle = message(
            "VideoSource/MotionAlarm",
            Some("Changed"),
            &PERSON.replace("Human", "Vehicle"),
            0,
        );
        vehicle.key.simple[0].value = "second-key".to_owned();
        let mut other_source = person.clone();
        other_source.source.simple[0].value = "another-source".to_owned();
        let first = tracker.apply(&person, received, BASE_MS).unwrap();
        tracker.apply(&vehicle, received, BASE_MS).unwrap();
        tracker.apply(&other_source, received, BASE_MS).unwrap();
        assert_eq!(tracker.active_count(), 3);
        let stopped = tracker
            .apply(
                &notification("Changed", false, 100),
                received,
                BASE_MS + 100,
            )
            .unwrap();
        assert!(
            matches!(stopped.as_slice(), [KeepPeekEvent::TimelineEventEnded { id, .. }] if id == &started(&first).id)
        );
        let mut deleted = message("VideoSource/MotionAlarm", Some("Deleted"), "", 200);
        deleted.key.simple[0].value = "second-key".to_owned();
        assert_eq!(
            tracker
                .apply(&deleted, received, BASE_MS + 200)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(tracker.active_count(), 1);
        assert_eq!(tracker.kinds(), vec!["person"]);
    }

    #[test]
    fn motion_opt_out_still_releases_a_classified_initialized_baseline() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), false);
        let initialized = message("VideoSource/MotionAlarm", Some("Initialized"), PERSON, 0);
        assert!(
            tracker
                .apply(&initialized, received, BASE_MS)
                .unwrap()
                .is_empty()
        );
        assert!(
            tracker
                .apply(
                    &notification("Changed", false, 1_000),
                    received + Duration::from_secs(1),
                    BASE_MS + 1_000
                )
                .unwrap()
                .is_empty()
        );
        let changes = tracker
            .apply(
                &message("VideoSource/MotionAlarm", Some("Changed"), PERSON, 2_000),
                received + Duration::from_secs(2),
                BASE_MS + 2_000,
            )
            .unwrap();
        assert_eq!(started(&changes).kind, "person");
    }

    fn assert_point(topic: &str, data: &str, expected_kind: &str) {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        let point = message(topic, None, data, 0);
        let first = tracker.apply(&point, received, BASE_MS).unwrap();
        let event = started(&first);
        assert_eq!(event.kind, expected_kind);
        assert_eq!(event.end_time_ms, Some(event.start_time_ms));
        assert_eq!(tracker.active_count(), 0);
        assert!(
            tracker
                .apply(&point, received + Duration::from_secs(1), BASE_MS + 1_000)
                .unwrap()
                .is_empty()
        );
        let later = tracker
            .apply(
                &message(topic, None, data, 1_000),
                received + Duration::from_secs(1),
                BASE_MS + 1_000,
            )
            .unwrap();
        assert_ne!(event.id, started(&later).id);
        assert_eq!(tracker.deduplicated(), 1);
    }

    #[test]
    fn point_kinds_are_finite_and_deduplicate_timestamp_plus_identity() {
        let count = r#"<tt:SimpleItem Name="Count" Value="0"/>"#;
        let class = r#"<tt:SimpleItem Name="Class" Value="Human"/>"#;
        for (topic, data, kind) in [
            ("RuleEngine/LineDetector/Crossed", "", "line_crossing"),
            (
                "RuleEngine/FieldDetector/RegionEntrance",
                "",
                "region_entry",
            ),
            ("RuleEngine/FieldDetector/RegionExit", "", "region_exit"),
            ("RuleEngine/Recognition/Face", "", "face"),
            ("RuleEngine/Recognition/LicensePlate", "", "license_plate"),
            ("RuleEngine/MyRuleDetector/Visitor", "", "doorbell_press"),
            ("RuleEngine/ObjectDetection/Object", class, "person"),
            ("RuleEngine/ObjectDetector/Count", count, "object_count"),
            ("RuleEngine/CountAggregation/Counter", count, "object_count"),
            (
                "RuleEngine/CountAggregation/OccupancyCounter",
                count,
                "object_count",
            ),
        ] {
            assert_point(topic, data, kind);
        }
    }

    #[test]
    fn ignored_operations_do_not_advance_lifecycle_or_watermarks() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .apply(&notification("Changed", true, 0), received, BASE_MS)
            .unwrap();
        for operation in [None, Some("Unsupported-private-operation")] {
            assert!(
                tracker
                    .apply(
                        &message("VideoSource/MotionAlarm", operation, ACTIVE, 200_000),
                        received,
                        BASE_MS
                    )
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(tracker.active_count(), 1);
        assert_eq!(
            tracker
                .apply(
                    &notification("Changed", false, 100),
                    received,
                    BASE_MS + 100
                )
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn initialized_baselines_and_real_reconnect_activity_remain_distinct() {
        let received = Instant::now();
        let mut baseline = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        baseline
            .apply(&notification("Initialized", true, 0), received, BASE_MS)
            .unwrap();
        assert!(baseline.disconnect("reconnect").is_empty());
        assert!(
            baseline
                .apply(
                    &notification("Changed", true, 1_000),
                    received,
                    BASE_MS + 1_000
                )
                .unwrap()
                .is_empty()
        );
        baseline
            .apply(
                &notification("Initialized", false, 2_000),
                received,
                BASE_MS + 2_000,
            )
            .unwrap();
        let opening = baseline
            .apply(
                &notification("Changed", true, 3_000),
                received,
                BASE_MS + 3_000,
            )
            .unwrap();
        baseline.disconnect("reconnect");
        assert!(
            baseline
                .apply(
                    &notification("Initialized", true, 4_000),
                    received,
                    BASE_MS + 4_000
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            baseline
                .apply(
                    &notification("Changed", true, 3_000),
                    received,
                    BASE_MS + 4_000
                )
                .unwrap()
                .is_empty()
        );
        let restarted = baseline
            .apply(
                &notification("Changed", true, 5_000),
                received,
                BASE_MS + 5_000,
            )
            .unwrap();
        assert_ne!(started(&opening).id, started(&restarted).id);
    }

    #[test]
    fn unsupported_operations_do_not_interpret_their_data() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        let input = message(
            "VideoSource/MotionAlarm",
            Some("Unsupported-operation"),
            r#"<tt:SimpleItem Name="State" Value="true"/><tt:SimpleItem Name="Confidence" Value="NaN"/>"#,
            0,
        );
        assert!(tracker.apply(&input, received, BASE_MS).unwrap().is_empty());
        assert_eq!(tracker.active_count(), 0);
        assert_eq!(tracker.deduplicated(), 0);
    }

    #[test]
    fn pullpoint_and_event_stream_share_canonical_notification_keys() {
        let xml = notification_xml("VideoSource/MotionAlarm", Some("Changed"), PERSON, 0);
        let mut pulled = parse_notifications(xml.as_bytes()).unwrap().remove(0);
        pulled.source.simple.reverse();
        let wrapped = format!(
            r#"<tt:MetadataStream xmlns:tt="http://www.onvif.org/ver10/schema"><tt:Event>{xml}</tt:Event></tt:MetadataStream>"#
        );
        let metadata = Metadata::parse(wrapped.as_bytes()).unwrap();
        assert_eq!(metadata.invalid_messages, 0);
        assert_eq!(metadata.notifications.len(), 1);
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker.apply(&pulled, received, BASE_MS).unwrap();
        assert!(
            tracker
                .apply(
                    &metadata.notifications[0],
                    received + Duration::from_millis(100),
                    BASE_MS + 100
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(tracker.active_count(), 1);
        assert_eq!(tracker.deduplicated(), 1);
    }

    #[test]
    fn timestamp_window_never_claims_future_activity_and_retains_camera_evidence() {
        for (offset, expected, reason) in [
            (-300_000, BASE_MS - 300_000, None),
            (-300_001, BASE_MS, Some("camera_time_out_of_window")),
            (0, BASE_MS, None),
            (1, BASE_MS, Some("camera_time_in_future")),
            (300_000, BASE_MS, Some("camera_time_in_future")),
            (300_001, BASE_MS, Some("camera_time_out_of_window")),
        ] {
            let input = notification("Changed", true, offset);
            let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
            let changes = tracker.apply(&input, Instant::now(), BASE_MS).unwrap();
            let event = started(&changes);
            assert_eq!(event.start_time_ms, expected);
            assert!(event.start_time_ms <= BASE_MS);
            let payload = event.payload.as_ref().unwrap();
            assert_eq!(payload["timestamp_reason"], serde_json::json!(reason));
            let original =
                chrono::DateTime::parse_from_rfc3339(payload["cameraTime"].as_str().unwrap())
                    .unwrap();
            assert_eq!(original, input.utc_time);
        }
    }

    fn timestamp_xml(attributes: &str, data: &str) -> String {
        format!(
            r#"<n:NotificationMessage xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:tt="http://www.onvif.org/ver10/schema"
                xmlns:q="http://www.onvif.org/ver10/topics">
                <n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">q:VideoSource/MotionAlarm</n:Topic>
                <n:Message><tt:Message {attributes} PropertyOperation="Changed">
                    <tt:Source><tt:SimpleItem Name="VideoSourceConfigurationToken" Value="source-token"/></tt:Source>
                    <tt:Data>{data}</tt:Data>
                </tt:Message></n:Message>
            </n:NotificationMessage>"#
        )
    }

    #[test]
    fn notifications_without_camera_time_use_receipt_without_camera_evidence() {
        let received_time = chrono::DateTime::from_timestamp_millis(BASE_MS).unwrap();
        for (attributes, reason) in [
            ("", "camera_time_missing"),
            (r#"UtcTime="private-invalid-clock""#, "camera_time_invalid"),
        ] {
            let xml = timestamp_xml(attributes, PERSON);
            let notifications =
                onvif::event::parse_notifications_at(xml.as_bytes(), received_time).unwrap();
            let received = Instant::now();
            let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
            let changes = tracker.apply(&notifications[0], received, BASE_MS).unwrap();
            let event = started(&changes);
            assert_eq!(event.kind, "person");
            assert_eq!(event.start_time_ms, BASE_MS);
            let payload = event.payload.as_ref().unwrap();
            assert_eq!(payload["observation_time_ms"], BASE_MS);
            assert_eq!(payload["timestamp_source"], "received");
            assert_eq!(payload["timestamp_reason"], reason);
            assert!(payload["cameraTime"].is_null());
            assert!(
                !serde_json::to_string(payload)
                    .unwrap()
                    .contains("private-invalid-clock")
            );
            let stopped = timestamp_xml(attributes, &ACTIVE.replace("true", "false"));
            let stopped = onvif::event::parse_notifications_at(
                stopped.as_bytes(),
                received_time + chrono::Duration::seconds(1),
            )
            .unwrap();
            let ended = tracker
                .apply(
                    &stopped[0],
                    received + Duration::from_secs(1),
                    BASE_MS + 1_000,
                )
                .unwrap();
            assert!(matches!(
                ended.as_slice(),
                [KeepPeekEvent::TimelineEventEnded { end_time_ms, .. }]
                    if *end_time_ms == BASE_MS + 1_000
            ));
            assert_eq!(tracker.active_count(), 0);
        }
    }

    #[test]
    fn closing_clamps_a_backward_received_clock_to_the_event_start() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .apply(&notification("Changed", true, 0), received, BASE_MS)
            .unwrap();
        let ended = tracker
            .apply(
                &notification("Changed", false, 1_000),
                received + Duration::from_secs(1),
                BASE_MS - 1_000,
            )
            .unwrap();
        assert!(
            matches!(ended.as_slice(), [KeepPeekEvent::TimelineEventEnded { end_time_ms, .. }] if *end_time_ms == BASE_MS)
        );
    }

    #[test]
    fn oversized_serialized_payload_rejects_without_truncation_or_state_changes() {
        let mut input = notification("Changed", true, 0);
        input.source.simple[0].value = "\u{1}".repeat(2_000);
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        let error = tracker
            .apply(&input, Instant::now(), BASE_MS)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "camera event payload exceeds limit");
        assert_eq!(tracker.active_count(), 0);
        assert_eq!(tracker.deduplicated(), 0);
        input.source.simple[0].value = "source-token".to_owned();
        assert_eq!(
            tracker
                .apply(&input, Instant::now(), BASE_MS)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn baseline_capacity_does_not_forget_existing_initialized_properties() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        for index in 0..128 {
            let mut input = notification("Initialized", true, 0);
            input.key.simple[0].value = index.to_string();
            assert!(tracker.apply(&input, received, BASE_MS).unwrap().is_empty());
        }
        let mut excess = notification("Initialized", true, 0);
        excess.key.simple[0].value = "excess".to_owned();
        assert!(tracker.apply(&excess, received, BASE_MS).is_err());
        assert_eq!(tracker.baselines.len(), 128);
        let mut existing = notification("Changed", true, 100);
        existing.key.simple[0].value = "0".to_owned();
        assert!(
            tracker
                .apply(&existing, received, BASE_MS + 100)
                .unwrap()
                .is_empty()
        );
        existing = notification("Changed", false, 200);
        existing.key.simple[0].value = "0".to_owned();
        tracker.apply(&existing, received, BASE_MS + 200).unwrap();
        assert!(
            tracker
                .apply(&excess, received, BASE_MS + 200)
                .unwrap()
                .is_empty()
        );
        assert_eq!(tracker.baselines.len(), 128);
    }
}
