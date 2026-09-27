use keeppeek::{
    cameras::{CameraConfig, CameraRecordingMode, EventRecordingStream},
    config::load_cameras,
};

#[test]
fn every_legacy_recording_mode_upgrades_with_preroll_disabled() {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-preroll-upgrade-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("config.toml");
    for (name, mode) in [
        ("off", CameraRecordingMode::Off),
        ("sub", CameraRecordingMode::Sub),
        ("main", CameraRecordingMode::Main),
        ("both", CameraRecordingMode::Both),
        ("event-boost", CameraRecordingMode::EventBoost),
    ] {
        let before = format!(
            "ip = '192.0.2.10'\nrecording_mode = '{name}'\nevent_recording_duration_secs = 45\n"
        );
        let old_fields: toml::Table = toml::from_str(&before).unwrap();
        assert!(!old_fields.contains_key("event_pre_recording_duration_secs"));
        assert!(!old_fields.contains_key("event_recording_stream"));
        std::fs::write(&path, format!("[cameras.upgrade]\n{before}")).unwrap();
        let loaded = load_cameras(&path).unwrap();
        let camera = &loaded["cameras"][0];
        assert_eq!(camera.recording_mode, mode);
        assert_eq!(camera.event_recording_duration_secs, 45);
        assert_eq!(camera.event_pre_recording_duration_secs, 0);
        assert_eq!(camera.event_recording_stream, EventRecordingStream::Main);
        let serialized = toml::to_string(camera).unwrap();
        let new_fields: toml::Table = toml::from_str(&serialized).unwrap();
        assert_eq!(new_fields["recording_mode"], old_fields["recording_mode"]);
        assert_eq!(
            new_fields["event_pre_recording_duration_secs"].as_integer(),
            Some(0)
        );
        assert_eq!(new_fields["event_recording_stream"].as_str(), Some("main"));
        let roundtrip: CameraConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(roundtrip.recording_mode, mode);
        std::fs::write(&path, format!("[cameras.upgrade]\n{serialized}")).unwrap();
        let reloaded = load_cameras(&path).unwrap();
        let camera = &reloaded["cameras"][0];
        assert_eq!(camera.recording_mode, mode);
        assert_eq!(camera.event_recording_duration_secs, 45);
        assert_eq!(camera.event_pre_recording_duration_secs, 0);
        assert_eq!(camera.event_recording_stream, EventRecordingStream::Main);
    }
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
