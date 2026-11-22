use super::*;
use crate::profiles::{CameraProfileOverride, ProfileRegistry, RuntimeProfile};

#[test]
fn failed_profile_identity_refresh_keeps_previous_privacy_deny() {
    let directory = std::env::temp_dir().join(format!(
        "keeppeek-profile-identity-refresh-{}",
        uuid::Uuid::new_v4()
    ));
    let path = directory.join("config.toml");
    std::fs::create_dir_all(&directory).unwrap();
    let old_identity = uuid::Uuid::new_v4().to_string();
    let new_identity = uuid::Uuid::new_v4().to_string();
    let ip = "192.0.2.10";
    let registry = ProfileRegistry {
        revision: 1,
        active_profile_id: Some("private".to_owned()),
        profiles: BTreeMap::from([(
            "private".to_owned(),
            RuntimeProfile {
                id: "private".to_owned(),
                name: "Private".to_owned(),
                revision: 1,
                cameras: BTreeMap::from([(
                    old_identity.clone(),
                    CameraProfileOverride {
                        privacy_enforced: Some(true),
                        ..CameraProfileOverride::default()
                    },
                )]),
            },
        )]),
    };
    let mut root: toml::Table = toml::from_str(&format!(
        "[cameras.front]\nip = '{ip}'\nprofile_identity = '{old_identity}'\n"
    ))
    .unwrap();
    root.insert(
        "runtime_profiles".to_owned(),
        toml::Value::try_from(registry).unwrap(),
    );
    config::write_configuration_table(&path, &root).unwrap();
    let config = config::load_config(&path).unwrap();
    let mut state = ServerState::new(
        &config,
        &HashMap::new(),
        &HashMap::new(),
        &StorageConfig::default(),
        RecordingDemand::new(Duration::ZERO),
        WebRtc::new(),
    );
    state.camera_config_path = Some(path.clone());
    let runtime = crate::keeppeek::KeepPeekLoop::new(Shutdown::new(), None);
    state.camera_runtime = Some(runtime.control());
    drop(runtime);

    root["cameras"]["front"].as_table_mut().unwrap().insert(
        "profile_identity".to_owned(),
        toml::Value::String(new_identity),
    );
    config::write_configuration_table(&path, &root).unwrap();
    assert!(refresh_runtime_profile_camera_ids(&state, &path).is_err());
    assert_eq!(
        state.privacy.profile_overrides_snapshot().get(ip),
        Some(&true)
    );
    assert!(state.privacy.decision(ip, chrono::Utc::now()).unwrap().0);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn failed_camera_removal_retains_configuration_and_privacy_for_an_idempotent_retry() {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-retry-remove-{}", uuid::Uuid::new_v4()));
    let path = directory.join("config.toml");
    let ip = "127.0.0.1";
    let mut state = super::tests::media_test_state().with_camera_config_path(path.clone());
    config::write_private_file(&path, b"[cameras]\n").unwrap();
    let mut camera = state.camera(ip).unwrap().configuration;
    camera.main_rtsp_url = Some("rtsp://127.0.0.1:9/test".into());
    config::upsert_camera(&path, &camera).unwrap();
    let cameras = config::load_cameras(&path).unwrap();
    let configured = crate::cameras::configured_cameras(&cameras);
    let prepared = &configured[&camera.ip];
    state.upsert_camera(camera_entry(&prepared.config, Some(prepared)));
    state
        .privacy
        .replace_profile_overrides(BTreeMap::from([(ip.into(), true)]))
        .unwrap();
    let revision = camera_configuration_revision(&state).unwrap();
    let runtime = crate::keeppeek::KeepPeekLoop::new(Shutdown::new(), None);
    state = state.with_camera_runtime(runtime.control());
    drop(runtime);
    assert!(delete_camera_settings(&state, ip, &revision).is_err());
    assert_eq!(camera_configuration_revision(&state).unwrap(), revision);
    assert!(configured_camera_present(&path, camera.ip).unwrap());
    assert!(state.privacy.decision(ip, chrono::Utc::now()).unwrap().0);

    let shutdown = Shutdown::new();
    let runtime = crate::keeppeek::KeepPeekLoop::new(shutdown.clone(), None);
    state = state.with_camera_runtime(runtime.control());
    let worker = std::thread::spawn(move || runtime.run());
    let removed_revision = delete_camera_settings(&state, ip, &revision).unwrap();
    assert_ne!(removed_revision, revision);
    assert_eq!(
        delete_camera_settings(&state, ip, &revision).unwrap(),
        removed_revision
    );
    assert!(!configured_camera_present(&path, camera.ip).unwrap());
    assert!(state.camera(ip).unwrap().runtime_camera.is_none());
    assert!(!state.privacy.decision(ip, chrono::Utc::now()).unwrap().0);
    assert!(
        !state
            .storage_config
            .volume_groups
            .read()
            .unwrap()
            .contains_key(ip)
    );
    shutdown.cancel();
    worker.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn restarting_an_edited_camera_rechecks_live_status_before_releasing_privacy() {
    use crate::profiles::RuntimeApplicationState;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let directory = std::env::temp_dir().join(format!(
        "keeppeek-edited-camera-privacy-{}",
        uuid::Uuid::new_v4()
    ));
    let path = directory.join("config.toml");
    let ip = "127.0.0.1";
    let mut state = super::tests::media_test_state().with_camera_config_path(path.clone());
    config::write_private_file(&path, b"[cameras]\n").unwrap();
    let mut config = state.camera(ip).unwrap().configuration;
    config.main_rtsp_url = Some("rtsp://127.0.0.1:9/original".into());
    config::upsert_camera(&path, &config).unwrap();
    let configured = config::load_cameras(&path).unwrap();
    let cameras = crate::cameras::configured_cameras(&configured);
    let shutdown = Shutdown::new();
    let mut runtime = crate::keeppeek::KeepPeekLoop::new(shutdown.clone(), None);
    runtime.add_cameras(&cameras).unwrap();
    state = state.with_camera_runtime(runtime.control());
    let status = state.runtime_profile_status.clone();
    let publications = Arc::new(AtomicUsize::new(0));
    let observed = publications.clone();
    runtime.set_runtime_profile_status_publisher(move || {
        if observed.fetch_add(1, Ordering::SeqCst) > 0 {
            // Identity refresh has acknowledged the original. The replacement fails admission.
            for camera in status.write().unwrap().cameras.values_mut() {
                camera.state = RuntimeApplicationState::Degraded;
                camera.reason = Some("replacement recording admission unavailable".into());
            }
        }
    });
    state
        .privacy
        .replace_profile_overrides(BTreeMap::from([(ip.into(), true)]))
        .unwrap();
    let worker = std::thread::spawn(move || runtime.run());
    config.main_rtsp_url = Some("rtsp://127.0.0.1:9/edited".into());
    config::upsert_camera(&path, &config).unwrap();
    assert!(start_runtime_camera(&state, &config, true, false).is_some());
    assert!(publications.load(Ordering::SeqCst) >= 2);
    assert!(state.privacy.decision(ip, chrono::Utc::now()).unwrap().0);
    shutdown.cancel();
    worker.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
