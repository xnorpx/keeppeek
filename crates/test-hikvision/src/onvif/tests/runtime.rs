use std::io::Read;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::{FakeOnvif, Subscription, notification};
use super::lifecycle::{EVENTS_NS, WSNT, create, pull};
use super::transport::{Client, texts};
use crate::Reply;

#[test]
fn push_wakes_a_pending_pull_and_preserves_live_timestamps() {
    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    let worker = std::thread::spawn(move || client.post(&target, &pull("PT60S", 2, &header)));
    assert!(fake.wait_for_pulls(1, Duration::from_secs(1)));
    let message = notification(
        "VideoSource/MotionAlarm",
        true,
        "Changed",
        "2026-09-05T12:00:00Z",
        "live-source",
    );
    let started = Instant::now();
    fake.push(message.clone()).unwrap();
    let (status, body) = worker.join().unwrap();
    assert_eq!(status, 200);
    assert!(body.contains(&message));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        texts(&body, WSNT, "Topic"),
        ["tns1:VideoSource/MotionAlarm"]
    );
    assert!(
        texts(&body, EVENTS_NS, "CurrentTime")[0] < texts(&body, EVENTS_NS, "TerminationTime")[0]
    );
}

#[test]
fn pull_returns_all_available_messages_up_to_the_requested_limit() {
    let messages: Vec<_> = ["MotionAlarm", "Tamper", "MotionAlarm"]
        .into_iter()
        .map(|topic| {
            notification(
                &format!("VideoSource/{topic}"),
                true,
                "Changed",
                "2026-09-05T12:00:00Z",
                "source",
            )
        })
        .collect();
    let fake = FakeOnvif::builder()
        .notifications(messages)
        .start()
        .unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    let (status, body) = client.post(&target, &pull("PT0S", 2, &header));
    assert_eq!(status, 200);
    assert_eq!(
        texts(&body, WSNT, "Topic"),
        ["tns1:VideoSource/MotionAlarm", "tns1:VideoSource/Tamper"]
    );
    let (status, body) = client.post(&target, &pull("PT0S", 2, &header));
    assert_eq!(status, 200);
    assert_eq!(
        texts(&body, WSNT, "Topic"),
        ["tns1:VideoSource/MotionAlarm"]
    );
}

#[test]
fn replacing_a_subscription_cannot_deliver_to_a_stale_waiting_pull() {
    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    let worker = std::thread::spawn(move || client.post(&target, &pull("PT60S", 2, &header)));
    assert!(fake.wait_for_pulls(1, Duration::from_secs(1)));
    {
        let mut state = fake.shared.state.lock().unwrap();
        state.subscriptions = 2;
        state.subscription = Some(Subscription {
            id: 2,
            expires: fake.shared.started.elapsed() + Duration::from_secs(90),
        });
        state
            .notifications
            .push_back("new-subscription-only".to_owned());
    }
    fake.shared.changed.notify_all();
    let (status, body) = worker.join().unwrap();
    assert_eq!(status, 500);
    assert!(body.contains("ResourceUnknown"));
    assert!(!body.contains("new-subscription-only"));
    assert_eq!(fake.shared.state.lock().unwrap().notifications.len(), 1);
}

#[test]
fn drop_interrupts_long_pulls_and_joins_every_worker() {
    let fake = FakeOnvif::builder().start().unwrap();
    let shared = Arc::clone(&fake.shared);
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    let body = pull("PT60S", 1, &header);
    let auth = client.authorization(&target, &body);
    let worker = std::thread::spawn(move || client.try_send(&target, &body, Some(&auth)));
    assert!(fake.wait_for_pulls(1, Duration::from_secs(1)));
    let idle = TcpStream::connect(fake.address()).unwrap();
    let started = Instant::now();
    drop(fake);
    assert!(worker.join().unwrap().is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(shared.state.lock().unwrap().sockets.is_empty());
    assert_eq!(Arc::strong_count(&shared), 1);
    drop(idle);
}

#[test]
fn drop_interrupts_held_open_scripted_pull_bodies() {
    let fake = FakeOnvif::builder().start().unwrap();
    let shared = Arc::clone(&fake.shared);
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    fake.next_pull_response(
        Reply::raw("HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n<partial>")
            .hold_open(),
    )
    .unwrap();
    let body = pull("PT0S", 1, &header);
    let auth = client.authorization(&target, &body);
    let worker = std::thread::spawn(move || client.try_send(&target, &body, Some(&auth)));
    assert!(fake.wait_for_pulls(1, Duration::from_secs(1)));
    let started = Instant::now();
    drop(fake);
    assert!(worker.join().unwrap().is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(Arc::strong_count(&shared), 1);
}

#[test]
fn socket_capacity_is_sixteen_and_drop_closes_blocked_readers() {
    let fake = FakeOnvif::builder().start().unwrap();
    let shared = Arc::clone(&fake.shared);
    let mut sockets: Vec<_> = (0..16)
        .map(|_| TcpStream::connect(fake.address()).unwrap())
        .collect();
    let (state, _) = shared
        .changed
        .wait_timeout_while(
            shared.state.lock().unwrap(),
            Duration::from_secs(2),
            |state| state.sockets.len() < 16,
        )
        .unwrap();
    let count = state.sockets.len();
    drop(state);
    assert_eq!(count, 16);
    let mut excess = TcpStream::connect(fake.address()).unwrap();
    excess
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut buffer = [0];
    match excess.read(&mut buffer) {
        Ok(count) => assert_eq!(count, 0),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
    }
    let started = Instant::now();
    drop(fake);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(shared.state.lock().unwrap().sockets.is_empty());
    assert_eq!(Arc::strong_count(&shared), 1);
    for socket in &mut sockets {
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(socket.read(&mut buffer).unwrap(), 0);
    }
}
