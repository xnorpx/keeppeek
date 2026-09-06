use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use onvif::event::{Frame, Kind, Object};
use serde_json::json;

use super::observation::{Action, Clock, Details, Identity, Observation, box_coordinates};
use super::{ACTIVE_MAX, Tracker};

/// These bounds match the ONVIF parser and also guard hand-built or modified frames.
const OBJECTS_MAX: usize = 128;
const STRING_BYTES_MAX: usize = 256;

pub(super) fn prepare(
    tracker: &Tracker,
    frame: &Frame,
    received: Instant,
    received_ms: i64,
) -> anyhow::Result<Vec<Observation>> {
    validate(frame)?;
    let mut observations = Vec::with_capacity(frame.objects.len() + frame.deleted_ids.len());
    for id in &frame.deleted_ids {
        let key = identity(frame, id);
        observations.push(Observation {
            kind: tracker.history.open_kind(&key),
            key,
            action: Action::End,
            clock: Clock::new(frame.utc_time, received, received_ms),
            details: Details {
                confidence: None,
                bbox: None,
                text: None,
            },
            payload: serde_json::Map::new(),
        });
    }
    for object in &frame.objects {
        let key = identity(frame, &object.id);
        let known = tracker
            .active
            .contains_key(&key)
            .then(|| tracker.history.open_kind(&key))
            .flatten();
        let Some(kind) = object.class.map(Kind::from).or(known) else {
            continue;
        };
        let clock = Clock::new(frame.utc_time, received, received_ms);
        let details = Details {
            confidence: object.class.and(object.confidence).map(f64::from),
            bbox: object.bbox.map(box_coordinates),
            text: object.text.clone(),
        };
        let mut payload = clock.payload();
        payload.extend([
            ("origin".to_owned(), json!("metadata")),
            ("topic".to_owned(), json!(null)),
            ("source_token".to_owned(), json!(null)),
            ("rule".to_owned(), json!(null)),
            ("analytics_module".to_owned(), json!(frame.source)),
            ("object_id".to_owned(), json!(object.id)),
            ("object_box".to_owned(), json!(details.bbox)),
        ]);
        observations.push(Observation {
            key,
            kind: Some(kind),
            action: Action::Active,
            clock,
            details,
            payload,
        });
    }
    Ok(observations)
}

fn identity(frame: &Frame, id: &str) -> Identity {
    Identity::Object {
        module: frame.source.as_deref().map(Arc::from),
        id: Arc::from(id),
    }
}

fn validate(frame: &Frame) -> anyhow::Result<()> {
    anyhow::ensure!(
        frame.objects.len() <= OBJECTS_MAX && frame.deleted_ids.len() <= OBJECTS_MAX,
        "camera metadata object count exceeds limit"
    );
    anyhow::ensure!(
        frame
            .source
            .as_ref()
            .is_none_or(|source| source.len() <= STRING_BYTES_MAX),
        "camera metadata module exceeds limit"
    );
    let mut identities = HashSet::with_capacity(frame.objects.len() + frame.deleted_ids.len());
    for id in &frame.deleted_ids {
        validate_id(id)?;
        anyhow::ensure!(
            identities.insert(id.as_str()),
            "camera metadata repeats an object identity"
        );
    }
    for object in &frame.objects {
        validate_id(&object.id)?;
        anyhow::ensure!(
            identities.insert(object.id.as_str()),
            "camera metadata repeats an object identity"
        );
        validate_object(object)?;
    }
    Ok(())
}

fn validate_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !id.is_empty() && id.len() <= STRING_BYTES_MAX,
        "camera metadata object identity exceeds limit"
    );
    Ok(())
}

fn validate_object(object: &Object) -> anyhow::Result<()> {
    anyhow::ensure!(
        object
            .confidence
            .is_none_or(|value| value.is_finite() && (0.0..=1.0).contains(&value)),
        "camera metadata confidence is invalid"
    );
    anyhow::ensure!(
        object
            .text
            .as_ref()
            .is_none_or(|text| text.len() <= STRING_BYTES_MAX),
        "camera metadata text exceeds limit"
    );
    if let Some(bbox) = object.bbox {
        anyhow::ensure!(
            box_coordinates(bbox)
                .iter()
                .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
                && bbox.width > 0.0
                && bbox.height > 0.0
                && bbox.x + bbox.width <= 1.0
                && bbox.y + bbox.height <= 1.0,
            "camera metadata box is invalid"
        );
    }
    Ok(())
}

pub(super) fn check_capacity(
    tracker: &Tracker,
    observations: &[Observation],
) -> anyhow::Result<()> {
    let mut slots = tracker.active.len() + tracker.pending.len();
    for observation in observations {
        match observation.action {
            Action::End => {
                if tracker
                    .active
                    .get(&observation.key)
                    .is_some_and(|active| !active.deferred(observation.clock.received))
                {
                    slots -= 1;
                }
            }
            Action::Active if tracker.needs_slot(observation) => slots += 1,
            _ => {}
        }
    }
    anyhow::ensure!(slots <= ACTIVE_MAX, "camera event active capacity reached");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use onvif::event::Frame;

    use super::super::Tracker;
    use super::super::tests::{BASE_MS, frame, notification, object, started};
    use crate::cameras::events::{EventConfig, EventMode, MetadataMode};
    use crate::keeppeek::KeepPeekEvent;
    use crate::storage::metadata::TimelineEvent;

    fn detailed_frame(offset_ms: i64, confidence: &str, text: &str, left: &str) -> Frame {
        let body = format!(
            r#"<tt:Object ObjectId="7"><tt:Appearance>
          <tt:Class><tt:Type Likelihood="{confidence}">LicensePlate</tt:Type></tt:Class>
          <tt:Shape><tt:BoundingBox left="{left}" right="0.5" top="0.5" bottom="-0.5"/></tt:Shape>
          <tt:LicensePlateInfo><tt:PlateNumber>{text}</tt:PlateNumber></tt:LicensePlateInfo>
        </tt:Appearance></tt:Object>"#
        );
        frame(offset_ms, "module", &body)
    }

    fn full_frame(offset_ms: i64, confidence: &str) -> Frame {
        let body = (0..128)
            .map(|index| object(&index.to_string(), "Human", confidence))
            .collect::<String>();
        frame(offset_ms, "module", &body)
    }

    fn updated(changes: &[KeepPeekEvent]) -> &TimelineEvent {
        match changes {
            [KeepPeekEvent::TimelineEventImages { event, images }] => {
                assert!(images.is_empty());
                assert!(event.attachments.is_empty());
                assert!(event.bbox.is_none());
                assert!(event.bbox_attachment_id.is_none());
                event
            }
            _ => panic!("expected exactly one metadata-only revision"),
        }
    }

    #[test]
    fn updates_publish_only_latest_details_at_most_once_per_second() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        let first = tracker
            .frame(
                &detailed_frame(0, "0.25", "first", "-0.5"),
                received,
                BASE_MS,
            )
            .unwrap();
        for (offset, score, text, left) in
            [(100, "0.5", "second", "-0.25"), (900, "0.75", "last", "0")]
        {
            assert!(
                tracker
                    .frame(
                        &detailed_frame(offset, score, text, left),
                        received + Duration::from_millis(offset as u64),
                        BASE_MS + offset
                    )
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(
            tracker
                .expire(received + Duration::from_millis(999))
                .is_empty()
        );
        let changes = tracker.expire(received + Duration::from_secs(1));
        let update = updated(&changes);
        assert_eq!(update.id, started(&first).id);
        assert_eq!(update.revision, started(&first).revision);
        assert_eq!(update.confidence, Some(0.75));
        assert_eq!(update.text.as_deref(), Some("last"));
        assert_eq!(
            update.payload.as_ref().unwrap()["object_box"],
            serde_json::json!([0.5, 0.25, 0.25, 0.5])
        );
        assert!(
            tracker
                .frame(
                    &detailed_frame(2_000, "0.75", "last", "0"),
                    received + Duration::from_secs(2),
                    BASE_MS + 2_000
                )
                .unwrap()
                .is_empty()
        );
        assert!(tracker.expire(received + Duration::from_secs(3)).is_empty());
    }

    #[test]
    fn final_throttled_details_survive_deletion_and_flush_after_the_end() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .frame(
                &detailed_frame(0, "0.25", "first", "-0.5"),
                received,
                BASE_MS,
            )
            .unwrap();
        tracker
            .frame(
                &detailed_frame(100, "0.75", "final", "0"),
                received + Duration::from_millis(100),
                BASE_MS + 100,
            )
            .unwrap();
        let deletion = frame(
            200,
            "module",
            r#"<tt:ObjectTree><tt:Delete ObjectId="7"/></tt:ObjectTree>"#,
        );
        let ended = tracker
            .frame(
                &deletion,
                received + Duration::from_millis(200),
                BASE_MS + 200,
            )
            .unwrap();
        assert!(
            matches!(ended.as_slice(), [KeepPeekEvent::TimelineEventEnded { end_time_ms, .. }] if *end_time_ms == BASE_MS + 200)
        );
        assert_eq!(tracker.active_count(), 0);
        assert!(
            tracker
                .expire(received + Duration::from_millis(999))
                .is_empty()
        );
        let changes = tracker.expire(received + Duration::from_secs(1));
        let event = updated(&changes);
        assert_eq!(event.text.as_deref(), Some("final"));
        assert_eq!(event.confidence, Some(0.75));
        assert_eq!(event.end_time_ms, Some(BASE_MS + 200));
        assert!(tracker.expire(received + Duration::from_secs(2)).is_empty());
    }

    #[test]
    fn details_that_revert_before_the_deadline_do_not_create_a_revision() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        for (offset, confidence, text) in [
            (0, "0.25", "original"),
            (100, "0.75", "temporary"),
            (500, "0.25", "original"),
        ] {
            tracker
                .frame(
                    &detailed_frame(offset, confidence, text, "-0.5"),
                    received + Duration::from_millis(offset as u64),
                    BASE_MS + offset,
                )
                .unwrap();
        }
        assert!(tracker.expire(received + Duration::from_secs(1)).is_empty());
        assert_eq!(tracker.disconnect("finished").len(), 1);
        assert!(tracker.pending.is_empty());
    }

    #[test]
    fn partial_known_objects_keep_class_confidence_and_text_without_inference() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .frame(
                &detailed_frame(0, "0.25", "retained", "-0.5"),
                received,
                BASE_MS,
            )
            .unwrap();
        let body = r#"<tt:Object ObjectId="7"><tt:Appearance>
          <tt:Class><tt:Type Likelihood="0.99">Unknown</tt:Type></tt:Class>
          <tt:Shape><tt:BoundingBox left="0" right="0.5" top="0.5" bottom="-0.5"/></tt:Shape>
        </tt:Appearance></tt:Object>"#;
        let changes = tracker
            .frame(
                &frame(1_000, "module", body),
                received + Duration::from_secs(1),
                BASE_MS + 1_000,
            )
            .unwrap();
        let event = updated(&changes);
        assert_eq!(event.kind, "license_plate");
        assert_eq!(event.confidence, Some(0.25));
        assert_eq!(event.text.as_deref(), Some("retained"));
        assert_eq!(
            event.payload.as_ref().unwrap()["object_box"],
            serde_json::json!([0.5, 0.25, 0.25, 0.5])
        );
    }

    #[test]
    fn overflowing_frames_are_atomic_and_deletions_can_release_capacity() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        assert_eq!(
            tracker
                .frame(&full_frame(0, "0.25"), received, BASE_MS)
                .unwrap()
                .len(),
            128
        );
        let body = object("0", "Human", "0.75") + &object("excess", "Human", "0.25");
        assert!(
            tracker
                .frame(
                    &frame(100, "module", &body),
                    received + Duration::from_millis(100),
                    BASE_MS + 100
                )
                .is_err()
        );
        assert_eq!(tracker.active_count(), 128);
        assert!(tracker.expire(received + Duration::from_secs(1)).is_empty());
        assert!(
            tracker
                .apply(
                    &notification("Changed", true, 1_000),
                    received + Duration::from_secs(1),
                    BASE_MS + 1_000
                )
                .is_err()
        );
        let replacement = object("excess", "Human", "0.25")
            + r#"<tt:ObjectTree><tt:Delete ObjectId="0"/></tt:ObjectTree>"#;
        let changes = tracker
            .frame(
                &frame(1_000, "module", &replacement),
                received + Duration::from_secs(1),
                BASE_MS + 1_000,
            )
            .unwrap();
        assert!(matches!(
            changes.as_slice(),
            [
                KeepPeekEvent::TimelineEventEnded { .. },
                KeepPeekEvent::TimelineEventStarted { .. }
            ]
        ));
        assert_eq!(tracker.active_count(), 128);
    }

    #[test]
    fn final_updates_share_the_active_budget_and_recover_after_flush() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .frame(&full_frame(0, "0.25"), received, BASE_MS)
            .unwrap();
        assert!(
            tracker
                .frame(
                    &full_frame(100, "0.75"),
                    received + Duration::from_millis(100),
                    BASE_MS + 100
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(tracker.disconnect_metadata("loss").len(), 128);
        assert_eq!(tracker.active_count(), 0);
        assert_eq!(tracker.pending.len(), 128);
        let input = notification("Changed", true, 200);
        assert!(
            tracker
                .apply(&input, received + Duration::from_millis(200), BASE_MS + 200)
                .is_err()
        );
        let updates = tracker.expire(received + Duration::from_secs(1));
        assert_eq!(updates.len(), 128);
        assert!(updates.iter().all(|change| matches!(change, KeepPeekEvent::TimelineEventImages { images, .. } if images.is_empty())));
        assert!(tracker.pending.is_empty());
        assert_eq!(
            tracker
                .apply(&input, received + Duration::from_secs(1), BASE_MS + 1_000)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn invalid_late_objects_cannot_mutate_earlier_objects_in_the_same_frame() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .frame(
                &frame(0, "module", &object("first", "Human", "0.25")),
                received,
                BASE_MS,
            )
            .unwrap();
        for confidence in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            let body =
                object("first", "Human", "0.75") + &object("../../private-object", "Human", "0.25");
            let mut invalid = frame(100, "module", &body);
            invalid.objects[1].confidence = Some(confidence);
            let error = tracker
                .frame(
                    &invalid,
                    received + Duration::from_millis(100),
                    BASE_MS + 100,
                )
                .err()
                .unwrap();
            assert!(!format!("{error:?}").contains("private-object"));
        }
        assert!(tracker.expire(received + Duration::from_secs(1)).is_empty());
        assert_eq!(tracker.expire(received + Duration::from_secs(5)).len(), 1);
    }

    #[test]
    fn frame_bounds_duplicate_ids_and_conflicting_deletions_are_rejected() {
        let base = detailed_frame(0, "0.25", "text", "-0.5");
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        for case in 0..9 {
            let mut invalid = base.clone();
            match case {
                0 => invalid.objects = vec![invalid.objects[0].clone(); 129],
                1 => invalid.deleted_ids = vec!["deleted".to_owned(); 129],
                2 => invalid.source = Some("module".repeat(43)),
                3 => invalid.objects[0].id = "id".repeat(129),
                4 => invalid.objects[0].id.clear(),
                5 => invalid.objects[0].text = Some("x".repeat(257)),
                6 => invalid.objects.push(invalid.objects[0].clone()),
                7 => invalid.deleted_ids.push("7".to_owned()),
                8 => invalid.objects[0].bbox.as_mut().unwrap().width = 1.0,
                _ => unreachable!(),
            }
            assert!(tracker.frame(&invalid, received, BASE_MS).is_err());
            assert_eq!(tracker.active_count(), 0);
        }
    }

    #[test]
    fn deletion_of_an_unseen_object_fences_older_late_frames() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        let deleted = frame(
            1_000,
            "module",
            r#"<tt:ObjectTree><tt:Delete ObjectId="7"/></tt:ObjectTree>"#,
        );
        assert!(
            tracker
                .frame(&deleted, received, BASE_MS + 1_000)
                .unwrap()
                .is_empty()
        );
        assert!(
            tracker
                .frame(
                    &frame(500, "module", &object("7", "Human", "0.25")),
                    received,
                    BASE_MS + 1_000
                )
                .unwrap()
                .is_empty()
        );
        let changes = tracker
            .frame(
                &frame(1_500, "module", &object("7", "Human", "0.25")),
                received,
                BASE_MS + 1_500,
            )
            .unwrap();
        assert_eq!(started(&changes).kind, "person");
        assert_eq!(tracker.deduplicated(), 1);
    }

    #[test]
    fn metadata_reconnect_requires_fresh_time_and_explicit_class_to_reopen() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        let input = frame(0, "module", &object("7", "Human", "0.25"));
        let first = tracker.frame(&input, received, BASE_MS).unwrap();
        tracker.disconnect_metadata("loss");
        assert!(
            tracker
                .frame(&input, received + Duration::from_secs(1), BASE_MS + 1_000)
                .unwrap()
                .is_empty()
        );
        assert!(
            tracker
                .frame(
                    &frame(1_000, "module", &object("7", "Unknown", "0.99")),
                    received + Duration::from_secs(1),
                    BASE_MS + 1_000
                )
                .unwrap()
                .is_empty()
        );
        let next = tracker
            .frame(
                &frame(2_000, "module", &object("7", "Human", "0.25")),
                received + Duration::from_secs(2),
                BASE_MS + 2_000,
            )
            .unwrap();
        assert_ne!(started(&first).id, started(&next).id);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn policy_gates_do_not_confuse_analytics_modules_with_camera_tokens() {
        let received = Instant::now();
        let input = frame(0, "analytics-module", &object("7", "Human", "0.25"));
        for (mode, metadata_stream, expected) in [
            (EventMode::Disabled, MetadataMode::Enabled, 0),
            (EventMode::Auto, MetadataMode::Disabled, 0),
            (EventMode::Auto, MetadataMode::Enabled, 1),
        ] {
            let policy = EventConfig {
                mode,
                metadata_stream,
                source_tokens: vec!["video-source-token".to_owned()],
                ..EventConfig::default()
            };
            let mut tracker = Tracker::new("camera".to_owned(), policy, true);
            let changes = tracker.frame(&input, received, BASE_MS).unwrap();
            assert_eq!(changes.len(), expected);
            if expected == 1 {
                let payload = started(&changes).payload.as_ref().unwrap();
                assert_eq!(payload["analytics_module"], "analytics-module");
                assert_eq!(payload["object_id"], "7");
                assert!(payload["source_token"].is_null());
            }
        }
    }
}
