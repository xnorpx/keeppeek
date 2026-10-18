use super::*;
use crate::storage::{
    MediaFrame, RecordingFrame, RecordingStreamIdentity, StorageConfig, StorageEngine, VideoCodec,
    VideoFrame,
};
use std::sync::{Arc, atomic::AtomicUsize};

fn recording_health_fixture() -> (KeepPeekLoop, StorageEngine, std::path::PathBuf, Camera) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-output")
        .join(format!("profile-write-failure-{}", uuid::Uuid::new_v4()));
    let medium = root.join("medium");
    std::fs::create_dir_all(&medium).unwrap();
    let config = StorageConfig::from_toml(&crate::config::StorageToml {
        medium_term_path: Some(medium.to_string_lossy().into_owned()),
        long_term_path: Some(root.join("archive").to_string_lossy().into_owned()),
        short_term_secs: 0,
        flush_interval_secs: 0,
        minimum_free_gb: 0,
        warning_free_gb: 0,
        critical_free_gb: 0,
        ..Default::default()
    });
    let storage = StorageEngine::start(config);
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), Some(storage.handle()));
    let mut camera = runtime_rtsp_camera("127.0.0.1:9".parse().unwrap());
    camera.config.recording_mode = crate::cameras::CameraRecordingMode::Sub;
    recorder
        .camera_workers
        .insert(camera.config.ip, CameraWorkers::new());
    recorder
        .camera_definitions
        .insert(camera.config.ip, camera.clone());
    (recorder, storage, medium, camera)
}

fn recording_health_keyframe() -> RecordingFrame {
    RecordingFrame {
        received_at: Instant::now(),
        timestamp: None,
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: true,
            width: 320,
            height: 240,
            data: vec![
                0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
                0x3c, 0x80, 0, 0, 0, 1, 0x65,
            ]
            .into(),
        }),
    }
}

fn apply_recording_health_revision(recorder: &mut KeepPeekLoop, revision: u64) {
    recorder
        .apply_runtime_profiles(crate::profiles::ProfileRegistry {
            revision,
            ..Default::default()
        })
        .unwrap();
}

fn poll_recording_health(recorder: &mut KeepPeekLoop) {
    recorder.next_recording_status_check = Instant::now();
    recorder.refresh_recording_runtime_status();
}

fn recording_health_busy_events(
    tx: SyncSender<KeepPeekEvent>,
    shutdown: Shutdown,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(1);
        for _ in 0..1_000_000 {
            if shutdown.is_cancelled() || Instant::now() >= deadline {
                return;
            }
            match tx.try_send(KeepPeekEvent::StreamConnected {
                camera_ip: "192.0.2.254".parse().unwrap(),
                stream: StreamKind::Sub,
            }) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => std::thread::yield_now(),
                Err(mpsc::TrySendError::Disconnected(_)) => return,
            }
        }
    })
}

fn wait_for_recording_health_failure(
    status: &std::sync::RwLock<crate::profiles::RuntimeProfileStatus>,
    camera_id: &str,
) -> crate::profiles::CameraRuntimeApplication {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let application = status.read().unwrap().cameras[camera_id].clone();
        if application.state == crate::profiles::RuntimeApplicationState::Degraded
            || Instant::now() >= deadline
        {
            return application;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn runtime_profile_later_write_failure_degrades_the_running_loop_status() {
    let (mut recorder, storage, medium, camera) = recording_health_fixture();
    let publications = Arc::new(AtomicUsize::new(0));
    let observed = publications.clone();
    recorder.set_runtime_profile_status_publisher(move || {
        observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    apply_recording_health_revision(&mut recorder, 1);
    assert_eq!(
        recorder.runtime_profile_status.read().unwrap().cameras[&camera.config.ip.to_string()]
            .state,
        crate::profiles::RuntimeApplicationState::Applied
    );
    std::fs::remove_dir(&medium).unwrap();
    std::fs::write(&medium, b"blocked recording directory").unwrap();
    let status = recorder.runtime_profile_status.clone();
    let shutdown = recorder.shutdown.clone();
    recorder.next_recording_status_check = Instant::now() + Duration::from_millis(25);
    let events = recording_health_busy_events(recorder.tx.clone(), shutdown.clone());
    let worker = std::thread::spawn(move || recorder.run());
    storage.handle().ingest_stream(
        RecordingStreamIdentity::new(camera.config.ip.to_string(), "sub", "friendly-label"),
        recording_health_keyframe(),
    );
    let application = wait_for_recording_health_failure(&status, &camera.config.ip.to_string());
    let observed_while_busy = !events.is_finished();
    shutdown.cancel();
    events.join().unwrap();
    worker.join().unwrap();
    let failed = storage
        .health()
        .snapshot()
        .streams
        .iter()
        .any(|stream| stream.stream_id == "friendly-label/sub" && stream.last_error.is_some());
    storage.shutdown();
    assert!(
        failed,
        "the real storage writer must observe the I/O failure"
    );
    assert_eq!(
        application.state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    assert!(
        observed_while_busy,
        "continuous events must not postpone health polling"
    );
    assert_eq!(publications.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert!(!application.reason.unwrap().contains("friendly-label"));
}

#[test]
fn runtime_profile_recording_health_coalesces_changes_and_preserves_other_failures() {
    let (mut recorder, storage, _, camera) = recording_health_fixture();
    let mut other = camera.clone();
    other.config.ip = "192.0.2.2".parse().unwrap();
    recorder
        .camera_definitions
        .insert(other.config.ip, other.clone());
    recorder
        .camera_workers
        .insert(other.config.ip, CameraWorkers::new());
    apply_recording_health_revision(&mut recorder, 1);
    let status = recorder.runtime_profile_status.clone();
    let publications = Arc::new(AtomicUsize::new(0));
    let observed = publications.clone();
    recorder.set_runtime_profile_status_publisher(move || {
        let _read = status
            .try_read()
            .expect("publish must release the status lock");
        observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    storage
        .handle()
        .note_test_writer_failure(&camera.config.ip.to_string());
    poll_recording_health(&mut recorder);
    {
        let mut status = recorder.runtime_profile_status.write().unwrap();
        assert_eq!(
            status.cameras[&camera.config.ip.to_string()].state,
            crate::profiles::RuntimeApplicationState::Degraded
        );
        assert_eq!(
            status.cameras[&other.config.ip.to_string()].state,
            crate::profiles::RuntimeApplicationState::Applied
        );
        let application = status
            .cameras
            .get_mut(&other.config.ip.to_string())
            .unwrap();
        application.state = crate::profiles::RuntimeApplicationState::Degraded;
        application.reason = Some("notification runtime is unavailable".to_owned());
    }
    storage
        .handle()
        .note_test_writer_failure(&other.config.ip.to_string());
    storage
        .health()
        .note_progress("friendly-label/sub", Duration::from_secs(1));
    poll_recording_health(&mut recorder);
    poll_recording_health(&mut recorder);
    assert_eq!(publications.load(std::sync::atomic::Ordering::Relaxed), 1);
    let status = recorder.runtime_profile_status.read().unwrap();
    assert_eq!(
        status.cameras[&other.config.ip.to_string()]
            .reason
            .as_deref(),
        Some("notification runtime is unavailable")
    );
    assert_eq!(
        status.cameras[&camera.config.ip.to_string()].state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    drop(status);
    storage.shutdown();
}

#[test]
fn runtime_profile_recording_health_preserves_pending_revisions_and_profile_identity() {
    let (mut recorder, storage, _, camera) = recording_health_fixture();
    apply_recording_health_revision(&mut recorder, 1);
    storage
        .handle()
        .note_test_writer_failure(&camera.config.ip.to_string());
    let accepted = recorder.runtime_profile_status.read().unwrap().clone();
    for (revision, profile, state) in [
        (2, None, crate::profiles::RuntimeApplicationState::Pending),
        (
            1,
            Some("newer".to_owned()),
            crate::profiles::RuntimeApplicationState::Applied,
        ),
        (1, None, crate::profiles::RuntimeApplicationState::Pending),
    ] {
        {
            let mut status = recorder.runtime_profile_status.write().unwrap();
            *status = accepted.clone();
            status.revision = revision;
            status.active_profile_id = profile;
            status
                .cameras
                .get_mut(&camera.config.ip.to_string())
                .unwrap()
                .state = state;
        }
        let expected = recorder.runtime_profile_status.read().unwrap().clone();
        poll_recording_health(&mut recorder);
        assert_eq!(*recorder.runtime_profile_status.read().unwrap(), expected);
    }
    storage.shutdown();
}

#[test]
fn runtime_profile_recording_health_retry_and_off_replace_the_latch() {
    let (mut recorder, storage, _, camera) = recording_health_fixture();
    apply_recording_health_revision(&mut recorder, 1);
    storage
        .handle()
        .note_test_writer_failure(&camera.config.ip.to_string());
    poll_recording_health(&mut recorder);
    assert_eq!(
        recorder.runtime_profile_status.read().unwrap().cameras[&camera.config.ip.to_string()]
            .state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    apply_recording_health_revision(&mut recorder, 1);
    poll_recording_health(&mut recorder);
    assert_eq!(
        recorder.runtime_profile_status.read().unwrap().cameras[&camera.config.ip.to_string()]
            .state,
        crate::profiles::RuntimeApplicationState::Applied
    );
    recorder
        .camera_definitions
        .get_mut(&camera.config.ip)
        .unwrap()
        .config
        .recording_mode = crate::cameras::CameraRecordingMode::Off;
    apply_recording_health_revision(&mut recorder, 2);
    storage
        .handle()
        .note_test_writer_failure(&camera.config.ip.to_string());
    poll_recording_health(&mut recorder);
    let status = recorder.runtime_profile_status.read().unwrap();
    assert_eq!(
        status.cameras[&camera.config.ip.to_string()].state,
        crate::profiles::RuntimeApplicationState::Applied
    );
    assert_eq!(
        status.cameras[&camera.config.ip.to_string()]
            .policy
            .recording_mode,
        crate::cameras::CameraRecordingMode::Off
    );
    drop(status);
    storage.shutdown();
}

#[test]
fn runtime_profile_recording_health_poll_throttles_repeated_checks() {
    let (mut recorder, storage, _, camera) = recording_health_fixture();
    apply_recording_health_revision(&mut recorder, 1);
    storage
        .handle()
        .note_test_writer_failure(&camera.config.ip.to_string());
    recorder.next_recording_status_check = Instant::now() + Duration::from_secs(5);
    for _ in 0..1024 {
        recorder.refresh_recording_runtime_status();
    }
    assert_eq!(
        recorder.runtime_profile_status.read().unwrap().cameras[&camera.config.ip.to_string()]
            .state,
        crate::profiles::RuntimeApplicationState::Applied
    );
    poll_recording_health(&mut recorder);
    assert_eq!(
        recorder.runtime_profile_status.read().unwrap().cameras[&camera.config.ip.to_string()]
            .state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    storage.shutdown();
}

#[test]
fn runtime_profile_recording_health_poll_bounds_work_and_reaches_later_cameras() {
    let (mut recorder, storage, _, camera) = recording_health_fixture();
    recorder.camera_definitions.clear();
    recorder.camera_workers.clear();
    for index in 1..=200 {
        let mut camera = camera.clone();
        camera.config.ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, index));
        recorder
            .camera_workers
            .insert(camera.config.ip, CameraWorkers::new());
        recorder.camera_definitions.insert(camera.config.ip, camera);
    }
    apply_recording_health_revision(&mut recorder, 1);
    for camera in recorder.camera_definitions.values() {
        storage
            .handle()
            .note_test_writer_failure(&camera.config.ip.to_string());
    }
    poll_recording_health(&mut recorder);
    let degraded = recorder
        .runtime_profile_status
        .read()
        .unwrap()
        .cameras
        .values()
        .filter(|application| {
            application.state == crate::profiles::RuntimeApplicationState::Degraded
        })
        .count();
    assert_eq!(degraded, 127, "one poll must respect the fixed work budget");
    assert!(recorder.recording_status_cursor.is_some());
    poll_recording_health(&mut recorder);
    let status = recorder.runtime_profile_status.read().unwrap();
    assert_eq!(status.cameras.len(), 200);
    assert!(
        status
            .cameras
            .values()
            .all(|application| application.state
                == crate::profiles::RuntimeApplicationState::Degraded)
    );
    drop(status);
    assert!(recorder.recording_status_cursor.is_none());
    storage.shutdown();
}

#[test]
fn runtime_profile_recording_health_poll_scale() {
    let (mut recorder, storage, _, camera) = recording_health_fixture();
    recorder.camera_definitions.clear();
    recorder.camera_workers.clear();
    for index in 1..=127 {
        let mut camera = camera.clone();
        camera.config.ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, index));
        recorder
            .camera_workers
            .insert(camera.config.ip, CameraWorkers::new());
        recorder.camera_definitions.insert(camera.config.ip, camera);
    }
    apply_recording_health_revision(&mut recorder, 1);
    let mut healthy_samples = Vec::with_capacity(30);
    for _ in 0..30 {
        let started = Instant::now();
        poll_recording_health(&mut recorder);
        healthy_samples.push(started.elapsed().as_micros());
    }
    for camera in recorder.camera_definitions.values() {
        storage
            .handle()
            .note_test_writer_failure(&camera.config.ip.to_string());
    }
    let mut failed_samples = Vec::with_capacity(30);
    for _ in 0..30 {
        for application in recorder
            .runtime_profile_status
            .write()
            .unwrap()
            .cameras
            .values_mut()
        {
            application.state = crate::profiles::RuntimeApplicationState::Applied;
        }
        let started = Instant::now();
        poll_recording_health(&mut recorder);
        failed_samples.push(started.elapsed().as_micros());
    }
    let status = recorder.runtime_profile_status.read().unwrap();
    assert_eq!(status.cameras.len(), 127);
    assert!(
        status
            .cameras
            .values()
            .all(|application| application.state
                == crate::profiles::RuntimeApplicationState::Degraded)
    );
    drop(status);
    healthy_samples.sort_unstable();
    failed_samples.sort_unstable();
    println!(
        "127-camera health poll, 30 samples: healthy_median={}us healthy_p95={}us failure_median={}us failure_p95={}us loop_bytes={}",
        healthy_samples[15],
        healthy_samples[28],
        failed_samples[15],
        failed_samples[28],
        std::mem::size_of::<KeepPeekLoop>()
    );
    storage.shutdown();
}
