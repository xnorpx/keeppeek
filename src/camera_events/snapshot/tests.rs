use std::collections::HashMap;
use std::time::Instant;

use super::*;
use crate::cameras::CameraConfig;
use test_hikvision::FakeHikvision;

mod integration;

const SNAPSHOT_PATH: &str = "/ISAPI/Streaming/channels/101/picture";

struct Worker {
    ip: IpAddr,
    registry: Registry,
    slot: Arc<Slot>,
    shutdown: Shutdown,
    jobs: SyncSender<Job>,
    received: Receiver<Input>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    fn new(camera: &Camera) -> Self {
        let registry = Registry::default();
        let shutdown = Shutdown::new();
        let (input, received) = mpsc::sync_channel(8);
        let slot = registry
            .install(
                camera.config.ip,
                input,
                shutdown.clone(),
                camera.config.events.clone(),
            )
            .unwrap();
        let (jobs, handle) = spawn(
            camera,
            Arc::clone(&slot),
            registry.clone(),
            shutdown.clone(),
        )
        .unwrap()
        .expect("enabled snapshots must start without profile evidence");
        Self {
            ip: camera.config.ip,
            registry,
            slot,
            shutdown,
            jobs,
            received,
            handle: Some(handle),
        }
    }

    fn send(&self, event_id: &str) {
        assert!(
            self.jobs
                .try_send(Job {
                    camera_id: self.ip.to_string(),
                    event_id: event_id.to_owned(),
                })
                .is_ok()
        );
    }

    fn jpeg(&self, expected_id: &str) -> Vec<u8> {
        let input = self.received.recv_timeout(Duration::from_secs(3)).unwrap();
        self.slot.consumed(&input);
        let Input::Snapshot {
            camera_id,
            event_id,
            jpeg,
        } = input
        else {
            panic!("snapshot input expected");
        };
        assert_eq!(camera_id, self.ip.to_string());
        assert_eq!(event_id, expected_id);
        assert!(crate::storage::events::jpeg_dimensions(&jpeg).is_ok());
        jpeg
    }

    fn wait_for_failures(&self, count: u64) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.slot.evidence.lock().unwrap().snapshot_failures == count {
                return;
            }
            assert!(matches!(
                self.received.recv_timeout(Duration::from_millis(10)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
        }
        panic!("snapshot worker did not report {count} failures");
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.handle.take().unwrap().join().unwrap();
    }
}

fn configured_camera() -> Camera {
    let config: CameraConfig = toml::from_str(
        "ip='127.0.0.1'\nusername='test'\npassword='test'\n[events]\nsnapshots=true\n",
    )
    .unwrap();
    let ip = config.ip;
    crate::cameras::configured_cameras(&HashMap::from([("test".to_owned(), vec![config])]))
        .remove(&ip)
        .unwrap()
}

#[test]
fn configured_only_worker_counts_missing_endpoint_without_stopping() {
    let camera = configured_camera();
    assert!(
        camera
            .profiles
            .iter()
            .all(|profile| profile.snapshot_uri.is_none())
    );
    let worker = Worker::new(&camera);
    worker.send("before-discovery");
    worker.wait_for_failures(1);
    let evidence = worker.registry.snapshot(camera.config.ip).unwrap();
    assert_eq!(evidence.snapshot_failures, 1);
    assert_eq!(evidence.snapshots, 0);
}

#[test]
fn configured_only_worker_uses_live_endpoint_and_reuses_digest() {
    let images = FakeHikvision::builder().start().unwrap();
    let worker = Worker::new(&configured_camera());
    worker.send("before-discovery");
    worker.wait_for_failures(1);
    assert!(images.requests().is_empty());
    let uri = format!("{}{SNAPSHOT_PATH}", images.origin());
    assert!(worker.registry.record_snapshot(worker.ip, Some(&uri)));
    let expected = images.resource(SNAPSHOT_PATH).unwrap();
    worker.send("after-discovery");
    assert_eq!(worker.jpeg("after-discovery"), expected);
    assert!(!worker.registry.record_snapshot(worker.ip, None));
    worker.send("after-failed-probe");
    assert_eq!(worker.jpeg("after-failed-probe"), expected);
    let requests = images.requests();
    assert_eq!(requests.len(), 3);
    assert!(!requests[0].authenticated());
    assert!(requests[1..].iter().all(|request| request.authenticated()));
    let evidence = worker.registry.snapshot(worker.ip).unwrap();
    assert_eq!(evidence.snapshot_failures, 1);
    assert!(
        !serde_json::to_string(&evidence)
            .unwrap()
            .contains(SNAPSHOT_PATH)
    );
}

#[test]
fn worker_refreshes_origin_and_forgets_removed_endpoint() {
    let first = FakeHikvision::builder().start().unwrap();
    let second = FakeHikvision::builder().start().unwrap();
    let worker = Worker::new(&configured_camera());
    for (images, event_id) in [(&first, "first-origin"), (&second, "second-origin")] {
        let uri = format!("{}{SNAPSHOT_PATH}", images.origin());
        assert!(worker.registry.record_snapshot(worker.ip, Some(&uri)));
        worker.send(event_id);
        assert_eq!(
            worker.jpeg(event_id),
            images.resource(SNAPSHOT_PATH).unwrap()
        );
        let requests = images.requests();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0].authenticated());
        assert!(requests[1].authenticated());
    }
    assert!(
        !worker
            .registry
            .record_snapshot(worker.ip, Some("http://192.0.2.1/private?token=secret"),)
    );
    worker.send("after-rejected-endpoint");
    assert_eq!(
        worker.jpeg("after-rejected-endpoint"),
        second.resource(SNAPSHOT_PATH).unwrap()
    );
    worker.registry.remove(worker.ip);
    worker.send("after-removal");
    worker.wait_for_failures(1);
    assert_eq!(first.requests().len(), 2);
    assert_eq!(second.requests().len(), 3);
}

#[test]
fn disabled_snapshots_do_not_start_a_worker() {
    let mut camera = configured_camera();
    camera.config.events.snapshots = false;
    let registry = Registry::default();
    let shutdown = Shutdown::new();
    let (input, _received) = mpsc::sync_channel(1);
    let slot = registry
        .install(
            camera.config.ip,
            input,
            shutdown.clone(),
            camera.config.events.clone(),
        )
        .unwrap();
    assert!(spawn(&camera, slot, registry, shutdown).unwrap().is_none());
}

#[test]
fn invalid_jpeg_does_not_clear_a_working_endpoint() {
    let images = FakeHikvision::builder()
        .replies([test_hikvision::Reply::http(
            200,
            "image/jpeg",
            [0xff, 0xd8, 0xff, 0xd9],
        )])
        .start()
        .unwrap();
    let worker = Worker::new(&configured_camera());
    let uri = format!("{}{SNAPSHOT_PATH}", images.origin());
    assert!(worker.registry.record_snapshot(worker.ip, Some(&uri)));
    worker.send("invalid-image");
    worker.wait_for_failures(1);
    assert_eq!(
        worker
            .registry
            .snapshot_endpoint(worker.ip)
            .unwrap()
            .as_str(),
        uri
    );
    worker.send("recovered-image");
    assert_eq!(
        worker.jpeg("recovered-image"),
        images.resource(SNAPSHOT_PATH).unwrap()
    );
}

#[test]
fn snapshot_worker_has_four_queued_jobs_and_one_in_flight_request() {
    let stalled = test_hikvision::Reply::raw(
        b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 4\r\n\r\n".to_vec(),
    )
    .hold_open();
    let images = FakeHikvision::builder().replies([stalled]).start().unwrap();
    let worker = Worker::new(&configured_camera());
    let uri = format!("{}{SNAPSHOT_PATH}", images.origin());
    assert!(worker.registry.record_snapshot(worker.ip, Some(&uri)));
    worker.send("in-flight");
    images.wait_for_requests(1, Duration::from_secs(1)).unwrap();
    for index in 0..4 {
        worker.send(&format!("queued-{index}"));
    }
    assert!(matches!(
        worker.jobs.try_send(Job {
            camera_id: worker.ip.to_string(),
            event_id: "excess".to_owned(),
        }),
        Err(mpsc::TrySendError::Full(_))
    ));
    assert!(
        images
            .wait_for_requests(2, Duration::from_millis(100))
            .is_err()
    );
    drop(worker);
    assert_eq!(images.requests().len(), 1);
}
