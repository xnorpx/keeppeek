use std::time::{Duration, Instant};

use super::{
    Tracker,
    tests::{BASE_MS, notification, started},
};
use crate::{cameras::events::EventConfig, keeppeek::KeepPeekEvent};

#[test]
fn metadata_only_property_closes_on_metadata_loss_and_rejects_replay() {
    let received = Instant::now();
    let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
    let message = notification("Changed", true, 0);
    let opening = tracker.apply_metadata(&message, received, BASE_MS).unwrap();
    let ending = tracker.disconnect_metadata("metadata_lost");
    assert!(
        matches!(ending.as_slice(), [KeepPeekEvent::TimelineEventEnded { id, end_time_ms }]
        if id == &started(&opening).id && *end_time_ms == BASE_MS)
    );
    assert!(
        tracker
            .apply_metadata(&message, received, BASE_MS)
            .unwrap()
            .is_empty()
    );
    assert_eq!(tracker.active_count(), 0);
}

#[test]
fn matching_pull_copy_keeps_a_metadata_property_open_until_both_sources_disconnect() {
    let received = Instant::now();
    let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
    let message = notification("Changed", true, 0);
    let opening = tracker.apply_metadata(&message, received, BASE_MS).unwrap();
    assert!(
        tracker
            .apply(&message, received, BASE_MS)
            .unwrap()
            .is_empty()
    );
    assert!(tracker.disconnect_metadata("metadata_lost").is_empty());
    assert_eq!(tracker.active_count(), 1);
    let ending = tracker.disconnect_pullpoint("pull_lost");
    assert!(
        matches!(ending.as_slice(), [KeepPeekEvent::TimelineEventEnded { id, .. }]
        if id == &started(&opening).id)
    );
    assert!(tracker.disconnect_pullpoint("pull_lost").is_empty());
}

#[test]
fn matching_metadata_copy_survives_pullpoint_loss_without_duplicate_opening() {
    let received = Instant::now();
    let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
    let message = notification("Changed", true, 0);
    let opening = tracker.apply(&message, received, BASE_MS).unwrap();
    assert!(
        tracker
            .apply_metadata(&message, received, BASE_MS)
            .unwrap()
            .is_empty()
    );
    assert!(tracker.disconnect_pullpoint("pull_lost").is_empty());
    assert_eq!(tracker.active_count(), 1);
    let ending = tracker.disconnect_metadata("metadata_lost");
    assert!(
        matches!(ending.as_slice(), [KeepPeekEvent::TimelineEventEnded { id, .. }]
        if id == &started(&opening).id)
    );
}

#[test]
fn stale_other_transport_copy_does_not_claim_current_property_ownership() {
    let received = Instant::now();
    let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
    let opening = tracker
        .apply_metadata(
            &notification("Changed", true, 1000),
            received,
            BASE_MS + 1000,
        )
        .unwrap();
    assert!(
        tracker
            .apply(
                &notification("Changed", true, 0),
                received + Duration::from_secs(1),
                BASE_MS + 2000,
            )
            .unwrap()
            .is_empty()
    );
    let ending = tracker.disconnect_metadata("metadata_lost");
    assert!(
        matches!(ending.as_slice(), [KeepPeekEvent::TimelineEventEnded { id, .. }]
        if id == &started(&opening).id)
    );
    assert_eq!(tracker.active_count(), 0);
}

#[test]
fn initialized_snapshot_does_not_claim_active_property_ownership() {
    let received = Instant::now();
    let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
    tracker
        .apply_metadata(&notification("Changed", true, 0), received, BASE_MS)
        .unwrap();
    assert!(
        tracker
            .apply(
                &notification("Initialized", true, 1000),
                received + Duration::from_secs(1),
                BASE_MS + 1000,
            )
            .unwrap()
            .is_empty()
    );
    assert_eq!(tracker.disconnect_metadata("metadata_lost").len(), 1);
    assert_eq!(tracker.active_count(), 0);
}

#[test]
fn delayed_different_class_copy_cannot_keep_metadata_replacement_open() {
    let received = Instant::now();
    let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
    let classified = |class: &str, offset| {
        super::tests::message(
            "VideoSource/MotionAlarm",
            Some("Changed"),
            &format!(
                r#"<tt:SimpleItem Name="State" Value="true"/><tt:SimpleItem Name="Class" Value="{class}"/>"#
            ),
            offset,
        )
    };
    let first = tracker
        .apply(&classified("Human", 0), received, BASE_MS)
        .unwrap();
    assert_eq!(started(&first).kind, "person");
    let replacement = tracker
        .apply_metadata(
            &classified("Vehicle", 1000),
            received + Duration::from_secs(1),
            BASE_MS + 1000,
        )
        .unwrap();
    assert!(
        matches!(replacement.last(), Some(KeepPeekEvent::TimelineEventStarted { event }) if event.kind == "vehicle")
    );
    assert!(
        tracker
            .apply(
                &classified("Human", 1000),
                received + Duration::from_secs(2),
                BASE_MS + 2000,
            )
            .unwrap()
            .is_empty()
    );
    assert_eq!(tracker.disconnect_metadata("metadata_lost").len(), 1);
    assert_eq!(tracker.active_count(), 0);
}

#[test]
fn receipt_timestamp_clear_is_not_replayed_against_a_fast_camera_clock() {
    let received = Instant::now();
    for timestamp in ["", "malformed"] {
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .apply(&notification("Changed", true, 1000), received, BASE_MS)
            .unwrap();
        let xml = super::tests::notification_xml(
            "VideoSource/MotionAlarm",
            Some("Changed"),
            r#"<tt:SimpleItem Name="State" Value="false"/>"#,
            0,
        );
        let camera_time = chrono::DateTime::from_timestamp_millis(BASE_MS)
            .unwrap()
            .to_rfc3339();
        let timestamp_attribute = format!(r#"UtcTime="{camera_time}""#);
        let fallback_attribute = if timestamp.is_empty() {
            String::new()
        } else {
            format!(r#"UtcTime="{timestamp}""#)
        };
        let xml = xml.replace(&timestamp_attribute, &fallback_attribute);
        let receipt = chrono::DateTime::from_timestamp_millis(BASE_MS + 100).unwrap();
        let clear = onvif::event::parse_notifications_at(xml.as_bytes(), receipt)
            .unwrap()
            .remove(0);
        let changes = tracker
            .apply(&clear, received + Duration::from_millis(100), BASE_MS + 100)
            .unwrap();
        assert!(
            matches!(changes.as_slice(), [KeepPeekEvent::TimelineEventEnded { end_time_ms, .. }]
            if *end_time_ms == BASE_MS + 100),
            "missing and invalid UTC must still end the event"
        );
        assert_eq!(tracker.active_count(), 0);
        assert!(
            tracker
                .apply(
                    &notification("Changed", true, 900),
                    received + Duration::from_millis(1100),
                    BASE_MS + 1100,
                )
                .unwrap()
                .is_empty(),
            "an older camera observation must not reopen after a receipt-time clear"
        );
    }
}
