use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use test_hikvision::{
    Reply,
    onvif::{FakeOnvif, notification},
};

use super::{Input, finish, producer};

fn camera() -> FakeOnvif {
    FakeOnvif::builder()
        .lease(Duration::from_secs(2))
        .notifications(vec![notification(
            "VideoSource/MotionAlarm",
            true,
            "Changed",
            "2000-01-01T00:00:00Z",
            "source-1",
        )])
        .start()
        .unwrap()
}

#[test]
fn full_queue_and_reset_share_one_bounded_pressure_wait() {
    let fake = camera();
    let (producer, _received) = producer(&fake);
    for _ in 0..32 {
        assert!(producer.slot.try_send(Input::MetadataLost).is_ok());
    }
    let shutdown = producer.shutdown.clone();
    let slot = Arc::clone(&producer.slot);
    let handle = thread::spawn(move || {
        producer.run();
        Ok(())
    });
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(2300));

    assert!(
        finished,
        "reset delivery started a second full pressure wait"
    );
    result.unwrap();
    assert_eq!(fake.pull_count(), 1);
    assert_eq!(fake.renew_count(), 0);
    assert_eq!(fake.subscription_count(), 1);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
    let evidence = slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.state, "delivery-stalled");
    assert_eq!(evidence.queue_drops, 2);
    assert_eq!(evidence.delivery_stalls, 2);
}

#[test]
fn closed_receiver_stops_worker_and_unsubscribes_without_renewal_loop() {
    let fake = camera();
    let (producer, received) = producer(&fake);
    drop(received);
    let shutdown = producer.shutdown.clone();
    let handle = thread::spawn(move || {
        producer.run();
        Ok(())
    });
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(2300));

    assert!(finished, "closed receiver kept the producer alive");
    result.unwrap();
    assert_eq!(fake.pull_count(), 1);
    assert_eq!(fake.renew_count(), 0);
    assert_eq!(fake.subscription_count(), 1);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
}

#[test]
fn queue_recovery_delivers_the_pending_pull_once_before_its_reset() {
    let fake = camera();
    let (producer, received) = producer(&fake);
    for _ in 0..32 {
        assert!(producer.slot.try_send(Input::MetadataLost).is_ok());
    }
    let shutdown = producer.shutdown.clone();
    let slot = Arc::clone(&producer.slot);
    let handle = thread::spawn(move || {
        producer.run();
        Ok(())
    });
    let deadline = Instant::now() + Duration::from_millis(500);
    while slot.evidence.lock().unwrap().delivery_stalls == 0 && Instant::now() < deadline {
        shutdown.wait_timeout(Duration::from_millis(10));
    }
    let stalled = slot.evidence.lock().unwrap().delivery_stalls > 0;
    fake.next_pull_response(Reply::http(401, "text/plain", "denied"))
        .unwrap();
    let mut notifications = 0;
    let mut reset = false;
    for _ in 0..34 {
        let Ok(input) = received.recv_timeout(Duration::from_millis(300)) else {
            break;
        };
        slot.consumed(&input);
        match input {
            Input::Pull { bytes, .. } => {
                notifications += onvif::event::Pull::parse(&bytes)
                    .unwrap()
                    .notifications
                    .len();
            }
            Input::Disconnected => {
                reset = true;
                break;
            }
            Input::MetadataLost => {}
            _ => panic!("unexpected queue input"),
        }
    }
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(500));

    assert!(stalled);
    assert!(finished);
    result.unwrap();
    assert!(reset);
    assert_eq!(notifications, 1);
    assert_eq!(fake.pull_count(), 2);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(slot.evidence.lock().unwrap().queue_drops, 0);
}
