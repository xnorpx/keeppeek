use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use test_hikvision::{
    Reply,
    onvif::{FakeOnvif, notification},
};

use super::{
    Client, Credentials, Endpoint, Input, LeaseClock, Operation, Producer, Request, Subscription,
};
use crate::camera_events::registry::Registry;
use crate::cameras::CameraConfig;
use crate::shutdown::Shutdown;

mod lease;
mod limits;
mod queue;
mod recovery;

fn soap(status: u16, body: &str) -> Reply {
    Reply::http(status, "application/soap+xml", envelope(body))
}

fn envelope(body: &str) -> String {
    format!(
        "<s:Envelope xmlns:s='http://www.w3.org/2003/05/soap-envelope' xmlns:e='http://www.onvif.org/ver10/events/wsdl' xmlns:n='http://docs.oasis-open.org/wsn/b-2'><s:Body>{body}</s:Body></s:Envelope>"
    )
}

fn producer(fake: &FakeOnvif) -> (Producer, mpsc::Receiver<Input>) {
    let camera: CameraConfig = toml::from_str(
        "ip='127.0.0.1'\nusername='test'\npassword='test'\n[events]\nmode='onvif-pullpoint'\nsnapshots=false\n",
    )
    .unwrap();
    let shutdown = Shutdown::new();
    let (sent, received) = mpsc::sync_channel(32);
    let slot = Registry::default()
        .install(camera.ip, sent, shutdown.clone(), camera.events.clone())
        .unwrap();
    (
        Producer {
            camera,
            endpoint: format!("{}/onvif/device_service", fake.origin()),
            service: None,
            slot,
            shutdown,
        },
        received,
    )
}

fn finish(
    handle: JoinHandle<anyhow::Result<()>>,
    shutdown: &Shutdown,
    timeout: Duration,
) -> (bool, anyhow::Result<()>) {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() && Instant::now() < deadline {
        shutdown.wait_timeout(Duration::from_millis(10));
    }
    let finished = handle.is_finished();
    shutdown.cancel();
    (finished, handle.join().unwrap())
}

fn subscription(fake: &FakeOnvif) -> (Client, Subscription, Instant) {
    let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
    let mut client = Client::new(
        endpoint.clone(),
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let started = Instant::now();
    let bytes = client
        .execute(
            &Request::create(&endpoint, Duration::from_secs(90)).unwrap(),
            Duration::from_secs(1),
        )
        .unwrap();
    let subscription = Subscription::parse(&endpoint, &bytes).unwrap();
    (client, subscription, started)
}

fn delayed_unavailable() -> Reply {
    Reply::raw(Vec::new()).then(
        Duration::from_millis(1200),
        b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
    )
}

#[test]
fn synchronization_stall_preserves_initial_lease_for_renewal_and_cleanup() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_secs(1))
        .start()
        .unwrap();
    let (producer, _received) = producer(&fake);
    let (mut client, subscription, started) = subscription(&fake);
    fake.next_response(delayed_unavailable()).unwrap();
    for _ in 0..3 {
        fake.next_pull_response(Reply::http(503, "text/plain", "unavailable"))
            .unwrap();
    }
    let shutdown = producer.shutdown.clone();
    let handle = thread::spawn(move || {
        let mut clock = LeaseClock::new(subscription.lease(), started);
        let result = producer.observe(&mut client, &subscription, &mut clock);
        client.execute(
            &subscription.request(Operation::Unsubscribe)?,
            Duration::from_millis(300),
        )?;
        result
    });
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(950));

    assert!(finished, "synchronization consumed the initial lease");
    assert!(result.is_err());
    assert!(
        fake.renew_count() > 0,
        "failed synchronization must renew promptly"
    );
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
}

#[test]
fn pull_stall_is_capped_by_renewal_budget_and_unsubscribes() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_secs(1))
        .start()
        .unwrap();
    let (mut producer, _received) = producer(&fake);
    fake.next_pull_response(delayed_unavailable()).unwrap();
    for _ in 0..2 {
        fake.next_pull_response(Reply::http(503, "text/plain", "unavailable"))
            .unwrap();
    }
    let shutdown = producer.shutdown.clone();
    let handle = thread::spawn(move || producer.subscribe());
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(1100));

    assert!(finished, "pull request outlived its lease budget");
    assert!(result.is_err());
    assert!(fake.renew_count() > 0);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
}

#[test]
fn queue_pressure_unsubscribes_before_two_second_lease_expires() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_secs(2))
        .notifications(vec![notification(
            "VideoSource/MotionAlarm",
            true,
            "Changed",
            "2000-01-01T00:00:00Z",
            "source-1",
        )])
        .start()
        .unwrap();
    let (mut producer, _received) = producer(&fake);
    for _ in 0..32 {
        assert!(producer.slot.try_send(Input::Disconnected).is_ok());
    }
    let shutdown = producer.shutdown.clone();
    let slot = Arc::clone(&producer.slot);
    let handle = thread::spawn(move || producer.subscribe());
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(1600));

    assert!(
        finished,
        "queue pressure kept the producer alive until cancellation"
    );
    assert!(
        result.is_err(),
        "undelivered notifications must be reported"
    );
    assert_eq!(fake.pull_count(), 1);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
    let evidence = slot.evidence.lock().unwrap().clone();
    assert!(evidence.delivery_stalls > 0);
    assert!(evidence.queue_drops > 0);
}
