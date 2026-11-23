use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use test_hikvision::{
    Reply,
    onvif::{FakeOnvif, notification},
};

use super::super::DeliveryTimeout;
use super::{
    Input, LeaseClock, assert_unsubscribed_after_first_pull, finish, producer, subscription,
};

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
fn delivery_timeout_preserves_the_earlier_deadline_for_reset() {
    let fake = camera();
    let (producer, _received) = producer(&fake);
    for _ in 0..32 {
        assert!(producer.slot.try_send(Input::MetadataLost).is_ok());
    }
    let deadline = Instant::now() + Duration::from_millis(25);

    let delivery = producer.send(Input::MetadataLost, deadline).unwrap_err();
    let delivery = delivery.downcast_ref::<DeliveryTimeout>().unwrap();
    assert_eq!(delivery.deadline, deadline);

    let reset = producer
        .send(Input::Disconnected, delivery.deadline)
        .unwrap_err();
    let reset = reset.downcast_ref::<DeliveryTimeout>().unwrap();
    assert_eq!(reset.deadline, deadline);
    let evidence = producer.slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.queue_drops, 2);
    assert_eq!(evidence.delivery_stalls, 2);
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
    let pulled = fake.wait_for_pulls(1, Duration::from_secs(5));
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(2300));
    assert!(pulled, "worker did not reach its first pull: {result:?}");

    assert!(
        finished,
        "reset delivery started a second full pressure wait"
    );
    result.unwrap();
    assert_eq!(fake.pull_count(), 1);
    assert_unsubscribed_after_first_pull(&fake);
    assert_eq!(fake.subscription_count(), 1);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
    let evidence = slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.state, "delivery-stalled");
    assert_eq!(evidence.queue_drops, 2);
    assert_eq!(evidence.delivery_stalls, 2);
}

#[test]
fn an_early_renewal_precedes_the_pull_that_stalls() {
    let fake = camera();
    let (producer, _received) = producer(&fake);
    for _ in 0..32 {
        assert!(producer.slot.try_send(Input::MetadataLost).is_ok());
    }
    let (mut client, subscription, started) = subscription(&fake);
    // A failed synchronization request asks for a renewal before the first pull.
    fake.next_response(Reply::http(503, "text/plain", "unavailable"))
        .unwrap();
    let shutdown = producer.shutdown.clone();
    let handle = thread::spawn(move || {
        let mut clock = LeaseClock::new(subscription.lease(), started);
        let result = producer.observe(&mut client, &subscription, &mut clock);
        producer.unsubscribe(&mut client, &subscription, clock)?;
        result
    });
    let pulled = fake.wait_for_pulls(1, Duration::from_secs(5));
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(2300));

    assert!(
        pulled && finished,
        "worker did not finish its stalled pull: {result:?}"
    );
    assert!(result.is_err_and(|error| error.is::<DeliveryTimeout>()));
    assert_eq!(fake.pull_count(), 1);
    assert!(
        fake.renew_count() >= 1,
        "synchronization failure must renew the lease"
    );
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_unsubscribed_after_first_pull(&fake);
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
    let pulled = fake.wait_for_pulls(1, Duration::from_secs(5));
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(2300));
    assert!(pulled, "worker did not reach its first pull: {result:?}");

    assert!(finished, "closed receiver kept the producer alive");
    result.unwrap();
    assert_eq!(fake.pull_count(), 1);
    assert_unsubscribed_after_first_pull(&fake);
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
