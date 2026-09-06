use super::*;
use crate::cameras::{CameraConfig, configured_cameras};
use retina::codec::CompressionType;
use std::collections::HashMap;

pub(super) fn consumer() -> (Consumer, Receiver<KeepPeekEvent>) {
    let config: CameraConfig = toml::from_str(
        "ip='127.0.0.1'\nusername='test'\npassword='test'\nrecord_generic_motion_events=true\n[events]\nsnapshots=false\n",
    )
    .unwrap();
    let camera = configured_cameras(&HashMap::from([("test".to_owned(), vec![config])]))
        .remove(&"127.0.0.1".parse().unwrap())
        .unwrap();
    let registry = crate::camera_events::Registry::default();
    let shutdown = Shutdown::new();
    let (input, received) = mpsc::sync_channel(1);
    let slot = registry
        .install(
            camera.config.ip,
            input,
            shutdown.clone(),
            camera.config.events.clone(),
        )
        .unwrap();
    let (sent, output) = mpsc::sync_channel(1);
    (
        Consumer::new(&camera, sent, received, slot, shutdown, None),
        output,
    )
}

pub(super) fn metadata(received: Instant, source: &str) -> Input {
    let notification = test_hikvision::onvif::notification(
        "VideoSource/MotionAlarm",
        true,
        "Changed",
        "1970-01-01T00:00:01Z",
        source,
    );
    Input::Metadata {
        bytes: format!(
            "<tt:MetadataStream xmlns:tt=\"http://www.onvif.org/ver10/schema\"><tt:Event>{notification}</tt:Event></tt:MetadataStream>"
        )
        .into_bytes(),
        compression: CompressionType::Uncompressed,
        loss: 0,
        received,
        received_ms: 1000,
    }
}

pub(super) fn frame(
    time_ms: i64,
    count: usize,
    class: &str,
    confidence: &str,
) -> onvif::event::Frame {
    let timestamp = chrono::DateTime::from_timestamp_millis(time_ms)
        .unwrap()
        .to_rfc3339();
    let objects = (0..count).map(|index| format!(
        "<tt:Object ObjectId=\"{index}\"><tt:Appearance><tt:Class><tt:Type Likelihood=\"{confidence}\">{class}</tt:Type></tt:Class></tt:Appearance></tt:Object>"
    )).collect::<String>();
    let bytes = format!(
        "<tt:MetadataStream xmlns:tt=\"http://www.onvif.org/ver10/schema\"><tt:VideoAnalytics><tt:Frame UtcTime=\"{timestamp}\" Source=\"module\">{objects}</tt:Frame></tt:VideoAnalytics></tt:MetadataStream>"
    );
    onvif::event::Metadata::parse(bytes.as_bytes())
        .unwrap()
        .frames
        .remove(0)
}

#[test]
fn metadata_cutoff_discards_already_decoded_notifications() {
    let (mut consumer, _output) = consumer();
    consumer.accept(metadata(Instant::now(), "metadata-source"));
    assert_eq!(consumer.work.len(), 1);
    assert!(consumer.slot.try_send(Input::MetadataLost).is_ok());
    assert!(consumer.slot.try_send(Input::MetadataLost).is_err());

    consumer.interruptions();

    assert!(
        consumer.work.is_empty(),
        "old metadata cannot reopen after its cutoff"
    );
}

#[test]
fn consumed_metadata_cutoffs_never_move_backwards() {
    let (mut consumer, _output) = consumer();
    let older = Instant::now();
    let newer = older + Duration::from_secs(2);
    assert!(consumer.slot.try_send(Input::MetadataLost).is_ok());
    assert!(
        consumer
            .slot
            .try_send(metadata(newer, "lost-newer"))
            .is_err()
    );
    consumer.interruptions();
    assert!(
        consumer
            .slot
            .try_send(metadata(older, "lost-older"))
            .is_err()
    );
    consumer.interruptions();

    consumer.accept(metadata(older + Duration::from_secs(1), "stale"));

    assert!(
        consumer.work.is_empty(),
        "a drained latch must not weaken its cutoff"
    );
}

#[test]
fn admitted_metadata_loss_interrupts_without_waiting_for_input_capacity() {
    let (mut consumer, _output) = consumer();
    consumer.accept(metadata(Instant::now(), "metadata-source"));
    assert!(consumer.slot.try_send(Input::MetadataLost).is_ok());

    consumer.interruptions();

    assert!(
        consumer.work.is_empty(),
        "metadata loss must not wait behind data work"
    );
}

#[test]
fn queued_metadata_loss_does_not_reset_objects_opened_after_the_cutoff() {
    let (mut consumer, _output) = consumer();
    assert!(consumer.slot.try_send(Input::MetadataLost).is_ok());
    consumer.interruptions();
    let changes = consumer
        .tracker
        .frame(&frame(1000, 1, "Human", "0.8"), Instant::now(), 1000)
        .unwrap();
    assert!(
        matches!(changes.as_slice(), [KeepPeekEvent::TimelineEventStarted { event }]
        if event.kind == "person")
    );
    consumer.enqueue(changes);

    let marker = consumer.received.try_recv().unwrap();
    consumer.slot.consumed(&marker);
    consumer.accept(marker);

    assert_eq!(consumer.tracker.active_count(), 1);
    assert_eq!(consumer.pending.len(), 1);
}

#[test]
fn pull_cutoff_preserves_work_received_after_the_interruption() {
    let (mut consumer, _output) = consumer();
    assert!(consumer.slot.try_send(Input::MetadataLost).is_ok());
    assert!(consumer.slot.try_send(Input::Disconnected).is_err());
    consumer.accept(metadata(Instant::now() + Duration::from_secs(1), "fresh"));

    assert!(consumer.advance());

    assert_eq!(consumer.pending.len(), 1);
    assert!(
        matches!(consumer.pending.front(), Some(KeepPeekEvent::TimelineEventStarted { event })
        if event.kind == "motion")
    );
}

#[test]
fn metadata_cutoff_fences_just_decoded_input_but_keeps_pull_notifications() {
    let (mut consumer, output) = consumer();
    let received = Instant::now();
    consumer.interruptions();
    consumer.accept(metadata(received, "stale-metadata"));
    let notification = onvif::event::parse_notifications(
        test_hikvision::onvif::notification(
            "VideoSource/MotionAlarm",
            true,
            "Changed",
            "1970-01-01T00:00:01Z",
            "pull-source",
        )
        .as_bytes(),
    )
    .unwrap()
    .remove(0);
    consumer
        .work
        .push_back(Work::Notification(notification, received, 1000));
    assert!(consumer.slot.try_send(Input::MetadataLost).is_ok());

    assert!(consumer.advance());

    assert!(consumer.work.is_empty());
    assert_eq!(consumer.tracker.active_count(), 1);
    assert!(consumer.flush());
    let KeepPeekEvent::NativeBatch { changes, reply, .. } = output.try_recv().unwrap() else {
        panic!("native persistence batch expected");
    };
    assert!(
        matches!(changes.as_slice(), [KeepPeekEvent::TimelineEventStarted { event }]
        if event.kind == "motion" && event.source == crate::storage::metadata::EventSource::Camera)
    );
    reply.send(changes.len()).unwrap();
    let evidence = consumer.slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.queue_drops, 1);
}

#[test]
fn consumed_metadata_property_ends_only_when_its_transport_is_lost() {
    let (mut consumer, _output) = consumer();
    consumer.accept(metadata(Instant::now(), "metadata-source"));
    assert!(consumer.advance());
    assert_eq!(consumer.tracker.active_count(), 1);
    consumer.accept(Input::Disconnected);
    assert_eq!(consumer.tracker.active_count(), 1);
    assert!(consumer.slot.try_send(Input::MetadataLost).is_ok());
    consumer.interruptions();
    assert_eq!(consumer.tracker.active_count(), 0);
    assert!(matches!(
        consumer.pending.back(),
        Some(KeepPeekEvent::TimelineEventEnded { .. })
    ));
    assert_eq!(consumer.pending.len(), 2);
}

#[test]
fn pull_queue_loss_preserves_metadata_received_before_the_cutoff() {
    let (mut consumer, _output) = consumer();
    consumer.accept(metadata(Instant::now(), "metadata-source"));
    assert!(
        consumer
            .slot
            .try_send(Input::Snapshot {
                camera_id: "camera".to_owned(),
                event_id: "unused".to_owned(),
                jpeg: Vec::new(),
            })
            .is_ok()
    );
    assert!(consumer.slot.try_send(Input::Disconnected).is_err());
    assert!(consumer.advance());
    assert_eq!(consumer.tracker.active_count(), 1);
    assert_eq!(consumer.pending.len(), 1);
    assert_eq!(consumer.slot.evidence.lock().unwrap().queue_drops, 0);
}
