use super::*;
use crate::cameras::events::EventMode;
use crate::keeppeek::KeepPeekEvent;
use test_hikvision::onvif::{FakeOnvif, notification};

struct Events {
    ip: IpAddr,
    registry: Registry,
    shutdown: Shutdown,
    received: Receiver<KeepPeekEvent>,
    handles: Vec<JoinHandle<()>>,
}

impl Events {
    fn new(fake: &FakeOnvif) -> Self {
        let mut camera = configured_camera();
        camera.config.onvif_port = Some(fake.address().port());
        camera.config.events.mode = EventMode::OnvifPullpoint;
        camera.config.record_generic_motion_events = true;
        let registry = Registry::default();
        let shutdown = Shutdown::new();
        let (sent, received) = mpsc::sync_channel(8);
        let handles =
            crate::camera_events::spawn(&camera, sent, registry.clone(), shutdown.clone()).unwrap();
        Self {
            ip: camera.config.ip,
            registry,
            shutdown,
            received,
            handles,
        }
    }

    fn changes(&self) -> Vec<KeepPeekEvent> {
        let message = self.received.recv_timeout(Duration::from_secs(3)).unwrap();
        let KeepPeekEvent::NativeBatch { changes, reply, .. } = message else {
            panic!("native batch expected");
        };
        reply.send(changes.len()).unwrap();
        changes
    }

    fn start_event(&self, fake: &FakeOnvif, source: &str) -> String {
        fake.push(notification(
            "VideoSource/MotionAlarm",
            true,
            "Changed",
            &chrono::Utc::now().to_rfc3339(),
            source,
        ))
        .unwrap();
        let changes = self.changes();
        let [KeepPeekEvent::TimelineEventStarted { event }] = changes.as_slice() else {
            panic!("one motion start expected");
        };
        assert_eq!(event.kind, "motion");
        assert!(event.attachments.is_empty());
        assert!(event.bbox_attachment_id.is_none());
        event.id.clone()
    }

    fn wait_for_snapshot_failure(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.registry.snapshot(self.ip).unwrap().snapshot_failures == 1 {
                return;
            }
            assert!(matches!(
                self.received.recv_timeout(Duration::from_millis(10)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
        }
        panic!("missing snapshot evidence was not counted");
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        self.shutdown.cancel();
        let deadline = Instant::now() + Duration::from_secs(6);
        while self.handles.iter().any(|handle| !handle.is_finished()) && Instant::now() < deadline {
            if let Ok(KeepPeekEvent::NativeBatch { changes, reply, .. }) =
                self.received.recv_timeout(Duration::from_millis(20))
            {
                reply.send(changes.len()).unwrap();
            }
        }
        assert!(self.handles.iter().all(JoinHandle::is_finished));
        for handle in self.handles.drain(..) {
            handle.join().unwrap();
        }
    }
}

#[test]
fn generic_event_snapshots_use_late_metadata_without_losing_events() {
    let fake = FakeOnvif::builder().start().unwrap();
    let images = FakeHikvision::builder().start().unwrap();
    let events = Events::new(&fake);
    assert_eq!(events.handles.len(), 3);
    assert!(fake.wait_for_pulls(1, Duration::from_secs(3)));
    let first_id = events.start_event(&fake, "source-before-discovery");
    events.wait_for_snapshot_failure();
    assert!(images.requests().is_empty());
    let uri = format!("{}{SNAPSHOT_PATH}", images.origin());
    assert!(events.registry.record_snapshot(events.ip, Some(&uri)));
    let second_id = events.start_event(&fake, "source-after-discovery");
    assert_ne!(first_id, second_id);
    let changes = events.changes();
    let [
        KeepPeekEvent::TimelineEventThumbnail {
            camera_id,
            event_id,
            jpeg,
        },
    ] = changes.as_slice()
    else {
        panic!("one snapshot expected after live discovery");
    };
    assert_eq!(camera_id, &events.ip.to_string());
    assert_eq!(event_id, &second_id);
    assert_eq!(jpeg, &images.resource(SNAPSHOT_PATH).unwrap());
    assert!(crate::storage::events::jpeg_dimensions(jpeg).is_ok());
    let evidence = events.registry.snapshot(events.ip).unwrap();
    assert_eq!(evidence.snapshot_failures, 1);
    assert_eq!(evidence.snapshots, 1);
    assert_eq!(evidence.dropped, 0);
    drop(events);
    assert_eq!(fake.unsubscribe_count(), 1);
}
