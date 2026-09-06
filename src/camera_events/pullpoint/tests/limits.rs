use std::thread;
use std::time::Duration;

use test_hikvision::{
    Reply,
    onvif::{FakeOnvif, notification},
};

use super::{Input, finish, producer, soap};

fn limits(timeout: &str, messages: u32) -> Reply {
    soap(
        500,
        &format!(
            "<s:Fault><s:Code><s:Value>s:Receiver</s:Value></s:Code><s:Reason><s:Text xml:lang='en'>limits</s:Text></s:Reason><s:Detail><e:PullMessagesFaultResponse><e:MaxTimeout>{timeout}</e:MaxTimeout><e:MaxMessageLimit>{messages}</e:MaxMessageLimit></e:PullMessagesFaultResponse></s:Detail></s:Fault>"
        ),
    )
}

#[test]
fn non_reducing_fault_limits_fail_and_unsubscribe_without_repeating() {
    let fake = FakeOnvif::builder().start().unwrap();
    for _ in 0..16 {
        fake.next_pull_response(limits("PT10S", 256)).unwrap();
    }
    let (mut producer, _received) = producer(&fake);
    let shutdown = producer.shutdown.clone();
    let handle = thread::spawn(move || producer.subscribe());
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(500));

    assert!(finished, "non-reducing limits were retried indefinitely");
    assert!(result.is_err());
    assert_eq!(fake.pull_count(), 1);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
}

#[test]
fn sub_millisecond_fault_limits_fail_without_zero_timeout_renewal_loop() {
    let fake = FakeOnvif::builder().start().unwrap();
    fake.next_pull_response(limits("PT0.0005S", 32)).unwrap();
    let (mut producer, _received) = producer(&fake);
    let shutdown = producer.shutdown.clone();
    let handle = thread::spawn(move || producer.subscribe());
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(500));

    assert!(
        finished,
        "sub-millisecond limits left the producer spinning"
    );
    assert!(result.is_err());
    assert_eq!(fake.pull_count(), 1);
    assert_eq!(fake.renew_count(), 0);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
}

#[test]
fn smaller_timeout_and_message_limits_deliver_every_notification() {
    let notifications = ["source-1", "source-2"].map(|source| {
        notification(
            "VideoSource/MotionAlarm",
            true,
            "Changed",
            "2000-01-01T00:00:00Z",
            source,
        )
    });
    let fake = FakeOnvif::builder()
        .max_timeout(Duration::from_millis(250))
        .max_message_limit(1)
        .notifications(notifications.to_vec())
        .start()
        .unwrap();
    let (mut producer, received) = producer(&fake);
    let shutdown = producer.shutdown.clone();
    let slot = std::sync::Arc::clone(&producer.slot);
    let handle = thread::spawn(move || producer.subscribe());
    let pulled = fake.wait_for_pulls(3, Duration::from_secs(1));
    shutdown.cancel();
    let (finished, result) = finish(handle, &shutdown, Duration::from_secs(1));
    let mut delivered = Vec::new();
    for input in received.try_iter() {
        slot.consumed(&input);
        if let Input::Pull { bytes, .. } = input {
            delivered.extend(onvif::event::Pull::parse(&bytes).unwrap().notifications);
        }
    }

    assert!(pulled);
    assert!(finished);
    result.unwrap();
    assert_eq!(delivered.len(), 2);
    assert_ne!(delivered[0].source, delivered[1].source);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(slot.evidence.lock().unwrap().queue_drops, 0);
}
