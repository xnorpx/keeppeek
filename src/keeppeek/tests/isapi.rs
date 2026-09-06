use super::*;
use crate::storage::RecordingCatalog;
use test_hikvision::{EventPart, FakeHikvision, Reply};

#[test]
fn generic_onvif_camera_commits_motion_before_live_delivery_and_unsubscribes() {
    use crate::storage::metadata::EventSource;
    use test_hikvision::onvif::{FakeOnvif, notification};
    let fake = FakeOnvif::builder().start().unwrap();
    let mut camera = runtime_rtsp_camera(fake.address());
    camera.config.username = "test".to_owned();
    camera.config.password = "test".to_owned();
    camera.config.onvif_port = Some(fake.address().port());
    camera.config.record_generic_motion_events = true;
    camera.config.events.mode = crate::cameras::events::EventMode::OnvifPullpoint;
    camera.config.events.snapshots = false;
    let directory = std::env::temp_dir().join(format!("keeppeek-onvif-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
    let store = EventStore::new(catalog.handle(), &directory.join("images"), 0).unwrap();
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    recorder.set_event_store(store.clone());
    let (sent, published) = mpsc::channel();
    let saved = store.clone();
    recorder.set_event_publisher(move |event| {
        assert_eq!(
            saved.event_by_id(&event.id).unwrap().unwrap().revision,
            event.revision
        );
        sent.send(event.clone()).unwrap();
    });
    recorder.add_camera(&camera, false, false).unwrap();
    assert!(fake.wait_for_pulls(1, Duration::from_secs(3)));
    fake.push(notification(
        "VideoSource/MotionAlarm",
        true,
        "Changed",
        &chrono::Utc::now().to_rfc3339(),
        "source-2",
    ))
    .unwrap();
    recorder.handle_event(recorder.rx.recv_timeout(Duration::from_secs(3)).unwrap());
    let started = published.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(started.kind, "motion");
    assert_eq!(started.source, EventSource::Camera);
    recorder.stop_camera(camera.config.ip);
    let ended = published.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(started.id, ended.id);
    assert!(ended.end_time_ms.is_some());
    assert_eq!(fake.unsubscribe_count(), 1);
    drop(recorder);
    drop(store);
    catalog.shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn generic_event_policy_reconfiguration_does_not_cancel_media_workers() {
    use test_hikvision::onvif::FakeOnvif;
    let fake = FakeOnvif::builder().start().unwrap();
    let mut camera = runtime_rtsp_camera(fake.address());
    camera.config.username = "test".to_owned();
    camera.config.password = "test".to_owned();
    camera.config.onvif_port = Some(fake.address().port());
    camera.config.events.mode = crate::cameras::events::EventMode::OnvifPullpoint;
    camera.config.events.snapshots = false;
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    recorder.add_camera(&camera, false, false).unwrap();
    let media_cancel = recorder.camera_workers[&camera.config.ip].shutdown.clone();
    assert!(fake.wait_for_pulls(1, Duration::from_secs(3)));
    camera.config.events.mode = crate::cameras::events::EventMode::Disabled;
    recorder.restart_camera(&camera).unwrap();
    assert!(!media_cancel.is_cancelled());
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
    camera.config.events.mode = crate::cameras::events::EventMode::OnvifPullpoint;
    recorder.restart_camera(&camera).unwrap();
    assert!(fake.wait_for_pulls(2, Duration::from_secs(3)));
    assert!(!media_cancel.is_cancelled());
    recorder.stop_camera(camera.config.ip);
    assert_eq!(fake.unsubscribe_count(), 2);
}

#[test]
fn automatic_events_fall_back_to_onvif_after_isapi_is_unsupported() {
    use test_hikvision::onvif::FakeOnvif;
    let generic = FakeOnvif::builder().start().unwrap();
    let vendor = FakeHikvision::builder().replies([Reply::http(404, "application/xml", "<ResponseStatus><statusCode>4</statusCode><subStatusCode>notSupport</subStatusCode></ResponseStatus>")]).start().unwrap();
    let mut camera = runtime_rtsp_camera(vendor.address());
    camera.config.username = "test".to_owned();
    camera.config.password = "test".to_owned();
    camera.config.manufacturer = Some("Hikvision".to_owned());
    camera.config.http_port = Some(vendor.address().port());
    camera.config.onvif_port = Some(generic.address().port());
    camera.config.events.snapshots = false;
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    recorder.add_camera(&camera, false, false).unwrap();
    let media = recorder.camera_workers[&camera.config.ip].shutdown.clone();
    assert!(generic.wait_for_pulls(1, Duration::from_secs(4)));
    assert_eq!(generic.subscription_count(), 1);
    assert_eq!(vendor.requests().len(), 1);
    assert!(!media.is_cancelled());
    recorder.stop_camera(camera.config.ip);
    assert_eq!(generic.active_subscriptions(), 0);
}

#[test]
fn generic_event_snapshots_are_optional_and_commit_after_the_event() {
    use test_hikvision::onvif::{FakeOnvif, notification};
    let fake = FakeOnvif::builder().start().unwrap();
    let images = FakeHikvision::builder().start().unwrap();
    let mut camera = runtime_rtsp_camera(fake.address());
    camera.config.username = "test".to_owned();
    camera.config.password = "test".to_owned();
    camera.config.onvif_port = Some(fake.address().port());
    camera.config.record_generic_motion_events = true;
    camera.config.events.mode = crate::cameras::events::EventMode::OnvifPullpoint;
    camera.profiles[0].snapshot_uri = Some(format!(
        "{}/ISAPI/Streaming/channels/101/picture",
        images.origin()
    ));
    let directory =
        std::env::temp_dir().join(format!("keeppeek-onvif-snapshot-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
    let store = EventStore::new(catalog.handle(), &directory.join("images"), 0).unwrap();
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    recorder.set_event_store(store.clone());
    let (sent, published) = mpsc::channel();
    recorder.set_event_publisher(move |event| {
        sent.send(event.clone()).unwrap();
    });
    recorder.add_camera(&camera, false, false).unwrap();
    assert!(fake.wait_for_pulls(1, Duration::from_secs(3)));
    fake.push(notification(
        "VideoSource/MotionAlarm",
        true,
        "Changed",
        &chrono::Utc::now().to_rfc3339(),
        "source-2",
    ))
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut revisions = Vec::new();
    while Instant::now() < deadline
        && !revisions
            .iter()
            .any(|event: &crate::storage::metadata::TimelineEvent| !event.attachments.is_empty())
    {
        if let Ok(event) = recorder.rx.recv_timeout(Duration::from_millis(50)) {
            recorder.handle_event(event);
        }
        revisions.extend(published.try_iter());
    }
    recorder.stop_camera(camera.config.ip);
    assert!(revisions.len() >= 2);
    assert!(revisions[0].attachments.is_empty());
    let enriched = revisions
        .iter()
        .find(|event| !event.attachments.is_empty())
        .unwrap();
    assert_eq!(enriched.attachments.len(), 1);
    assert_eq!(enriched.id, revisions[0].id);
    assert!(
        store
            .thumbnail_path(&enriched.camera_id, &enriched.id)
            .unwrap()
            .is_some()
    );
    drop(recorder);
    drop(store);
    catalog.shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn isapi_camera_worker_persists_revisions_before_live_delivery() {
    let server = FakeHikvision::builder()
        .alert_streams([Reply::alert(
            [
                EventPart::motion(true),
                EventPart::motion(true),
                EventPart::motion(false),
            ],
            true,
        )])
        .start()
        .unwrap();
    let address = server.address();
    let mut camera = runtime_rtsp_camera(address);
    camera.config.username = "test".to_owned();
    camera.config.password = "test".to_owned();
    camera.config.manufacturer = Some("ANNKE".to_owned());
    camera.config.http_port = Some(address.port());
    let directory = std::env::temp_dir().join(format!("keeppeek-isapi-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
    let events = EventStore::new(catalog.handle(), &directory.join("thumbnails"), 0).unwrap();
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    recorder.set_event_store(events.clone());
    let (published, received) = mpsc::channel();
    let committed = events.clone();
    recorder.set_event_publisher(move |event| {
        let stored = committed.event_by_id(&event.id).unwrap().unwrap();
        assert_eq!(stored.revision, event.revision);
        assert_eq!(stored.end_time_ms, event.end_time_ms);
        published.send(event.clone()).unwrap();
    });
    recorder.add_camera(&camera, false, false).unwrap();
    assert_eq!(
        recorder.camera_workers[&camera.config.ip]
            .event_handles
            .len(),
        1
    );
    for _ in 0..2 {
        let message = recorder.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        recorder.handle_event(message);
    }
    recorder.stop_camera(camera.config.ip);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.authenticated())
            .count(),
        1
    );
    let started = received.recv_timeout(Duration::from_secs(2)).unwrap();
    let closed = received.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(started.kind, "person");
    assert_eq!(started.id, closed.id);
    assert_eq!(started.revision, 1);
    assert_eq!(closed.revision, 2);
    assert!(closed.end_time_ms.is_some());
    assert_eq!(closed.payload.as_ref().unwrap()["protocol"], "isapi");
    drop(recorder);
    drop(events);
    catalog.shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn isapi_images_commit_together_and_survive_reconciliation() {
    use crate::storage::metadata::{EventAttachment, EventSource, TimelineEvent};
    let directory =
        std::env::temp_dir().join(format!("keeppeek-isapi-images-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
    let root = directory.join("images");
    let events = EventStore::new(catalog.handle(), &root, 0).unwrap();
    let mut images = Vec::new();
    for width in [16, 32] {
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(width, 16)
            .write_to(&mut output, image::ImageFormat::Jpeg)
            .unwrap();
        images.push(output.into_inner());
    }
    let descriptors: Vec<_> = images
        .iter()
        .enumerate()
        .map(|(index, bytes)| EventAttachment {
            id: format!("isapi-image-{index}"),
            attachment_type: "snapshot".to_owned(),
            content_type: "image/jpeg".to_owned(),
            byte_len: Some(bytes.len() as u64),
            ordinal: index as u32,
            timestamp_ms: Some(1000),
            text: None,
        })
        .collect();
    let event = TimelineEvent {
        id: uuid::Uuid::new_v4().to_string(),
        revision: 1,
        camera_id: "192.0.2.10".to_owned(),
        stream: None,
        source: EventSource::Camera,
        kind: "vehicle".to_owned(),
        start_time_ms: 1000,
        end_time_ms: None,
        confidence: Some(0.9),
        bbox: Some([0.1, 0.1, 0.5, 0.5]),
        bbox_attachment_id: Some(descriptors[1].id.clone()),
        zone: None,
        text: Some("TEST123".to_owned()),
        payload: None,
        attachments: descriptors,
        canonical_attachment_id: Some("isapi-image-1".to_owned()),
        icon_key: "vehicle".to_owned(),
        rejected_icon_key: None,
        thumbnail_filename: None,
    };
    let bytes: Vec<_> = event
        .attachments
        .iter()
        .zip(&images)
        .map(|(descriptor, bytes)| {
            (
                descriptor.id.clone(),
                std::sync::Arc::<[u8]>::from(bytes.clone()),
            )
        })
        .collect();
    let committed = events.commit_native_images(event, &bytes).unwrap();
    assert!(committed.canonical_image_owns_bbox());
    for (id, bytes) in &bytes {
        assert_eq!(
            std::fs::read(
                events
                    .attachment_path(&committed.camera_id, &committed.id, id)
                    .unwrap()
                    .unwrap()
            )
            .unwrap()
            .as_slice(),
            bytes.as_ref()
        );
    }
    events.close(&committed.id, 2000).unwrap();
    drop(events);
    let events = EventStore::new(catalog.handle(), &root, 0).unwrap();
    assert_eq!(
        events.event_by_id(&committed.id).unwrap().unwrap().revision,
        2
    );
    for (id, _) in bytes {
        assert!(
            events
                .attachment_path(&committed.camera_id, &committed.id, &id)
                .unwrap()
                .is_some()
        );
    }
    drop(events);
    catalog.shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn fake_hikvision_anpr_images_are_committed_with_the_correct_box_and_revision() {
    let server = FakeHikvision::builder().start().unwrap();
    let jpeg = server
        .resource("/ISAPI/Streaming/channels/101/picture")
        .unwrap();
    let event = br#"{"uuid":"capture-1","eventType":"ANPR","eventState":"active","channelID":1,"ANPR":{"licensePlate":"TEST123","confidenceLevel":97,"vehicleInfo":{"color":"blue"},"pictureInfoList":[{"fileName":"overview.jpg"},{"fileName":"vehicle.jpg","plateRect":{"X":32,"Y":18,"width":64,"height":36}}]}}"#;
    server.enqueue_alert(Reply::alert([
        EventPart::new("image/jpeg", jpeg.clone()).identified("overview.jpg"),
        EventPart::new("application/json", event.to_vec()),
            EventPart::new("image/jpeg", jpeg).identified("vehicle.jpg"),
        EventPart::new("application/json", br#"{"uuid":"capture-1","eventType":"ANPR","eventState":"inactive","channelID":1}"#.to_vec()),
    ], true).fragmented(37).unwrap()).unwrap();
    let mut camera = runtime_rtsp_camera(server.address());
    camera.config.manufacturer = Some("Hikvision".to_owned());
    camera.config.username = "test".to_owned();
    camera.config.password = "test".to_owned();
    camera.config.http_port = Some(server.address().port());
    let directory =
        std::env::temp_dir().join(format!("keeppeek-fake-hikvision-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
    let store = EventStore::new(catalog.handle(), &directory.join("images"), 0).unwrap();
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    recorder.set_event_store(store.clone());
    let (sent, received) = mpsc::channel();
    let committed = store.clone();
    recorder.set_event_publisher(move |event| {
        for descriptor in &event.attachments {
            assert!(
                committed
                    .attachment_path(&event.camera_id, &event.id, &descriptor.id)
                    .unwrap()
                    .is_some()
            );
        }
        sent.send(event.clone()).unwrap();
    });
    recorder.add_camera(&camera, false, false).unwrap();
    for _ in 0..2 {
        let event = recorder.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        recorder.handle_event(event);
    }
    let first = received.recv_timeout(Duration::from_secs(2)).unwrap();
    let closed = received.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(first.kind, "vehicle");
    assert_eq!(first.text.as_deref(), Some("TEST123"));
    assert_eq!(first.attachments.len(), 2);
    assert!(first.canonical_image_owns_bbox());
    assert_eq!(first.bbox, Some([0.1, 0.1, 0.2, 0.2]));
    assert_eq!(
        first.payload.as_ref().unwrap()["image_references"]["vehicle.jpg"],
        first.canonical_attachment_id.clone().unwrap()
    );
    assert_eq!(closed.id, first.id);
    assert_eq!(closed.revision, 2);
    recorder.stop_camera(camera.config.ip);
    drop(recorder);
    drop(store);
    catalog.shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn stopping_fake_hikvision_camera_revokes_callbacks_and_closes_observations() {
    let fake = FakeHikvision::builder().start().unwrap();
    let mut camera = runtime_rtsp_camera(fake.address());
    camera.config.manufacturer = Some("Hikvision".to_owned());
    let ip = camera.config.ip;
    let directory =
        std::env::temp_dir().join(format!("keeppeek-fake-callback-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
    let events = EventStore::new(catalog.handle(), &directory.join("images"), 0).unwrap();
    let stop = Shutdown::new();
    let mut recorder = KeepPeekLoop::new(stop.clone(), None);
    recorder.set_event_store(events.clone());
    let config = crate::isapi::callbacks::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        trusted_proxy: None,
        sources: vec![crate::isapi::callbacks::Source {
            ip,
            channel: 1,
            username: "receiver".to_owned(),
            password: "test-only-password".to_owned(),
        }],
    };
    recorder
        .configure_isapi_callbacks(
            Some(&config),
            &std::collections::HashMap::from([(ip, camera.clone())]),
        )
        .unwrap();
    let address = recorder.isapi_callbacks.as_ref().unwrap().address();
    recorder.add_camera(&camera, false, false).unwrap();
    assert!(recorder.camera_workers[&ip].handles.is_empty());
    let control = recorder.control();
    let (sent, received) = mpsc::channel();
    recorder.set_event_publisher(move |event| {
        sent.send(event.clone()).unwrap();
    });
    let worker = std::thread::spawn(move || recorder.run());
    let destination = format!("http://{address}/ISAPI/Event/notification/callback/{ip}");
    let body = EventPart::callback_body([EventPart::motion(true)]);
    assert_eq!(
        fake.post_callback(
            &destination,
            "multipart/form-data; boundary=camera",
            &body,
            "receiver",
            "test-only-password"
        )
        .unwrap()
        .status(),
        200
    );
    let first = received.recv_timeout(Duration::from_secs(2)).unwrap();
    control.stop_camera(ip).unwrap();
    assert_eq!(
        fake.post_callback(
            &destination,
            "multipart/form-data; boundary=camera",
            &body,
            "receiver",
            "test-only-password"
        )
        .unwrap()
        .status(),
        403
    );
    let closed = received.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(closed.id, first.id);
    assert!(closed.end_time_ms.is_some());
    stop.cancel();
    worker.join().unwrap();
    drop(events);
    catalog.shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}
