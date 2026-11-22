use super::*;
use crate::profiles::{ProfileRegistry, RuntimeApplicationState};

#[test]
fn profile_stop_gives_healthy_workers_the_shared_grace_period() {
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    let mut targets = BTreeMap::new();
    for octet in 1..=2 {
        let mut camera = runtime_rtsp_camera((Ipv4Addr::LOCALHOST, 0).into());
        let ip = Ipv4Addr::new(127, 0, 0, octet).into();
        camera.config.ip = ip;
        recorder.camera_definitions.insert(ip, camera);
        targets.insert(
            ip.to_string(),
            crate::profiles::CameraProfileOverride {
                availability: Some(false),
                ..Default::default()
            },
        );
    }
    // The first camera visited stalls regardless of HashMap iteration order.
    let mut order = recorder.camera_definitions.keys().copied();
    let stalled = order.next().unwrap();
    let healthy = order.next().unwrap();
    let (release, blocked) = mpsc::channel();
    let mut workers = CameraWorkers::new();
    workers.handles.push(std::thread::spawn(move || {
        let _ = blocked.recv();
    }));
    recorder.camera_workers.insert(stalled, workers);
    let mut workers = CameraWorkers::new();
    let shutdown = workers.shutdown.clone();
    workers.handles.push(std::thread::spawn(move || {
        assert!(shutdown.wait_timeout(2 * CAMERA_STOP_BUDGET));
        std::thread::sleep(Duration::from_millis(30));
    }));
    recorder.camera_workers.insert(healthy, workers);
    let profile = crate::profiles::RuntimeProfile {
        id: "away".into(),
        name: "Away".into(),
        revision: 1,
        cameras: targets,
    };
    let selection = ProfileRegistry {
        revision: 1,
        active_profile_id: Some(profile.id.clone()),
        profiles: BTreeMap::from([(profile.id.clone(), profile)]),
    };
    recorder.apply_runtime_profiles(selection).unwrap();
    let status = recorder.runtime_profile_status.read().unwrap().clone();
    assert_eq!(
        status.cameras[&healthy.to_string()].state,
        RuntimeApplicationState::Applied
    );
    assert_eq!(
        status.cameras[&stalled.to_string()].state,
        RuntimeApplicationState::Degraded
    );
    assert!(recorder.camera_workers.is_empty());
    assert_eq!(recorder.retiring_workers.len(), 1);
    release.send(()).unwrap();
    recorder
        .finish_camera_stop(stalled, Instant::now() + CAMERA_STOP_BUDGET)
        .unwrap();
}

#[test]
fn profile_reapply_uses_saved_definition_after_restart_and_start_failures() {
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    let mut camera = runtime_rtsp_camera((Ipv4Addr::LOCALHOST, 0).into());
    let ip = camera.config.ip;
    recorder.camera_definitions.insert(ip, camera.clone());
    let mut workers = CameraWorkers::new();
    workers.configuration = Some(camera.config.clone());
    let (release, blocked) = mpsc::channel();
    workers.handles.push(std::thread::spawn(move || {
        let _ = blocked.recv();
    }));
    recorder.camera_workers.insert(ip, workers);
    camera.config.username = "rotated".into();
    assert!(recorder.restart_camera(&camera).is_err());
    assert_eq!(recorder.camera_definitions[&ip].config.username, "rotated");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    camera.config.main_rtsp_url = Some(format!(
        "rtsp://{}/replacement",
        listener.local_addr().unwrap()
    ));
    assert!(recorder.add_camera(&camera, true, true).is_err());
    assert_eq!(
        recorder.camera_definitions[&ip].config.main_rtsp_url,
        camera.config.main_rtsp_url
    );
    let profile = ProfileRegistry {
        revision: 1,
        ..Default::default()
    };
    let blocked = recorder.apply_runtime_profiles(profile.clone()).unwrap();
    assert_eq!(blocked[0].state, RuntimeApplicationState::Degraded);
    assert!(recorder.camera_workers.is_empty());
    assert!(recorder.retiring_workers.contains_key(&ip));
    release.send(()).unwrap();
    recorder
        .finish_camera_stop(ip, Instant::now() + CAMERA_STOP_BUDGET)
        .unwrap();
    let recovered = recorder.apply_runtime_profiles(profile).unwrap();
    assert_eq!(recovered[0].state, RuntimeApplicationState::Applied);
    let _connection = accept_rtsp_connection(&listener);
    let running = recorder.camera_workers[&ip].configuration.as_ref().unwrap();
    assert_eq!(running.username, "rotated");
    assert_eq!(running.main_rtsp_url, camera.config.main_rtsp_url);
    recorder.stop_camera(ip).unwrap();
}

#[test]
fn profile_reapply_recovers_a_timed_out_event_edit_without_replacing_media() {
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    let mut camera = runtime_rtsp_camera((Ipv4Addr::LOCALHOST, 0).into());
    let ip = camera.config.ip;
    recorder.camera_definitions.insert(ip, camera.clone());
    let mut workers = CameraWorkers::new();
    workers.configuration = Some(camera.config.clone());
    let media = workers.shutdown.clone();
    let (release, blocked) = mpsc::channel();
    workers.event_handles.push(std::thread::spawn(move || {
        let _ = blocked.recv();
    }));
    recorder.camera_workers.insert(ip, workers);
    camera.config.record_generic_motion_events = true;
    assert!(recorder.restart_camera(&camera).is_err());
    assert!(
        recorder.camera_definitions[&ip]
            .config
            .record_generic_motion_events
    );
    let profile = ProfileRegistry {
        revision: 1,
        ..Default::default()
    };
    let blocked = recorder.apply_runtime_profiles(profile.clone()).unwrap();
    assert_eq!(blocked[0].state, RuntimeApplicationState::Degraded);
    release.send(()).unwrap();
    let deadline = Instant::now() + CAMERA_STOP_BUDGET;
    while !recorder.camera_workers[&ip].event_handles[0].is_finished() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let recovered = recorder.apply_runtime_profiles(profile).unwrap();
    assert_eq!(recovered[0].state, RuntimeApplicationState::Applied);
    let workers = &recorder.camera_workers[&ip];
    assert!(
        workers
            .configuration
            .as_ref()
            .unwrap()
            .record_generic_motion_events
    );
    assert!(!media.is_cancelled());
    assert!(!workers.event_shutdown.is_cancelled());
    recorder.stop_camera(ip).unwrap();
}
