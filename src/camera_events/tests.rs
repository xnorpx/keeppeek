use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::cameras::{Camera, CameraConfig};
use crate::keeppeek::KeepPeekEvent;
use crate::shutdown::Shutdown;
use test_hikvision::onvif::{FakeOnvif, notification};

fn camera(fake: &FakeOnvif) -> Camera {
    let config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nusername='test'\npassword='test'\nonvif_port={}\nrecord_generic_motion_events=true\n[events]\nmode='onvif-pullpoint'\nsnapshots=false\n", fake.address().port())).unwrap();
    crate::cameras::configured_cameras(&HashMap::from([("test".to_owned(), vec![config])]))
        .remove(&fake.address().ip())
        .unwrap()
}

#[test]
fn pullpoint_worker_delivers_once_renews_and_unsubscribes_on_shutdown() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_secs(2))
        .start()
        .unwrap();
    let camera = camera(&fake);
    let registry = super::Registry::default();
    let shutdown = Shutdown::new();
    let (sent, received) = mpsc::sync_channel(8);
    let handles = super::spawn(&camera, sent, registry.clone(), shutdown.clone()).unwrap();
    assert!(fake.wait_for_pulls(1, Duration::from_secs(3)));
    let timestamp = chrono::Utc::now().to_rfc3339();
    let event = notification(
        "VideoSource/MotionAlarm",
        true,
        "Changed",
        &timestamp,
        "source-2",
    );
    fake.push(event.clone()).unwrap();
    fake.push(event).unwrap();
    let KeepPeekEvent::NativeBatch { changes, reply, .. } =
        received.recv_timeout(Duration::from_secs(3)).unwrap()
    else {
        panic!("native batch expected");
    };
    assert_eq!(changes.len(), 1);
    let KeepPeekEvent::TimelineEventStarted { event } = &changes[0] else {
        panic!("motion start expected");
    };
    assert_eq!(event.kind, "motion");
    let id = event.id.clone();
    reply.send(changes.len()).unwrap();
    assert!(fake.wait_for_pulls(4, Duration::from_secs(5)));
    assert!(fake.renew_count() > 0);
    shutdown.cancel();
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut endings = 0;
    while handles.iter().any(|handle| !handle.is_finished()) && Instant::now() < deadline {
        if let Ok(KeepPeekEvent::NativeBatch { changes, reply, .. }) =
            received.recv_timeout(Duration::from_millis(50))
        {
            endings += changes.iter().filter(|change| matches!(change, KeepPeekEvent::TimelineEventEnded { id: ended, .. } if ended == &id)).count();
            reply.send(changes.len()).unwrap();
        }
    }
    assert!(handles.iter().all(std::thread::JoinHandle::is_finished));
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(endings, 1);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
    let evidence = registry.snapshot(camera.config.ip).unwrap();
    assert!(evidence.kinds.contains(&"motion".to_owned()));
    assert!(evidence.deduplicated >= 1);
    assert_eq!(evidence.active, 0);
}

#[test]
fn metadata_owner_accepts_gzip_ignores_exi_and_deduplicates_profile_copies() {
    use crate::keeppeek::StreamKind;
    use retina::codec::CompressionType;
    use std::io::Write;

    let fake = FakeOnvif::builder().start().unwrap();
    let mut camera = camera(&fake);
    camera.config.events.mode = crate::cameras::events::EventMode::RtspMetadata;
    let registry = super::Registry::default();
    let shutdown = Shutdown::new();
    let (sent, received) = mpsc::sync_channel(8);
    let handles = super::spawn(&camera, sent, registry.clone(), shutdown.clone()).unwrap();
    let body = format!(
        "<tt:MetadataStream xmlns:tt=\"http://www.onvif.org/ver10/schema\"><tt:Event>{}</tt:Event></tt:MetadataStream>",
        notification(
            "VideoSource/MotionAlarm",
            true,
            "Changed",
            &chrono::Utc::now().to_rfc3339(),
            "source-2"
        )
    );
    let mut encoded = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoded.write_all(body.as_bytes()).unwrap();
    let gzip = encoded.finish().unwrap();
    registry.metadata(
        camera.config.ip,
        StreamKind::Main,
        CompressionType::ExiDefault,
        0,
        b"unsupported",
        Instant::now(),
    );
    registry.metadata(
        camera.config.ip,
        StreamKind::Main,
        CompressionType::GzipCompressed,
        0,
        &gzip,
        Instant::now(),
    );
    registry.metadata(
        camera.config.ip,
        StreamKind::Sub,
        CompressionType::GzipCompressed,
        0,
        &gzip,
        Instant::now(),
    );
    let KeepPeekEvent::NativeBatch { changes, reply, .. } =
        received.recv_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("native event batch expected");
    };
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(&changes[0], KeepPeekEvent::TimelineEventStarted { event } if event.kind == "motion")
    );
    reply.send(changes.len()).unwrap();
    shutdown.cancel();
    let until = Instant::now() + Duration::from_secs(6);
    while handles.iter().any(|handle| !handle.is_finished()) && Instant::now() < until {
        if let Ok(KeepPeekEvent::NativeBatch { changes, reply, .. }) =
            received.recv_timeout(Duration::from_millis(20))
        {
            reply.send(changes.len()).unwrap();
        }
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(fake.subscription_count(), 0);
    let evidence = registry.snapshot(camera.config.ip).unwrap();
    assert_eq!(evidence.metadata_errors, 1);
    assert_eq!(evidence.metadata_documents, 1);
    assert!(evidence.kinds.contains(&"motion".to_owned()));
}
