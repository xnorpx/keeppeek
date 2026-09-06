use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::{FakeOnvif, Subscription, notification};
use super::lifecycle::{EVENTS_NS, WSNT, create, pull};
use super::transport::{Client, texts};
use crate::Reply;

fn wait_for_fragment_consumed(socket: &TcpStream) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        assert!(
            Instant::now() < deadline,
            "request fragment was not consumed"
        );
        match socket.peek(&mut [0]) {
            Ok(count) => {
                assert!(count > 0, "request peer closed before cancellation");
                std::thread::yield_now();
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return;
            }
            Err(error) => panic!("request observation failed: {error}"),
        }
    }
}

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
fn stopped_device_rejects_request_reads_before_waiting_for_socket_data() {
    let fake = FakeOnvif::builder().start().unwrap();
    let shared = Arc::clone(&fake.shared);
    drop(fake);
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (socket, _) = listener.accept().unwrap();
    let started = Instant::now();
    let result = super::super::wire::handle(socket, &shared);
    let elapsed = started.elapsed();
    let error = result.unwrap_err().downcast::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
    assert!(
        elapsed < Duration::from_secs(1),
        "stopped request read elapsed: {elapsed:?}"
    );
    assert!(shared.state.lock().unwrap().requests.is_empty());
}

#[test]
fn cancellation_interrupts_partial_requests_before_and_after_peer_close() {
    for (fragment, close_peer) in [
        ("POST /onvif/events HTTP/1.1\r\n", false),
        (
            "POST /onvif/events HTTP/1.1\r\nContent-Length: 8\r\n\r\n<par",
            false,
        ),
        ("POST /onvif/events HTTP/1.1\r\n", true),
        (
            "POST /onvif/events HTTP/1.1\r\nContent-Length: 8\r\n\r\n<par",
            true,
        ),
    ] {
        let fake = FakeOnvif::builder().start().unwrap();
        let shared = Arc::clone(&fake.shared);
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        let (socket, _) = listener.accept().unwrap();
        let observer = socket.try_clone().unwrap();
        observer
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        client.write_all(fragment.as_bytes()).unwrap();
        assert!(observer.peek(&mut [0]).unwrap() > 0);
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::spawn(move || super::super::wire::handle(socket, &worker_shared));
        wait_for_fragment_consumed(&observer);
        let waiting = client.peek(&mut [0]).unwrap_err();
        assert!(matches!(
            waiting.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        assert!(!worker.is_finished(), "request ended before cancellation");
        let started = Instant::now();
        shared.state.lock().unwrap().stopped = true;
        shared.changed.notify_all();
        if close_peer {
            drop(client);
        }
        let result = worker.join().unwrap();
        let elapsed = started.elapsed();
        let error = result.unwrap_err().downcast::<std::io::Error>().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
        assert!(
            elapsed < Duration::from_secs(1),
            "partial request cancellation elapsed: {elapsed:?}"
        );
        assert!(shared.state.lock().unwrap().requests.is_empty());
    }
}

#[test]
fn slow_request_fragments_survive_short_read_waits() {
    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = TcpStream::connect(fake.address()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(150)))
        .unwrap();
    for fragment in [
        "POST /onvif/events HTTP/1.1\r\n",
        "Content-Length: 7\r\n\r\n",
        "<par",
    ] {
        client.write_all(fragment.as_bytes()).unwrap();
        let waiting = client.peek(&mut [0]).unwrap_err();
        assert!(matches!(
            waiting.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
    }
    client.write_all(b"t/>").unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body(), b"<part/>");
}

#[test]
fn request_deadline_is_shared_across_header_and_body_waits() {
    let fake = FakeOnvif::builder().start().unwrap();
    let shared = Arc::clone(&fake.shared);
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (socket, _) = listener.accept().unwrap();
    let (finished, received) = std::sync::mpsc::sync_channel(1);
    let worker_shared = Arc::clone(&shared);
    let started = Instant::now();
    let worker = std::thread::spawn(move || {
        let result = super::super::wire::handle(socket, &worker_shared);
        finished.send(result).unwrap();
    });
    client
        .write_all(b"POST /onvif/events HTTP/1.1\r\n")
        .unwrap();
    let waiting = client.peek(&mut [0]).unwrap_err();
    assert!(matches!(
        waiting.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    client.write_all(b"Content-Length: 1\r\n\r\n").unwrap();
    let result = received.recv_timeout(Duration::from_secs(6).saturating_sub(started.elapsed()));
    let elapsed = started.elapsed();
    drop(client);
    worker.join().unwrap();
    let error = result
        .expect("request restarted its deadline after headers")
        .unwrap_err()
        .downcast::<std::io::Error>()
        .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        elapsed >= Duration::from_millis(4750),
        "elapsed: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(6), "elapsed: {elapsed:?}");
    assert!(shared.state.lock().unwrap().requests.is_empty());
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
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "sixteen blocked readers shutdown elapsed: {elapsed:?}"
    );
    assert!(shared.state.lock().unwrap().sockets.is_empty());
    assert_eq!(Arc::strong_count(&shared), 1);
    for socket in &mut sockets {
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(socket.read(&mut buffer).unwrap(), 0);
    }
}
