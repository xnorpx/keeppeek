use super::*;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

fn installed(capacity: usize) -> (Registry, Arc<Slot>, Receiver<Input>) {
    let registry = Registry::default();
    let (sent, received) = mpsc::sync_channel(capacity);
    let slot = registry
        .install(
            "127.0.0.1".parse().unwrap(),
            sent,
            Shutdown::new(),
            EventConfig::default(),
        )
        .unwrap();
    (registry, slot, received)
}

fn metadata(received: Instant) -> Input {
    Input::Metadata {
        bytes: vec![0],
        compression: CompressionType::Uncompressed,
        loss: 0,
        received,
        received_ms: 1000,
    }
}

#[test]
fn full_slot_latches_metadata_without_weakening_the_cutoff() {
    let (registry, slot, _received) = installed(1);
    let older = Instant::now();
    let newer = older + Duration::from_secs(1);
    assert!(slot.try_send(metadata(older)).is_ok());
    for received in [newer, older] {
        registry.metadata(
            "127.0.0.1".parse().unwrap(),
            StreamKind::Main,
            CompressionType::Uncompressed,
            0,
            b"metadata",
            received,
        );
    }

    assert_eq!(slot.take_interruptions(), (None, Some(newer)));
    assert_eq!(slot.queued.load(Ordering::Acquire), 1);
}

#[test]
fn byte_pressure_latches_metadata_and_releases_capacity_after_consumption() {
    let (_registry, slot, received) = installed(2);
    assert!(
        slot.try_send(Input::Snapshot {
            camera_id: "camera".to_owned(),
            event_id: "event".to_owned(),
            jpeg: vec![0; QUEUED_BYTES_MAX],
        })
        .is_ok()
    );
    let cutoff = Instant::now();
    assert!(slot.try_send(metadata(cutoff)).is_err());

    assert_eq!(slot.take_interruptions(), (None, Some(cutoff)));
    assert_eq!(slot.queued.load(Ordering::Acquire), QUEUED_BYTES_MAX);
    let input = received.try_recv().unwrap();
    slot.consumed(&input);
    assert_eq!(slot.queued.load(Ordering::Acquire), 0);
    assert!(
        slot.try_send(metadata(cutoff + Duration::from_secs(1)))
            .is_ok()
    );
    let input = received.try_recv().unwrap();
    slot.consumed(&input);
    assert_eq!(slot.queued.load(Ordering::Acquire), 0);
}

#[test]
fn pull_admission_failure_remains_retryable_without_a_disconnect() {
    let (_registry, slot, received) = installed(1);
    let timestamp = Instant::now();
    assert!(slot.try_send(metadata(timestamp)).is_ok());
    let retry = slot
        .try_send(Input::Pull {
            bytes: b"pull".to_vec(),
            received: timestamp,
            received_ms: 1000,
        })
        .unwrap_err();

    assert_eq!(slot.take_interruptions(), (None, None));
    let first = received.try_recv().unwrap();
    slot.consumed(&first);
    assert!(slot.try_send(retry).is_ok());
    let retried = received.try_recv().unwrap();
    assert!(matches!(&retried, Input::Pull { received, .. } if *received == timestamp));
    slot.consumed(&retried);
    assert_eq!(slot.queued.load(Ordering::Acquire), 0);
}

#[test]
fn metadata_owner_handoff_preserves_the_triggering_document() {
    let (registry, slot, received) = installed(2);
    let ip = "127.0.0.1".parse().unwrap();
    let earlier = Instant::now() - Duration::from_secs(6);
    registry.metadata(
        ip,
        StreamKind::Main,
        CompressionType::Uncompressed,
        0,
        b"old",
        earlier,
    );
    let old = received.try_recv().unwrap();
    slot.consumed(&old);
    let current = Instant::now();
    registry.metadata(
        ip,
        StreamKind::Sub,
        CompressionType::Uncompressed,
        0,
        b"point",
        current,
    );
    let (_, cutoff) = slot.take_interruptions();
    assert!(
        cutoff.is_some_and(|cutoff| cutoff >= earlier && cutoff < current),
        "handoff must fence old documents without fencing its first new document"
    );
    let current_input = received
        .try_iter()
        .find(|input| matches!(input, Input::Metadata { .. }))
        .unwrap();
    assert!(
        matches!(&current_input, Input::Metadata { received, bytes, .. }
        if *received == current && bytes == b"point")
    );
    slot.consumed(&current_input);
    assert_eq!(slot.queued.load(Ordering::Acquire), 0);
}

#[test]
fn explicit_metadata_loss_preserves_a_waiting_replacement_profile_document() {
    for consume_signal in [false, true] {
        let (registry, slot, received) = installed(3);
        let ip = "127.0.0.1".parse().unwrap();
        registry.metadata(
            ip,
            StreamKind::Main,
            CompressionType::Uncompressed,
            0,
            b"old",
            Instant::now(),
        );
        let old = received.try_recv().unwrap();
        slot.consumed(&old);
        let waiting = Instant::now();
        registry.metadata_lost(ip, StreamKind::Main);
        let consumed_cutoff = if consume_signal {
            slot.take_interruptions().1
        } else {
            None
        };
        registry.metadata(
            ip,
            StreamKind::Sub,
            CompressionType::Uncompressed,
            0,
            b"point",
            waiting,
        );
        let (_, cutoff) = slot.take_interruptions();
        let cutoff = cutoff.or(consumed_cutoff).unwrap();
        let input = received
            .try_iter()
            .find(|input| matches!(input, Input::Metadata { .. }))
            .unwrap();
        assert!(
            matches!(&input, Input::Metadata { received, bytes, .. } if *received > cutoff && bytes == b"point"),
            "replacement admission must follow the reset even if wire receipt preceded it"
        );
        slot.consumed(&input);
        assert_eq!(slot.queued.load(Ordering::Acquire), 0);
    }
}
