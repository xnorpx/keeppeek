use super::*;

#[test]
fn runtime_profile_recording_rejection_is_degraded_and_keeps_live_media() {
    let (storage, respond) = StorageHandle::with_test_ack_queue(8);
    let writer = std::thread::spawn(move || respond(vec![Some(Ok(())); 2]));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let camera = recording_admission_test_camera(listener.local_addr().unwrap());
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    recorder.add_camera(&camera, true, true).unwrap();
    let _connection = accept_rtsp_connection(&listener);
    let profiles = serde_json::from_value(serde_json::json!({
        "revision": 1, "active_profile_id": "away", "profiles": {"away": {
            "id": "away", "name": "Away", "revision": 1,
            "cameras": {camera.config.ip.to_string(): {"recording_mode": "event-only"}}
        }}
    }))
    .unwrap();
    let applications = recorder.apply_runtime_profiles(profiles).unwrap();
    let live = recorder.camera_workers.contains_key(&camera.config.ip);
    recorder.stop_camera(camera.config.ip);
    assert_eq!(writer.join().unwrap(), 2);
    assert!(live, "recording rejection must not disable live media");
    assert_eq!(
        applications[0].state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    assert!(
        applications[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("invalid event configuration"))
    );
    assert_eq!(
        recorder.runtime_profile_status.read().unwrap().cameras[&camera.config.ip.to_string()]
            .state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
}

fn recording_admission_test_camera(address: std::net::SocketAddr) -> Camera {
    let mut camera = runtime_rtsp_camera(address);
    camera.config.recording_mode = crate::cameras::CameraRecordingMode::Sub;
    camera.config.event_recording_duration_secs = 0;
    camera.config.events.mode = crate::cameras::events::EventMode::Disabled;
    camera
}

#[test]
fn runtime_profile_recording_disconnection_keeps_startup_live_and_degraded() {
    let (storage, drain) = StorageHandle::with_test_queue(1);
    drop(drain);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let camera = recording_admission_test_camera(listener.local_addr().unwrap());
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    recorder.add_camera(&camera, true, true).unwrap();
    let _connection = accept_rtsp_connection(&listener);
    let application = recorder.runtime_profile_status.read().unwrap().cameras
        [&camera.config.ip.to_string()]
        .clone();
    let live = recorder.camera_workers.contains_key(&camera.config.ip);
    recorder.stop_camera(camera.config.ip);
    assert!(live);
    assert_eq!(
        application.state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    assert!(application.reason.is_some());
}

#[test]
fn runtime_profile_recording_rejection_still_disables_another_camera() {
    let (storage, respond) = StorageHandle::with_test_ack_queue(8);
    let writer = std::thread::spawn(move || respond(vec![Some(Ok(())); 4]));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let camera = recording_admission_test_camera(listener.local_addr().unwrap());
    let mut other = camera.clone();
    other.config.ip = "127.0.0.2".parse().unwrap();
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    recorder.add_camera(&camera, true, true).unwrap();
    recorder.add_camera(&other, true, true).unwrap();
    let _first = accept_rtsp_connection(&listener);
    let _second = accept_rtsp_connection(&listener);
    let profiles = serde_json::from_value(serde_json::json!({
        "revision": 1, "active_profile_id": "away", "profiles": {"away": {
            "id": "away", "name": "Away", "revision": 1, "cameras": {
                camera.config.ip.to_string(): {"recording_mode": "event-only"},
                other.config.ip.to_string(): {"availability": false}
            }
        }}
    }))
    .unwrap();
    let applications = recorder.apply_runtime_profiles(profiles).unwrap();
    let other_stopped = !recorder.camera_workers.contains_key(&other.config.ip);
    recorder.stop_camera(camera.config.ip);
    assert_eq!(writer.join().unwrap(), 4);
    assert!(other_stopped);
    assert_eq!(applications.len(), 2);
    for ip in [camera.config.ip, other.config.ip] {
        assert!(
            applications
                .iter()
                .any(|application| application.camera_id == ip.to_string())
        );
    }
    for application in applications {
        assert_eq!(
            application.state,
            if application.camera_id == camera.config.ip.to_string() {
                crate::profiles::RuntimeApplicationState::Degraded
            } else {
                crate::profiles::RuntimeApplicationState::Applied
            }
        );
    }
}

#[test]
fn runtime_profile_recording_configures_once_and_recovers_after_a_full_queue() {
    let (storage, respond) = StorageHandle::with_test_ack_queue(1);
    let acknowledge = || {
        let respond = respond.clone();
        std::thread::spawn(move || respond(vec![Some(Ok(()))]))
    };
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let camera = recording_admission_test_camera(listener.local_addr().unwrap());
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage));
    recorder.set_runtime_profiles(
        serde_json::from_value(serde_json::json!({
            "revision": 1, "active_profile_id": "away", "profiles": {"away": {
                "id": "away", "name": "Away", "revision": 1,
                "cameras": {camera.config.ip.to_string(): {"availability": false}}
            }}
        }))
        .unwrap(),
    );
    let writer = acknowledge();
    recorder.add_camera(&camera, true, true).unwrap();
    assert_eq!(writer.join().unwrap(), 1);
    let profiles = crate::profiles::ProfileRegistry {
        revision: 2,
        ..Default::default()
    };
    let writer = acknowledge();
    let initial = recorder.apply_runtime_profiles(profiles.clone()).unwrap();
    assert_eq!(writer.join().unwrap(), 1);
    let _connection = accept_rtsp_connection(&listener);
    let stalled = recorder.apply_runtime_profiles(profiles.clone()).unwrap();
    let full = recorder.apply_runtime_profiles(profiles.clone()).unwrap();
    assert_eq!(respond(vec![Some(Ok(()))]), 1);
    let writer = acknowledge();
    let recovered = recorder.apply_runtime_profiles(profiles).unwrap();
    assert_eq!(writer.join().unwrap(), 1);
    recorder.stop_camera(camera.config.ip);
    assert_eq!(
        initial[0].state,
        crate::profiles::RuntimeApplicationState::Applied
    );
    assert_eq!(
        full[0].state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    assert_eq!(
        stalled[0].state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    assert_eq!(
        recovered[0].state,
        crate::profiles::RuntimeApplicationState::Applied
    );
    assert_eq!(
        recorder.runtime_profile_status.read().unwrap().cameras[&camera.config.ip.to_string()]
            .state,
        crate::profiles::RuntimeApplicationState::Applied
    );
}
