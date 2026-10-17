use super::*;

fn ack_test_camera(address: SocketAddr) -> Camera {
    let mut camera = runtime_rtsp_camera(address);
    camera.config.recording_mode = crate::cameras::CameraRecordingMode::Sub;
    camera.config.events.mode = crate::cameras::events::EventMode::Disabled;
    camera
}

#[test]
fn runtime_profile_stalled_recording_writer_does_not_report_applied() {
    let (storage, drain) = StorageHandle::with_test_queue(8);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let camera = ack_test_camera(listener.local_addr().unwrap());
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    recorder.add_camera(&camera, true, true).unwrap();
    let _connection = accept_rtsp_connection(&listener);
    let live = recorder.camera_workers.contains_key(&camera.config.ip);
    let application = recorder.runtime_profile_status.read().unwrap().cameras
        [&camera.config.ip.to_string()]
        .clone();
    recorder.stop_camera(camera.config.ip);
    assert!(live, "writer timeout must preserve live viewing");
    assert_eq!(drain(), 1, "the configuration entered the queue");
    assert_eq!(
        application.state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    assert!(
        application
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("acknowledg"))
    );
}

#[test]
fn runtime_profile_recording_stays_pending_until_writer_acknowledges() {
    let (storage, respond) = StorageHandle::with_test_ack_queue(1);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let camera = ack_test_camera(listener.local_addr().unwrap());
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    let mut pending = Vec::with_capacity(1);
    recorder
        .begin_add_camera(&camera, true, true, &mut pending)
        .unwrap();
    let state_before = recorder.runtime_profile_status.read().unwrap().cameras
        [&camera.config.ip.to_string()]
        .state
        .clone();
    assert_eq!(respond(vec![Some(Ok(()))]), 1);
    recorder.finish_camera_recordings(pending);
    let state_after = recorder.runtime_profile_status.read().unwrap().cameras
        [&camera.config.ip.to_string()]
        .state
        .clone();
    recorder.stop_camera(camera.config.ip);
    assert_eq!(
        state_before,
        crate::profiles::RuntimeApplicationState::Pending
    );
    assert_eq!(
        state_after,
        crate::profiles::RuntimeApplicationState::Applied
    );
}

#[test]
fn runtime_profile_writer_rejection_preserves_live_and_reports_degraded() {
    let (storage, respond) = StorageHandle::with_test_ack_queue(1);
    let writer =
        std::thread::spawn(move || respond(vec![Some(Err("writer rejected configuration"))]));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let camera = ack_test_camera(listener.local_addr().unwrap());
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    recorder.add_camera(&camera, true, true).unwrap();
    let _connection = accept_rtsp_connection(&listener);
    let live = recorder.camera_workers.contains_key(&camera.config.ip);
    let application = recorder.runtime_profile_status.read().unwrap().cameras
        [&camera.config.ip.to_string()]
        .clone();
    recorder.stop_camera(camera.config.ip);
    assert_eq!(writer.join().unwrap(), 1);
    assert!(live);
    assert_eq!(
        application.state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    assert!(
        application
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("writer rejected"))
    );
}

#[test]
fn runtime_profile_late_writer_ack_preserves_newer_pending_revision() {
    let (storage, respond) = StorageHandle::with_test_ack_queue(1);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let camera = ack_test_camera(listener.local_addr().unwrap());
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    let mut pending = Vec::with_capacity(1);
    recorder
        .begin_add_camera(&camera, true, true, &mut pending)
        .unwrap();
    {
        let mut status = recorder.runtime_profile_status.write().unwrap();
        status.revision = 1;
        status
            .cameras
            .get_mut(&camera.config.ip.to_string())
            .unwrap()
            .state = crate::profiles::RuntimeApplicationState::Pending;
    }
    assert_eq!(respond(vec![Some(Ok(()))]), 1);
    recorder.finish_camera_recordings(pending);
    let status = recorder.runtime_profile_status.read().unwrap().clone();
    recorder.stop_camera(camera.config.ip);
    assert_eq!(status.revision, 1);
    assert_eq!(
        status.cameras[&camera.config.ip.to_string()].state,
        crate::profiles::RuntimeApplicationState::Pending
    );
}

fn ack_batch_cameras() -> HashMap<IpAddr, Camera> {
    (1..=127)
        .map(|index| {
            let mut camera = ack_test_camera("127.0.0.1:9".parse().unwrap());
            camera.config.ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, index));
            (camera.config.ip, camera)
        })
        .collect()
}

fn ack_batch_away(cameras: &HashMap<IpAddr, Camera>) -> crate::profiles::ProfileRegistry {
    crate::profiles::ProfileRegistry {
        revision: 1,
        active_profile_id: Some("away".to_owned()),
        profiles: std::collections::BTreeMap::from([(
            "away".to_owned(),
            crate::profiles::RuntimeProfile {
                id: "away".to_owned(),
                name: "Away".to_owned(),
                revision: 1,
                cameras: cameras
                    .keys()
                    .map(|ip| {
                        (
                            ip.to_string(),
                            crate::profiles::CameraProfileOverride {
                                availability: Some(false),
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
            },
        )]),
    }
}

#[test]
fn runtime_profile_recording_startup_batch_shares_one_ack_deadline() {
    let (storage, drain) = StorageHandle::with_test_queue(128);
    let cameras = ack_batch_cameras();
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    recorder.set_runtime_profiles(ack_batch_away(&cameras));
    let started = Instant::now();
    recorder.add_cameras(&cameras).unwrap();
    let elapsed = started.elapsed();
    let status = recorder.runtime_profile_status.read().unwrap();
    assert_eq!(status.cameras.len(), 127);
    assert!(
        status
            .cameras
            .values()
            .all(|application| application.state
                == crate::profiles::RuntimeApplicationState::Degraded)
    );
    assert_eq!(drain(), 127);
    assert!(elapsed < Duration::from_secs(2), "batch took {elapsed:?}");
    println!(
        "127-camera stalled startup acknowledgement: {}us",
        elapsed.as_micros()
    );
}

#[test]
fn runtime_profile_recording_activation_batch_shares_one_ack_deadline() {
    let (storage, drain) = StorageHandle::with_test_queue(128);
    let cameras = ack_batch_cameras();
    let profiles = ack_batch_away(&cameras);
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    recorder.camera_definitions = cameras;
    let started = Instant::now();
    let applications = recorder.apply_runtime_profiles(profiles).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(applications.len(), 127);
    assert!(
        applications
            .iter()
            .all(|application| application.state
                == crate::profiles::RuntimeApplicationState::Degraded)
    );
    assert_eq!(drain(), 127);
    assert!(elapsed < Duration::from_secs(2), "batch took {elapsed:?}");
    println!(
        "127-camera stalled profile acknowledgement: {}us",
        elapsed.as_micros()
    );
}

#[test]
fn runtime_profile_real_writer_acknowledgement_scale() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-output")
        .join(format!("writer-ack-{}", uuid::Uuid::new_v4()));
    let config = crate::storage::StorageConfig::from_toml(&crate::config::StorageToml {
        medium_term_path: Some(root.to_string_lossy().into_owned()),
        long_term_path: Some(root.to_string_lossy().into_owned()),
        ..Default::default()
    });
    let storage = crate::storage::StorageEngine::start(config);
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage.handle()));
    let mut cameras = ack_batch_cameras();
    for camera in cameras.values_mut() {
        camera.config.recording_mode = crate::cameras::CameraRecordingMode::EventOnly;
        recorder
            .camera_workers
            .insert(camera.config.ip, CameraWorkers::new());
    }
    recorder.camera_definitions = cameras;
    let mut samples = Vec::with_capacity(30);
    for revision in 1..=30 {
        let started = Instant::now();
        let applications = recorder
            .apply_runtime_profiles(crate::profiles::ProfileRegistry {
                revision,
                ..Default::default()
            })
            .unwrap();
        samples.push(started.elapsed().as_micros());
        assert_eq!(applications.len(), 127);
        assert!(
            applications.iter().all(|application| application.state
                == crate::profiles::RuntimeApplicationState::Applied)
        );
    }
    storage.shutdown();
    samples.sort_unstable();
    println!(
        "127 cameras, 30 real writer acknowledgements: median={}us p95={}us max={}us",
        samples[samples.len() / 2],
        samples[samples.len() * 95 / 100],
        samples.last().unwrap()
    );
    assert!(
        samples[samples.len() * 95 / 100] < 250_000,
        "acknowledgement p95 exceeded 250ms"
    );
}
