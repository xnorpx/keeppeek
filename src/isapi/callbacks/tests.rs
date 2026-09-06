use super::*;
use std::net::Ipv4Addr;
use std::sync::mpsc;

fn receiver() -> (Receiver, mpsc::Receiver<crate::keeppeek::KeepPeekEvent>) {
    let (tx, rx) = mpsc::sync_channel(8);
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        trusted_proxy: None,
        sources: vec![Source {
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            channel: 1,
            username: "camera".to_owned(),
            password: "test-only-password".to_owned(),
        }],
    };
    (
        Receiver::new(&config, tx, crate::shutdown::Shutdown::new()).unwrap(),
        rx,
    )
}

#[test]
fn unauthenticated_callbacks_do_not_consume_bodies_and_wrong_sources_are_rejected() {
    let (receiver, _) = receiver();
    let request = rouille::Request::fake_http(
        "POST",
        "/ISAPI/Event/notification/callback/127.0.0.1",
        vec![],
        vec![1, 2, 3],
    );
    let response = receiver.handle(&request);
    assert_eq!(response.status_code, 401);
    assert!(request.data().is_some());
    assert_eq!(
        receiver
            .handle(&rouille::Request::fake_http(
                "POST",
                "/ISAPI/Event/notification/callback/192.0.2.99",
                vec![],
                Vec::new()
            ))
            .status_code,
        403
    );
}

#[test]
fn authenticated_callbacks_acknowledge_committed_batches_and_deduplicate_retries() {
    let (receiver, received) = receiver();
    let uri = "/ISAPI/Event/notification/callback/127.0.0.1";
    let initial = receiver.handle(&rouille::Request::fake_http(
        "POST",
        uri,
        vec![],
        Vec::new(),
    ));
    let challenge = initial
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("www-authenticate"))
        .unwrap()
        .1
        .to_string();
    let mut session =
        ::isapi::Session::new(::isapi::Credentials::new("camera", "test-only-password")).unwrap();
    session.handle_challenge(challenge).unwrap();
    let body = br#"{"uuid":"one","eventType":"VMD","eventState":"active","channelID":1,"detectionTarget":"human"}"#;
    let request =
        ::isapi::Request::with_body(::isapi::Method::Post, uri, ::isapi::Format::Json, body)
            .unwrap();
    let worker = std::thread::spawn(move || {
        let crate::keeppeek::KeepPeekEvent::IsapiBatch { changes, reply, .. } =
            received.recv_timeout(Duration::from_secs(3)).unwrap()
        else {
            panic!("expected callback batch")
        };
        assert_eq!(changes.len(), 1);
        let crate::keeppeek::KeepPeekEvent::TimelineEventStarted { event } = &changes[0] else {
            panic!("expected event")
        };
        assert_eq!(event.kind, "person");
        reply.send(changes.len()).unwrap();
        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
    });
    for _ in 0..2 {
        let authorization = session
            .authorization(&request, "test-cnonce")
            .unwrap()
            .unwrap();
        let incoming = rouille::Request::fake_http(
            "POST",
            uri,
            vec![
                (
                    "Authorization".to_owned(),
                    authorization.as_str().to_owned(),
                ),
                ("Content-Type".to_owned(), "application/json".to_owned()),
            ],
            body.to_vec(),
        );
        assert_eq!(receiver.handle(&incoming).status_code, 200);
    }
    worker.join().unwrap();
}

#[test]
fn fake_hikvision_delivers_real_digest_callbacks_and_listener_stops() {
    let camera = test_hikvision::FakeHikvision::builder().start().unwrap();
    let (tx, rx) = mpsc::sync_channel(8);
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        trusted_proxy: None,
        sources: vec![Source {
            ip: "127.0.0.1".parse().unwrap(),
            channel: 1,
            username: "receiver".to_owned(),
            password: "test-only-password".to_owned(),
        }],
    };
    let stop = Shutdown::new();
    let runtime = Runtime::start(&config, tx, stop).unwrap();
    let destination = format!("http://{}{PATH_PREFIX}127.0.0.1", runtime.address());
    let worker = std::thread::spawn(move || {
        let KeepPeekEvent::IsapiBatch { changes, reply, .. } =
            rx.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("expected native callback batch")
        };
        assert_eq!(changes.len(), 1);
        reply.send(changes.len()).unwrap();
    });
    let body = test_hikvision::EventPart::callback_body([test_hikvision::EventPart::motion(true)]);
    let response = camera
        .post_callback(
            &destination,
            "multipart/form-data; boundary=camera",
            &body,
            "receiver",
            "test-only-password",
        )
        .unwrap();
    assert_eq!(response.status(), 200);
    worker.join().unwrap();
    let started = Instant::now();
    runtime.join();
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn callback_rejection_does_not_wait_for_or_drain_untrusted_body_lengths() {
    use std::io::{Read, Write};
    let camera = test_hikvision::FakeHikvision::builder().start().unwrap();
    let (tx, _rx) = mpsc::sync_channel(1);
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        trusted_proxy: None,
        sources: vec![Source {
            ip: "127.0.0.1".parse().unwrap(),
            channel: 1,
            username: "receiver".to_owned(),
            password: "test-only-password".to_owned(),
        }],
    };
    let runtime = Runtime::start(&config, tx, Shutdown::new()).unwrap();
    for length in [512, 128 * 1024 * 1024] {
        let mut socket = camera.callback_connection(runtime.address()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        write!(socket, "POST {PATH_PREFIX}127.0.0.1 HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/xml\r\nContent-Length: {length}\r\n\r\n").unwrap();
        let mut response = [0; 512];
        let count = socket.read(&mut response).unwrap();
        assert!(
            std::str::from_utf8(&response[..count])
                .unwrap()
                .starts_with("HTTP/1.1 401")
        );
    }
    let started = Instant::now();
    runtime.join();
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn callback_configuration_is_reserved_from_camera_namespaces() {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-callback-config-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let config = directory.join("config.toml");
    crate::config::write_private_file(
        &directory.join("secrets.toml"),
        b"CALLBACK_TEST = 'test-only-password'\n",
    )
    .unwrap();
    crate::config::write_private_file(&config, b"[isapi_callbacks]\nbind='127.0.0.1:0'\n[[isapi_callbacks.sources]]\nip='127.0.0.1'\nchannel=1\nusername='receiver'\npassword='{secret:CALLBACK_TEST}'\n[cameras.fake]\nip='127.0.0.1'\nmanufacturer='Hikvision'\n").unwrap();
    assert!(
        crate::config::load_config(&config)
            .unwrap()
            .isapi_callbacks
            .is_some()
    );
    let cameras = crate::config::load_cameras(&config).unwrap();
    assert_eq!(cameras.len(), 1);
    assert_eq!(cameras["cameras"].len(), 1);
    crate::config::remove_camera(&config, "127.0.0.1".parse().unwrap()).unwrap();
    assert!(
        crate::config::load_config(&config)
            .unwrap()
            .isapi_callbacks
            .is_none()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn rapid_callback_replacement_closes_old_observations_before_reactivation() {
    let (tx, rx) = mpsc::sync_channel(8);
    let ip = "127.0.0.1".parse().unwrap();
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        trusted_proxy: None,
        sources: vec![Source {
            ip,
            channel: 1,
            username: "receiver".to_owned(),
            password: "test-only-password".to_owned(),
        }],
    };
    let runtime = Runtime::start(&config, tx, Shutdown::new()).unwrap();
    let event = ::isapi::Event::parse_json(
        br#"{"eventType":"VMD","eventState":"active","channelID":1,"detectionTarget":"human"}"#,
    )
    .unwrap();
    let entry = &runtime.receiver.entries[&ip];
    entry
        .state
        .lock()
        .unwrap()
        .tracker
        .apply(&event, Instant::now(), 1000)
        .unwrap();
    runtime.deactivate(ip);
    runtime.activate(ip, false);
    runtime.receiver.maintain();
    let KeepPeekEvent::IsapiBatch { changes, reply, .. } =
        rx.recv_timeout(Duration::from_millis(500)).unwrap()
    else {
        panic!("expected old-observation cleanup")
    };
    assert_eq!(changes.len(), 1);
    assert!(matches!(
        changes[0],
        KeepPeekEvent::TimelineEventEnded { .. }
    ));
    reply.send(changes.len()).unwrap();
    runtime.receiver.maintain();
    assert_eq!(
        entry
            .state
            .lock()
            .unwrap()
            .tracker
            .apply(&event, Instant::now(), 2000)
            .unwrap()
            .changes
            .len(),
        1
    );
    runtime.join();
}

#[test]
fn lost_callback_commit_acknowledgment_does_not_resubmit_an_unknown_outcome() {
    let (receiver, rx) = receiver();
    let mut state = receiver
        .entries
        .values()
        .next()
        .unwrap()
        .state
        .lock()
        .unwrap();
    state.pending = Some(Pending::new(
        None,
        state.tracker.clone(),
        vec![KeepPeekEvent::TimelineEventEnded {
            id: "uncertain".to_owned(),
            end_time_ms: 1000,
        }],
        None,
    ));
    state.submit(&receiver.tx).unwrap();
    drop(rx.recv_timeout(Duration::from_millis(100)).unwrap());
    state.reconcile();
    let _ = state.submit(&receiver.tx);
    assert!(rx.try_recv().is_err());
}

#[test]
fn fake_callback_with_unsupported_http_version_releases_its_connection() {
    use std::io::{Read, Write};
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let (tx, _rx) = mpsc::sync_channel(1);
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        trusted_proxy: None,
        sources: vec![Source {
            ip: "127.0.0.1".parse().unwrap(),
            channel: 1,
            username: "receiver".to_owned(),
            password: "test-only-password".to_owned(),
        }],
    };
    let runtime = Runtime::start(&config, tx, Shutdown::new()).unwrap();
    let mut socket = fake.callback_connection(runtime.address()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    socket
        .write_all(b"POST / HTTP/2.0\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let mut response = [0; 512];
    let count = socket.read(&mut response).unwrap();
    assert!(
        std::str::from_utf8(&response[..count])
            .unwrap()
            .contains("505")
    );
    runtime.join();
}

#[test]
fn fake_hikvision_malformed_callbacks_never_enter_the_commit_queue() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        trusted_proxy: None,
        sources: vec![Source {
            ip: "127.0.0.1".parse().unwrap(),
            channel: 1,
            username: "receiver".to_owned(),
            password: "test-only-password".to_owned(),
        }],
    };
    let runtime = Runtime::start(&config, tx, Shutdown::new()).unwrap();
    let destination = format!("http://{}{PATH_PREFIX}127.0.0.1", runtime.address());
    for (media, body) in [
        ("application/xml", b"<EventNotificationAlert>".to_vec()),
        (
            "application/json",
            br#"{"eventType":"VMD","eventType":"ANPR","eventState":"active","channelID":1}"#
                .to_vec(),
        ),
        ("application/json", vec![b' '; 256 * 1024 + 1]),
        (
            "multipart/form-data; boundary=camera",
            b"--camera\r\nContent-Type: application/xml\r\nContent-Length: 20\r\n\r\nshort"
                .to_vec(),
        ),
        (
            "application/json",
            br#"{"eventType":"VMD","eventState":"active","channelID":2,"detectionTarget":"human"}"#
                .to_vec(),
        ),
    ] {
        assert_eq!(
            fake.post_callback(&destination, media, &body, "receiver", "test-only-password")
                .unwrap()
                .status(),
            400
        );
        assert!(rx.try_recv().is_err());
    }
    runtime.join();
}
