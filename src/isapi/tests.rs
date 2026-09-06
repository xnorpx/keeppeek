use super::{Route, spawn};
use crate::cameras::{
    Camera, CameraCapabilities, CameraConfig, CameraPorts, DeviceInfo, MediaProfile,
};
use crate::keeppeek::KeepPeekEvent;
use crate::shutdown::Shutdown;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use test_hikvision::{EventPart, FakeHikvision, Reply};

fn camera() -> Camera {
    Camera {
        config: toml::from_str::<CameraConfig>(
            "ip='127.0.0.1'\nmanufacturer='ANNKE'\nusername='operator'\npassword='test-secret'\n",
        )
        .unwrap(),
        device: DeviceInfo::default(),
        reported_manufacturer: None,
        hostname: None,
        mac_address: None,
        ports: CameraPorts::default(),
        capabilities: CameraCapabilities::default(),
        event_service: None,
        profiles: vec![MediaProfile {
            token: "main".to_owned(),
            name: "Main".to_owned(),
            stream_uri: Some("rtsp://127.0.0.1/Streaming/Channels/101".to_owned()),
            snapshot_uri: None,
            video: None,
            audio: None,
        }],
        is_reolink: false,
        ptz: None,
        imaging: None,
    }
}

#[test]
fn route_uses_known_identity_or_channel_path_but_never_duplicates_reolink_events() {
    let mut camera = camera();
    camera.config.http_port = Some(8080);
    let route = Route::for_camera(&camera).unwrap();
    assert!(!camera.capabilities.events);
    assert_eq!(route.channel, 1);
    assert_eq!(route.origin, "http://127.0.0.1:8080");
    camera.config.manufacturer = None;
    camera.profiles[0].stream_uri = Some("rtsp://127.0.0.1/Streaming/Channels/201".to_owned());
    assert_eq!(Route::for_camera(&camera).unwrap().channel, 2);
    camera.profiles[0].stream_uri = Some("rtsp://127.0.0.1/main".to_owned());
    assert!(Route::for_camera(&camera).is_none());
    assert!(!camera.capabilities.events);
    camera.config.manufacturer = Some("Hikvision".to_owned());
    assert!(Route::for_camera(&camera).is_some());
    camera.is_reolink = true;
    assert!(Route::for_camera(&camera).is_none());
}

#[test]
fn worker_reconnects_coalesces_events_and_stops_without_waiting_for_camera_data() {
    let server = FakeHikvision::builder()
        .credentials("operator", "test-secret")
        .alert_streams([
            Reply::alert(
                [
                    EventPart::motion(true),
                    EventPart::motion(true),
                    EventPart::motion(false),
                ],
                false,
            ),
            Reply::alert([EventPart::motion(true)], false).hold_open(),
        ])
        .start()
        .unwrap();
    let mut camera = camera();
    camera.config.http_port = Some(server.address().port());
    let shutdown = Shutdown::new();
    let (sender, receiver) = mpsc::sync_channel(16);
    let worker = spawn(&camera, sender, None, shutdown.clone())
        .unwrap()
        .unwrap();
    let first = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let clear = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let KeepPeekEvent::TimelineEventStarted { event: first } = first else {
        panic!("expected first start")
    };
    let KeepPeekEvent::TimelineEventEnded { id, .. } = clear else {
        panic!("expected explicit clear")
    };
    let KeepPeekEvent::TimelineEventStarted { event: second } = second else {
        panic!("expected reconnected start")
    };
    assert_eq!(first.kind, "person");
    assert_eq!(first.id, id);
    assert_ne!(first.id, second.id);
    let before = Instant::now();
    shutdown.cancel();
    worker.join().unwrap();
    assert!(before.elapsed() < Duration::from_secs(2));
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.authenticated())
            .count(),
        2
    );
}

#[test]
fn saturated_event_queue_is_bounded_and_counted() {
    let camera = camera();
    let (sender, _receiver) = mpsc::sync_channel(0);
    let mut worker = super::Worker {
        tracker: super::Tracker::new(camera.config.ip, 1, true),
        route: Route::for_camera(&camera).unwrap(),
        camera: camera.config,
        tx: sender,
        storage: None,
        shutdown: Shutdown::new(),
        counts: super::Counts::default(),
        pending: std::collections::VecDeque::new(),
    };
    let before = Instant::now();
    assert!(
        worker
            .publish(vec![KeepPeekEvent::TimelineEventEnded {
                id: "test".to_owned(),
                end_time_ms: 1
            }])
            .is_err()
    );
    assert_eq!(worker.counts.dropped, 0);
    assert_eq!(worker.pending.len(), 1);
    assert!(before.elapsed() < Duration::from_secs(2));
}

#[test]
fn delayed_clear_is_delivered_in_order_after_queue_pressure_recovers() {
    let camera = camera();
    let (sender, receiver) = mpsc::sync_channel(1);
    sender
        .try_send(KeepPeekEvent::TimelineEventEnded {
            id: "existing".to_owned(),
            end_time_ms: 1,
        })
        .unwrap_or_else(|_| panic!("fixture queue should have capacity"));
    let mut worker = super::Worker {
        tracker: super::Tracker::new(camera.config.ip, 1, true),
        route: Route::for_camera(&camera).unwrap(),
        camera: camera.config,
        tx: sender,
        storage: None,
        shutdown: Shutdown::new(),
        counts: super::Counts::default(),
        pending: std::collections::VecDeque::new(),
    };
    let clear = KeepPeekEvent::TimelineEventEnded {
        id: "delayed-clear".to_owned(),
        end_time_ms: 2000,
    };
    assert!(worker.publish(vec![clear]).is_err());
    assert_eq!(worker.pending.len(), 1);
    let KeepPeekEvent::TimelineEventEnded { id, .. } = receiver.try_recv().unwrap() else {
        panic!("expected existing event")
    };
    assert_eq!(id, "existing");
    worker.publish(Vec::new()).unwrap();
    let KeepPeekEvent::TimelineEventEnded { id, end_time_ms } = receiver.try_recv().unwrap() else {
        panic!("expected retained clear")
    };
    assert_eq!(id, "delayed-clear");
    assert_eq!(end_time_ms, 2000);
    assert!(worker.pending.is_empty());
    assert_eq!(worker.counts.transitions, 1);
    assert_eq!(worker.counts.dropped, 0);
}

#[test]
fn invalid_credentials_stop_the_worker_without_a_reconnect_loop() {
    let server = FakeHikvision::builder()
        .credentials("operator", "different-secret")
        .start()
        .unwrap();
    let mut camera = camera();
    camera.config.http_port = Some(server.address().port());
    let shutdown = Shutdown::new();
    let (sender, _receiver) = mpsc::sync_channel(16);
    let worker = spawn(&camera, sender, None, shutdown.clone())
        .unwrap()
        .unwrap();
    server.wait_for_requests(2, Duration::from_secs(3)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !worker.is_finished() && Instant::now() < deadline {
        thread::park_timeout(Duration::from_millis(5));
    }
    let stopped = worker.is_finished();
    shutdown.cancel();
    worker.join().unwrap();
    assert!(
        stopped,
        "invalid credentials must not be retried by the worker"
    );
}
