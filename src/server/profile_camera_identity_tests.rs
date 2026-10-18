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
