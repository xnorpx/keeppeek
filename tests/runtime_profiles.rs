//! Verifies persistent runtime-profile field boundaries, backup preservation, and activation isolation.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use keeppeek::backup::{
    BackupSecretPolicy, BackupSection, CreateBundleOptions, create_bundle, inspect_bundle,
};
use keeppeek::cameras::CameraRecordingMode;
use keeppeek::config::{load_config, secrets_path};
use keeppeek::profiles::{
    BaseCameraPolicy, CameraProfileOverride, FieldSource, MAX_PROFILE_TARGETS, MAX_PROFILES,
    ProfileRegistry, RuntimeProfile, resolve_camera_policy,
};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-output")
            .join(format!("runtime-profiles-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("test directory must be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn runtime_safe_field_matrix_accepts_eligible_domains_and_rejects_prohibited_classes() {
    let eligible_modes = [
        ("off", CameraRecordingMode::Off),
        ("sub", CameraRecordingMode::Sub),
        ("main", CameraRecordingMode::Main),
        ("both", CameraRecordingMode::Both),
        ("event-boost", CameraRecordingMode::EventBoost),
        ("event-only", CameraRecordingMode::EventOnly),
    ];
    let base = BaseCameraPolicy {
        available: true,
        recording_mode: CameraRecordingMode::Sub,
        privacy_enforced: false,
        notifications_enabled: true,
    };

    for (wire_mode, expected_mode) in eligible_modes {
        let document = format!(r#"{{"recording_mode":"{wire_mode}"}}"#);
        let parsed: CameraProfileOverride = serde_json::from_str(&document)
            .unwrap_or_else(|error| panic!("eligible mode {wire_mode} was rejected: {error}"));
        assert_eq!(parsed.recording_mode, Some(expected_mode));
        let effective = resolve_camera_policy(&base, Some(&parsed));
        assert_eq!(effective.recording_mode, expected_mode);
        assert_eq!(effective.recording_mode_source, FieldSource::Profile);
        assert_eq!(effective.available_source, FieldSource::Base);
        assert_eq!(effective.privacy_source, FieldSource::Base);
        assert_eq!(effective.notifications_source, FieldSource::Base);
    }

    // Reject every reconnect-required, security, detector, storage, and metadata class.
    let prohibited_documents = [
        r#"{"ip":"192.0.2.10"}"#,
        r#"{"host":"192.0.2.10"}"#,
        r#"{"http_port":80}"#,
        r#"{"onvif_port":8000}"#,
        r#"{"username":"operator"}"#,
        r#"{"password":"{secret:CAMERA_PASSWORD}"}"#,
        r#"{"main_rtsp_url":"rtsp://192.0.2.10:554/main"}"#,
        r#"{"sub_rtsp_url":"rtsp://192.0.2.10:554/sub"}"#,
        r#"{"codec":"h265"}"#,
        r#"{"backend":"retina"}"#,
        r#"{"transport":"udp"}"#,
        r#"{"storage_path":"/recordings"}"#,
        r#"{"long_term_path":"/archive"}"#,
        r#"{"retention_days":30}"#,
        r#"{"detector":"yolov8"}"#,
        r#"{"model":"person-v2"}"#,
        r#"{"zones":[]}"#,
        r#"{"masks":[]}"#,
        r#"{"access_control":"admin"}"#,
        r#"{"ptz_preset":1}"#,
        r#"{"privacy_schedule":[]}"#,
        r#"{"notification_rules":[]}"#,
        r#"{"uid":"reolink-uid"}"#,
        r#"{"display_name":"Front"}"#,
        r#"{"metadata":{"note":"arbitrary"}}"#,
    ];
    for document in prohibited_documents {
        assert!(
            serde_json::from_str::<CameraProfileOverride>(document).is_err(),
            "prohibited field was accepted in CameraProfileOverride: {document}"
        );
    }
}

#[test]
fn registry_rejects_active_deletion_empty_overrides_and_invalid_bounds() {
    let mut registry = ProfileRegistry {
        revision: 2,
        active_profile_id: Some("away".to_owned()),
        profiles: BTreeMap::from([(
            "away".to_owned(),
            RuntimeProfile {
                id: "away".to_owned(),
                name: "Away".to_owned(),
                revision: 1,
                cameras: BTreeMap::from([(
                    "00000000-0000-4000-8000-000000000001".to_owned(),
                    CameraProfileOverride {
                        availability: Some(false),
                        ..Default::default()
                    },
                )]),
            },
        )]),
    };

    assert!(registry.validate().is_ok());
    assert!(registry.validate_delete("away").is_err());

    registry.active_profile_id = None;
    assert!(registry.validate_delete("away").is_ok());
    assert!(registry.validate_delete("missing").is_err());

    let mut invalid_empty = registry;
    invalid_empty
        .profiles
        .get_mut("away")
        .unwrap()
        .cameras
        .insert(
            "00000000-0000-4000-8000-000000000002".to_owned(),
            CameraProfileOverride::default(),
        );
    assert!(invalid_empty.validate().is_err());

    for invalid_id in ["", ".", "..", "bad/id", "has space", &"a".repeat(129)] {
        let mut candidate = ProfileRegistry {
            revision: 1,
            active_profile_id: None,
            profiles: BTreeMap::from([(
                invalid_id.to_owned(),
                RuntimeProfile {
                    id: invalid_id.to_owned(),
                    name: "Valid".to_owned(),
                    revision: 1,
                    cameras: BTreeMap::new(),
                },
            )]),
        };
        assert!(
            candidate.validate().is_err(),
            "invalid profile ID must fail validation: {invalid_id:?}"
        );
        candidate.profiles.clear();
        candidate.active_profile_id = Some(invalid_id.to_owned());
        assert!(candidate.validate().is_err());
    }

    assert_eq!(MAX_PROFILES, 32);
    assert_eq!(MAX_PROFILE_TARGETS, 127);
}

fn write_migrated_profile_fixture(dir: &Path, camera_id: &str) -> PathBuf {
    let config_path = dir.join("config.toml");
    let raw_config = format!(
        r#"host = "127.0.0.1"
port = 8081

[cameras.front]
ip = "192.0.2.10"
profile_identity = "{camera_id}"
username = "{{secret:CAMERA_USER}}"
password = "{{secret:CAMERA_PASSWORD}}"

[runtime_profiles]
identity_schema = 1
revision = 3
active_profile_id = "away"

[runtime_profiles.profiles.away]
id = "away"
name = "Away"
revision = 2

[runtime_profiles.profiles.away.cameras."{camera_id}"]
availability = false
recording_mode = "event-only"
privacy_enforced = true
notifications_suppressed = true
"#
    );
    std::fs::write(&config_path, &raw_config).unwrap();
    std::fs::write(
        secrets_path(&config_path),
        "CAMERA_USER = 'synthetic-user'\nCAMERA_PASSWORD = 'synthetic-password'\n",
    )
    .unwrap();
    load_config(&config_path).expect("seeded migrated configuration must load");
    config_path
}

fn assert_restored_profile_table(archived_config: &str, camera_id: &str) {
    assert!(!archived_config.contains("synthetic-password"));
    assert!(archived_config.contains("{secret:CAMERA_PASSWORD}"));
    let parsed: toml::Table = toml::from_str(archived_config).unwrap();
    assert_eq!(
        parsed["cameras"]["front"]["profile_identity"].as_str(),
        Some(camera_id)
    );
    assert_eq!(
        parsed["runtime_profiles"]["identity_schema"].as_integer(),
        Some(1)
    );
    assert_eq!(
        parsed["runtime_profiles"]["active_profile_id"].as_str(),
        Some("away")
    );
    assert_eq!(parsed["runtime_profiles"]["revision"].as_integer(), Some(3));
    let override_table = &parsed["runtime_profiles"]["profiles"]["away"]["cameras"][camera_id];
    assert_eq!(override_table["availability"].as_bool(), Some(false));
    assert_eq!(
        override_table["recording_mode"].as_str(),
        Some("event-only")
    );
    assert_eq!(override_table["privacy_enforced"].as_bool(), Some(true));
    assert_eq!(
        override_table["notifications_suppressed"].as_bool(),
        Some(true)
    );
}

#[test]
fn configuration_backup_preserves_runtime_profiles_and_stays_config_only() {
    let dir = TempDir::new("backup");
    let camera_id = "00000000-0000-4000-8000-000000000042";
    let config_path = write_migrated_profile_fixture(dir.path(), camera_id);

    let (cursor, created_manifest) = create_bundle(
        Cursor::new(Vec::new()),
        CreateBundleOptions {
            config_path: &config_path,
            sections: &[],
            created_at_unix_ms: 1_788_000_000_000,
        },
    )
    .unwrap();
    let archive_bytes = cursor.into_inner();
    let inspected = inspect_bundle(Cursor::new(archive_bytes.as_slice())).unwrap();
    assert_eq!(created_manifest.sections.len(), 1);
    assert_eq!(inspected.sections().len(), 1);
    assert_eq!(inspected.sections()[0].kind(), BackupSection::RuntimeConfig);
    assert_eq!(inspected.secret_policy(), BackupSecretPolicy::Included);

    let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes)).unwrap();
    let mut archived_config = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name(inspected.sections()[0].path()).unwrap(),
        &mut archived_config,
    )
    .unwrap();
    assert_restored_profile_table(&archived_config, camera_id);

    let restored_dir = TempDir::new("restored");
    let restored_config_path = restored_dir.path().join("config.toml");
    std::fs::write(&restored_config_path, &archived_config).unwrap();
    std::fs::write(
        secrets_path(&restored_config_path),
        "CAMERA_USER = 'synthetic-user'\nCAMERA_PASSWORD = 'synthetic-password'\n",
    )
    .unwrap();
    load_config(&restored_config_path).expect("restored configuration must load");
}

#[test]
fn unknown_identity_schema_fails_closed_without_mutating_configuration() {
    let dir = TempDir::new("unknown-schema");
    let config_path = dir.path().join("config.toml");
    let original = b"[runtime_profiles]\nidentity_schema = 99\nrevision = 1\n[runtime_profiles.profiles]\n[cameras.front]\nip = '192.0.2.10'\n";
    std::fs::write(&config_path, original).unwrap();

    assert!(load_config(&config_path).is_err());
    assert_eq!(std::fs::read(&config_path).unwrap(), original);
}

fn collect_rust_sources(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = roots.to_vec();
    while let Some(path) = stack.pop() {
        if path.is_file() {
            if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
            continue;
        }
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry_path = entry.unwrap().path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else if entry_path.extension().is_some_and(|ext| ext == "rs") {
                files.push(entry_path);
            }
        }
    }
    files
}

fn extract_http_router_block(server_source: &str) -> &str {
    let start = server_source
        .find("router!(request,")
        .expect("HTTP router macro must exist in src/server.rs");
    let slice = &server_source[start..];
    let mut depth = 0usize;
    for (offset, ch) in slice.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &slice[..=offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated router!(request, ...) macro in src/server.rs");
}

#[test]
fn runtime_profile_activation_is_absent_from_non_webrtc_surfaces() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let non_activation_roots = [
        manifest_dir.join("src/event_forwarder"),
        manifest_dir.join("src/server/mqtt_integration.rs"),
        manifest_dir.join("src/notifications"),
        manifest_dir.join("src/bin"),
    ];
    let forbidden_tokens = [
        "service/runtime-profiles/",
        "keeppeek.runtime-profiles.",
        "runtime_profile_activated",
        "submit_runtime_profiles",
        "runtime-profiles",
    ];
    for file in collect_rust_sources(&non_activation_roots) {
        let source = std::fs::read_to_string(&file).unwrap();
        for token in forbidden_tokens {
            assert!(
                !source.contains(token),
                "unexpected runtime-profile activation token {token:?} in {}",
                file.display()
            );
        }
    }

    // Confirm HTTP routing and top-level server code expose no non-WebRTC profile dispatch.
    let server_source = std::fs::read_to_string(manifest_dir.join("src/server.rs")).unwrap();
    let http_router = extract_http_router_block(&server_source);
    for forbidden in [
        "profile",
        "runtime-profiles",
        "runtime_profiles",
        "service/runtime-profiles/",
        "keeppeek.runtime-profiles.",
    ] {
        assert!(
            !http_router.to_ascii_lowercase().contains(forbidden),
            "HTTP router must not reference runtime profiles ({forbidden:?})"
        );
    }
    assert!(!server_source.contains("service/runtime-profiles/"));
    for line in server_source.lines() {
        if line.contains("runtime-profiles") {
            assert!(
                line.contains("\"keeppeek.runtime-profiles.v1\""),
                "src/server.rs may reference runtime-profiles only as capability ID: {line}"
            );
        }
    }
}
