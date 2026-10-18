use super::*;
use crate::cameras::RecordedAudioSettings;

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
    state: ServerState,
}

impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-audio-settings-{}", Uuid::new_v4()));
        let path = directory.join("config.toml");
        config::write_private_file(
            &path,
            b"[cameras.front]\nip='192.0.2.10'\nusername='operator'\npassword='{secret:TEST_PASSWORD}'\nprofile_identity='8f2eb655-f371-4fbd-b11a-0fb680e10ccf'\n[cameras.front.recorded_audio]\nmain_offset_ms=75\n",
        )
        .unwrap();
        config::write_private_file(
            &directory.join("secrets.toml"),
            b"TEST_PASSWORD='synthetic-private-password'\n",
        )
        .unwrap();
        let camera = config::load_cameras(&path).unwrap()["cameras"][0].clone();
        let state = ServerState::empty().with_camera_config_path(path.clone());
        state.upsert_camera(camera_entry(&camera, None));
        Self {
            directory,
            path,
            state,
        }
    }

    fn update(&self, settings: RecordedAudioSettings) -> CameraSettingsUpdate {
        CameraSettingsUpdate {
            expected_configuration_revision: camera_configuration_revision(&self.state).unwrap(),
            recorded_audio: Some(settings),
            ..Default::default()
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn audio_only_save_preserves_ingress_identity_and_updates_the_live_settings_snapshot() {
    let fixture = Fixture::new();
    let before = fixture.state.camera("192.0.2.10").unwrap();
    let settings = RecordedAudioSettings {
        default_offset_ms: Some(-100),
        main_offset_ms: Some(0),
        sub_offset_ms: None,
    };
    let (_router, router_tx) = crate::runtime::Router::new().unwrap();
    let result = save_camera_settings(
        fixture.update(settings),
        &router_tx,
        &fixture.state,
        "192.0.2.10",
    )
    .unwrap();
    assert!(!result.restart_required);
    assert_eq!(result.camera.recorded_audio, settings);
    let after = fixture.state.camera("192.0.2.10").unwrap();
    assert!(Arc::ptr_eq(
        &before.control_revision,
        &after.control_revision
    ));
    assert_eq!(before.recording_label, after.recording_label);
    assert_eq!(before.info.profiles.len(), after.info.profiles.len());
    assert_eq!(after.configuration.recorded_audio, settings);
    assert_eq!(
        config::load_cameras(&fixture.path).unwrap()["cameras"][0].recorded_audio,
        settings
    );
    let raw = std::fs::read_to_string(&fixture.path).unwrap();
    assert!(raw.contains("{secret:TEST_PASSWORD}"));
    assert!(raw.contains("8f2eb655-f371-4fbd-b11a-0fb680e10ccf"));
    assert!(!raw.contains("synthetic-private-password"));
    assert!(
        !serde_json::to_string(&result.camera)
            .unwrap()
            .contains("synthetic-private-password")
    );
}

#[test]
fn invalid_or_stale_audio_save_keeps_disk_and_runtime_unchanged() {
    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.path).unwrap();
    let original = fixture
        .state
        .camera("192.0.2.10")
        .unwrap()
        .configuration
        .recorded_audio;
    let invalid = RecordedAudioSettings {
        sub_offset_ms: Some(-5_001),
        ..Default::default()
    };
    let (_router, router_tx) = crate::runtime::Router::new().unwrap();
    let Err(error) = save_camera_settings(
        fixture.update(invalid),
        &router_tx,
        &fixture.state,
        "192.0.2.10",
    ) else {
        panic!("invalid audio settings must fail");
    };
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
    let mut stale = fixture.update(RecordedAudioSettings::default());
    stale.expected_configuration_revision = "stale".to_owned();
    let Err(error) = save_camera_settings(stale, &router_tx, &fixture.state, "192.0.2.10") else {
        panic!("stale audio settings must fail");
    };
    assert_eq!(error.code, proto::ErrorCode::Rejected);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
    assert_eq!(
        fixture
            .state
            .camera("192.0.2.10")
            .unwrap()
            .configuration
            .recorded_audio,
        original
    );
}

#[test]
fn audio_only_partial_reset_keeps_saved_and_live_inheritance_in_sync() {
    let fixture = Fixture::new();
    let previous_revision = camera_configuration_revision(&fixture.state).unwrap();
    let settings = RecordedAudioSettings {
        default_offset_ms: Some(125),
        main_offset_ms: None,
        sub_offset_ms: Some(0),
    };
    let (_router, router_tx) = crate::runtime::Router::new().unwrap();
    let result = save_camera_settings(
        fixture.update(settings),
        &router_tx,
        &fixture.state,
        "192.0.2.10",
    )
    .unwrap();
    assert!(!result.restart_required);
    assert_ne!(result.configuration_revision, previous_revision);
    assert_eq!(result.camera.recorded_audio, settings);
    assert_eq!(
        fixture
            .state
            .camera("192.0.2.10")
            .unwrap()
            .configuration
            .recorded_audio,
        settings
    );
    let saved = config::load_cameras(&fixture.path).unwrap()["cameras"][0].recorded_audio;
    assert_eq!(saved, settings);
    assert_eq!(saved.effective_offset_ms("main").unwrap(), 125);
    assert_eq!(saved.effective_offset_ms("sub").unwrap(), 0);
    assert_eq!(
        camera_configuration_revision(&fixture.state).unwrap(),
        result.configuration_revision
    );
}

#[test]
fn wire_update_preserves_absence_zero_and_empty_reset_and_rejects_out_of_range() {
    let absent =
        camera_settings_update_from_proto(proto::UpdateCameraConfiguration::default()).unwrap();
    assert!(absent.recorded_audio.is_none());
    let reset = camera_settings_update_from_proto(proto::UpdateCameraConfiguration {
        recorded_audio: Some(proto::RecordedAudioSettings::default()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(reset.recorded_audio, Some(RecordedAudioSettings::default()));
    let zero = camera_settings_update_from_proto(proto::UpdateCameraConfiguration {
        recorded_audio: Some(proto::RecordedAudioSettings {
            default_offset_ms: Some(125),
            main_offset_ms: Some(0),
            sub_offset_ms: None,
        }),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(zero.recorded_audio.unwrap().main_offset_ms, Some(0));
    let Err(error) = camera_settings_update_from_proto(proto::UpdateCameraConfiguration {
        recorded_audio: Some(proto::RecordedAudioSettings {
            sub_offset_ms: Some(5_001),
            ..Default::default()
        }),
        ..Default::default()
    }) else {
        panic!("invalid wire settings must fail");
    };
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
}

#[cfg(windows)]
#[test]
fn failed_atomic_replace_preserves_live_audio_and_saved_file() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.path).unwrap();
    let original = fixture
        .state
        .camera("192.0.2.10")
        .unwrap()
        .configuration
        .recorded_audio;
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&fixture.path)
        .unwrap();
    let (_router, router_tx) = crate::runtime::Router::new().unwrap();
    let result = save_camera_settings(
        fixture.update(RecordedAudioSettings::default()),
        &router_tx,
        &fixture.state,
        "192.0.2.10",
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
    assert_eq!(
        fixture
            .state
            .camera("192.0.2.10")
            .unwrap()
            .configuration
            .recorded_audio,
        original
    );
    drop(locked);
}
