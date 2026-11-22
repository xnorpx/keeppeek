use super::*;
use crate::cameras::RecordedAudioSettings;

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn new(audio: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-recorded-audio-{}", uuid::Uuid::new_v4()));
        let path = directory.join("config.toml");
        let text = format!(
            r#"
custom_keep = "unrelated"
[camera_defaults]
username = "{{secret:CAMERA_USERNAME}}"
password = "{{secret:CAMERA_PASSWORD}}"
[cameras.front]
ip = "192.0.2.10"
profile_identity = "8f2eb655-f371-4fbd-b11a-0fb680e10ccf"
custom_camera_key = "preserved"
{audio}
"#
        );
        write_private_file(&path, text.as_bytes()).unwrap();
        write_private_file(
            &secrets_path(&path),
            b"CAMERA_USERNAME='operator'\nCAMERA_PASSWORD='synthetic-secret'\n",
        )
        .unwrap();
        Self { directory, path }
    }

    fn camera(&self) -> CameraConfig {
        load_cameras(&self.path)
            .unwrap()
            .remove("cameras")
            .unwrap()
            .remove(0)
    }

    fn raw(&self) -> toml::Table {
        toml::from_str(&std::fs::read_to_string(&self.path).unwrap()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn loader_resolves_only_camera_and_stream_inheritance_and_keeps_explicit_zero() {
    let empty = Fixture::new("");
    assert_eq!(
        empty.camera().recorded_audio,
        RecordedAudioSettings::default()
    );
    let fixture =
        Fixture::new("[cameras.front.recorded_audio]\ndefault_offset_ms=-275\nmain_offset_ms=0");
    let camera = fixture.camera();
    assert_eq!(
        camera.recorded_audio.effective_offset_ms("main").unwrap(),
        0
    );
    assert_eq!(
        camera.recorded_audio.effective_offset_ms("sub").unwrap(),
        -275
    );
    upsert_camera(&fixture.path, &camera).unwrap();
    assert_eq!(fixture.camera().recorded_audio, camera.recorded_audio);
    assert_eq!(
        fixture.raw()["cameras"]["front"]["recorded_audio"]["main_offset_ms"].as_integer(),
        Some(0)
    );
}

#[test]
fn loader_rejects_invalid_offsets_and_unknown_nested_keys() {
    for invalid in [
        "default_offset_ms=-5001",
        "main_offset_ms=5001",
        "sub_offset_ms=-2147483648",
        "main_offset_ms=0.25",
        "main_offset_ms='100'",
        "offset_ms=100",
    ] {
        let fixture = Fixture::new(&format!("[cameras.front.recorded_audio]\n{invalid}"));
        assert!(load_cameras(&fixture.path).is_err(), "{invalid}");
        assert!(
            validated_configuration_table(&fixture.path, &fixture.raw()).is_err(),
            "{invalid}"
        );
    }
    let fixture =
        Fixture::new("[cameras.front.recorded_audio]\nmain_offset_ms=-5000\nsub_offset_ms=5000");
    assert!(validated_configuration_table(&fixture.path, &fixture.raw()).is_ok());
}

#[test]
fn writer_preserves_saved_audio_secrets_identity_and_unrelated_fields() {
    let fixture =
        Fixture::new("[cameras.front.recorded_audio]\ndefault_offset_ms=100\nsub_offset_ms=0");
    let identity = load_config(&fixture.path)
        .unwrap()
        .camera_profile_identities()
        .unwrap();
    let secrets_before = std::fs::read(secrets_path(&fixture.path)).unwrap();
    let mut camera = fixture.camera();
    camera.display_name = Some("New label".to_owned());
    upsert_camera(&fixture.path, &camera).unwrap();
    let raw = fixture.raw();
    assert_eq!(raw["custom_keep"].as_str(), Some("unrelated"));
    assert_eq!(
        raw["cameras"]["front"]["custom_camera_key"].as_str(),
        Some("preserved")
    );
    assert_eq!(
        raw["camera_defaults"]["password"].as_str(),
        Some("{secret:CAMERA_PASSWORD}")
    );
    assert!(
        !std::fs::read_to_string(&fixture.path)
            .unwrap()
            .contains("synthetic-secret")
    );
    assert_eq!(fixture.camera().recorded_audio, camera.recorded_audio);
    assert_eq!(
        std::fs::read(secrets_path(&fixture.path)).unwrap(),
        secrets_before
    );
    assert_eq!(
        load_config(&fixture.path)
            .unwrap()
            .camera_profile_identities()
            .unwrap(),
        identity
    );
}

#[test]
fn replacing_with_empty_settings_removes_saved_overrides_and_restores_zero() {
    let fixture = Fixture::new(
        "[cameras.front.recorded_audio]\ndefault_offset_ms=500\nmain_offset_ms=0\nsub_offset_ms=-200",
    );
    let mut camera = fixture.camera();
    camera.recorded_audio = RecordedAudioSettings::default();
    upsert_camera(&fixture.path, &camera).unwrap();
    assert!(
        !fixture.raw()["cameras"]["front"]
            .as_table()
            .unwrap()
            .contains_key("recorded_audio")
    );
    assert_eq!(
        fixture
            .camera()
            .recorded_audio
            .effective_offset_ms("main")
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .camera()
            .recorded_audio
            .effective_offset_ms("sub")
            .unwrap(),
        0
    );
}

#[test]
fn replacing_nonempty_settings_clears_only_the_omitted_stream_override() {
    let fixture = Fixture::new(
        "[cameras.front.recorded_audio]\ndefault_offset_ms=100\nmain_offset_ms=75\nsub_offset_ms=0",
    );
    let mut camera = fixture.camera();
    camera.recorded_audio.default_offset_ms = Some(125);
    camera.recorded_audio.main_offset_ms = None;
    upsert_camera(&fixture.path, &camera).unwrap();
    let restored = fixture.camera().recorded_audio;
    assert_eq!(restored, camera.recorded_audio);
    assert_eq!(restored.effective_offset_ms("main").unwrap(), 125);
    assert_eq!(restored.effective_offset_ms("sub").unwrap(), 0);
    let raw = fixture.raw();
    let saved = raw["cameras"]["front"]["recorded_audio"]
        .as_table()
        .unwrap();
    assert!(!saved.contains_key("main_offset_ms"));
    assert_eq!(saved["default_offset_ms"].as_integer(), Some(125));
    assert_eq!(saved["sub_offset_ms"].as_integer(), Some(0));
}

#[test]
fn invalid_writer_candidate_leaves_file_and_reload_unchanged() {
    let fixture = Fixture::new("[cameras.front.recorded_audio]\nmain_offset_ms=50");
    let before = std::fs::read(&fixture.path).unwrap();
    let original = fixture.camera().recorded_audio;
    let mut camera = fixture.camera();
    camera.recorded_audio.sub_offset_ms = Some(5_001);
    assert!(upsert_camera(&fixture.path, &camera).is_err());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
    assert_eq!(fixture.camera().recorded_audio, original);
}
