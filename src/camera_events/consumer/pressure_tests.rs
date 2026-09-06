use super::continuity_tests::{consumer, frame, metadata};
use super::*;
use crate::storage::metadata::EventSource;

fn apply_frame(
    consumer: &mut Consumer,
    received: Instant,
    time_ms: i64,
    class: &str,
    confidence: &str,
) {
    consumer.work.push_back(Work::Frame(
        frame(time_ms, 128, class, confidence),
        received,
        time_ms,
    ));
    assert!(consumer.advance());
    assert!(consumer.work.is_empty());
}

fn drain(consumer: &mut Consumer, output: &Receiver<KeepPeekEvent>) -> Vec<KeepPeekEvent> {
    let mut committed = Vec::with_capacity(PENDING_MAX);
    for _ in 0..=PENDING_MAX / 128 {
        assert!(consumer.flush());
        if consumer.pending.is_empty() {
            return committed;
        }
        let KeepPeekEvent::NativeBatch { changes, reply, .. } = output.try_recv().unwrap() else {
            panic!("native persistence batch expected");
        };
        assert!(changes.len() <= 128);
        let count = changes.len();
        committed.extend(changes);
        reply.send(count).unwrap();
    }
    panic!("bounded pending changes did not drain");
}

fn assert_vehicle_endings(changes: &[KeepPeekEvent]) {
    let mut starts = 0;
    for (index, change) in changes.iter().enumerate() {
        if let KeepPeekEvent::TimelineEventStarted { event } = change {
            assert_eq!(event.kind, "vehicle");
            assert!(matches!(event.source, EventSource::Camera));
            assert!(changes[index + 1..].iter().any(|change| matches!(change,
                KeepPeekEvent::TimelineEventEnded { id, end_time_ms }
                    if id == &event.id && *end_time_ms == 2000)));
            starts += 1;
        }
    }
    assert_eq!(starts, 128);
}

#[test]
fn withheld_ack_reserves_all_endings_and_drains_after_queue_saturation() {
    let (mut consumer, output) = consumer();
    let received = Instant::now();
    apply_frame(&mut consumer, received, 1000, "Human", "0.8");
    assert_eq!(drain(&mut consumer, &output).len(), 128);
    apply_frame(
        &mut consumer,
        received + Duration::from_millis(100),
        1100,
        "Human",
        "0.9",
    );
    assert!(consumer.pending.is_empty());
    for _ in 0..PENDING_INPUT_MAX - 1 {
        consumer.accept(Input::Snapshot {
            camera_id: "camera".to_owned(),
            event_id: "event".to_owned(),
            jpeg: Vec::new(),
        });
    }
    assert!(consumer.flush());
    let KeepPeekEvent::NativeBatch { changes, reply, .. } = output.try_recv().unwrap() else {
        panic!("native persistence batch expected");
    };
    assert_eq!(changes.len(), 128);
    apply_frame(
        &mut consumer,
        received + Duration::from_secs(1),
        2000,
        "Vehicle",
        "0.9",
    );
    assert_eq!(consumer.pending.len(), PENDING_INPUT_MAX - 1 + 128 * 3);
    assert!(consumer.slot.try_send(metadata(received, "stale")).is_ok());
    assert!(
        consumer
            .slot
            .try_send(metadata(received + Duration::from_secs(2), "lost"))
            .is_err()
    );

    for offset in 0..8 {
        assert!(
            consumer
                .slot
                .try_send(metadata(received + Duration::from_secs(2 + offset), "lost"))
                .is_err()
        );
        assert!(consumer.advance());
        assert!(consumer.flush());
        assert!(matches!(output.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(consumer.pending.len(), PENDING_INPUT_MAX - 1 + 128 * 4);
        assert_eq!(consumer.tracker.active_count(), 0);
    }
    reply.send(changes.len()).unwrap();
    let committed = drain(&mut consumer, &output);
    assert_vehicle_endings(&committed);
    assert_eq!(
        committed
            .iter()
            .filter(|change| matches!(change, KeepPeekEvent::TimelineEventEnded { .. }))
            .count(),
        256
    );
    assert_input_recovery(&mut consumer, &output, received);
}

fn assert_input_recovery(
    consumer: &mut Consumer,
    output: &Receiver<KeepPeekEvent>,
    received: Instant,
) {
    assert!(consumer.pending.is_empty());
    assert!(consumer.advance());
    assert!(consumer.work.is_empty());
    assert!(
        consumer
            .slot
            .try_send(metadata(received + Duration::from_secs(12), "fresh"))
            .is_ok()
    );
    assert!(consumer.advance());
    assert!(consumer.advance());
    assert!(
        matches!(drain(consumer, output).as_slice(), [KeepPeekEvent::TimelineEventStarted { event }]
        if event.kind == "motion" && event.source == EventSource::Camera)
    );
}

#[test]
fn snapshot_bytes_remain_bounded_until_ack_without_using_the_ending_reserve() {
    let (mut consumer, output) = consumer();
    consumer.accept(Input::Snapshot {
        camera_id: "camera".to_owned(),
        event_id: "event".to_owned(),
        jpeg: vec![0; SNAPSHOT_BYTES_MAX],
    });
    assert!(consumer.flush());
    let KeepPeekEvent::NativeBatch { changes, reply, .. } = output.try_recv().unwrap() else {
        panic!("native persistence batch expected");
    };
    assert!(
        matches!(changes.as_slice(), [KeepPeekEvent::TimelineEventThumbnail { jpeg, .. }]
        if jpeg.len() == SNAPSHOT_BYTES_MAX)
    );
    consumer.accept(Input::Snapshot {
        camera_id: "camera".to_owned(),
        event_id: "event".to_owned(),
        jpeg: vec![0],
    });
    assert_eq!(consumer.pending.len(), 1);
    consumer.work.push_back(Work::Frame(
        frame(1000, 1, "Human", "0.8"),
        Instant::now(),
        1000,
    ));
    assert!(consumer.advance());
    assert!(consumer.slot.try_send(Input::MetadataLost).is_ok());
    assert!(consumer.advance());
    assert_eq!(consumer.pending.len(), 3);
    assert_eq!(consumer.tracker.active_count(), 0);
    reply.send(changes.len()).unwrap();
    let committed = drain(&mut consumer, &output);
    assert!(
        matches!(committed.as_slice(), [KeepPeekEvent::TimelineEventStarted { event },
        KeepPeekEvent::TimelineEventEnded { id, end_time_ms }]
        if event.id == *id && *end_time_ms == 1000)
    );
    consumer.accept(Input::Snapshot {
        camera_id: "camera".to_owned(),
        event_id: "event".to_owned(),
        jpeg: vec![0; SNAPSHOT_BYTES_MAX],
    });
    assert_eq!(consumer.pending.len(), 1);
    let evidence = consumer.slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.snapshot_failures, 1);
}

#[test]
fn shutdown_finishes_within_six_seconds_when_native_ack_is_withheld() {
    let (mut consumer, output) = consumer();
    consumer.work.push_back(Work::Frame(
        frame(1000, 1, "Human", "0.8"),
        Instant::now(),
        1000,
    ));
    assert!(consumer.advance());
    assert!(consumer.flush());
    let KeepPeekEvent::NativeBatch {
        changes,
        reply,
        lifetime,
        ..
    } = output.try_recv().unwrap()
    else {
        panic!("native persistence batch expected");
    };
    assert!(matches!(
        changes.as_slice(),
        [KeepPeekEvent::TimelineEventStarted { .. }]
    ));
    let slot = Arc::clone(&consumer.slot);
    consumer.shutdown.cancel();
    let (finished, completion) = mpsc::sync_channel(1);
    let started = Instant::now();
    let worker = std::thread::spawn(move || {
        consumer.run();
        finished.send(()).unwrap();
    });

    completion.recv_timeout(Duration::from_secs(6)).unwrap();
    worker.join().unwrap();

    assert!(started.elapsed() < Duration::from_secs(6));
    assert!(!lifetime.load(std::sync::atomic::Ordering::Acquire));
    assert!(reply.send(0).is_err());
    let evidence = slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.active, 0);
    assert_eq!(evidence.state, "stopped");
    assert_eq!(evidence.dropped, 2);
}
