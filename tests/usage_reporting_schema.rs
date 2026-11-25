use keeppeek::usage_reporting::{
    model::{
        Architecture, BasicSystemData, CURRENT_DISCLOSURE_REVISION, CameraCapabilityId,
        CameraCompatibilityData, CameraCompatibilityGroup, CpuModelFamily, CpuVendor,
        DeploymentType, EventSourceTypeId, FeatureAdoptionData, IntegrationId, MAX_CAMERA_GROUPS,
        MAX_PAYLOAD_BYTES, MemoryBucket, OsFamily, ProjectedRetentionBucket, RecordingModeCount,
        ReleaseChannel, ResolutionBucket, SCHEMA_VERSION_V1, SchemaValidationError,
        StorageCapacityBucket, TelemetryCameraBackend, TelemetryCameraTransport,
        TelemetryRecordingMode, UsageReportV1, VideoAcceleration, VideoCodec,
    },
    normalize::{
        ALLOWED_MANUFACTURERS, ALLOWED_MODELS, UNKNOWN_SLUG, is_allowed_manufacturer,
        is_allowed_model_for_manufacturer, normalize_architecture, normalize_camera_backend,
        normalize_camera_identity, normalize_camera_transport, normalize_cpu,
        normalize_memory_bucket, normalize_os_family, normalize_os_version,
        normalize_projected_retention_bucket, normalize_recording_mode, normalize_release_channel,
        normalize_resolution_bucket, normalize_storage_capacity_bucket,
        normalize_video_acceleration, normalize_video_codec,
    },
};
use serde_json::Value;

const SAMPLE_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

fn sample_basic() -> BasicSystemData {
    BasicSystemData {
        keeppeek_version: "0.1.0-alpha.1".to_owned(),
        release_channel: ReleaseChannel::Alpha,
        deployment_type: DeploymentType::Container,
        os_family: OsFamily::Linux,
        os_version: "24.04".to_owned(),
        architecture: Architecture::X86_64,
        cpu_vendor: CpuVendor::Intel,
        cpu_model_family: CpuModelFamily::IntelNSeries,
        physical_cores: Some(4),
        logical_cores: 4,
        memory_bucket: MemoryBucket::From8To16Gb,
        video_acceleration: vec![VideoAcceleration::Vaapi],
    }
}

fn sample_report(include_camera: bool, include_feature: bool) -> UsageReportV1 {
    UsageReportV1 {
        schema_version: SCHEMA_VERSION_V1,
        disclosure_revision: CURRENT_DISCLOSURE_REVISION,
        report_sequence: 1,
        installation_id: SAMPLE_UUID.to_owned(),
        basic: sample_basic(),
        camera_compatibility: include_camera.then(|| CameraCompatibilityData {
            configured_cameras: 3,
            connected_cameras: 3,
            decodable_cameras: 3,
            recording_cameras: 2,
            groups: vec![CameraCompatibilityGroup {
                manufacturer: "reolink".to_owned(),
                model: "reolink_rlc_810a".to_owned(),
                backend: TelemetryCameraBackend::ReoProto,
                transport: TelemetryCameraTransport::Tcp,
                video_codec: VideoCodec::H265,
                resolution_bucket: ResolutionBucket::Uhd4k,
                capabilities: vec![
                    CameraCapabilityId::Audio,
                    CameraCapabilityId::Events,
                    CameraCapabilityId::Recording,
                ],
                count: 2,
            }],
        }),
        feature_adoption: include_feature.then(|| FeatureAdoptionData {
            recording_modes: vec![
                RecordingModeCount {
                    mode: TelemetryRecordingMode::Continuous,
                    count: 2,
                },
                RecordingModeCount {
                    mode: TelemetryRecordingMode::EventOnly,
                    count: 1,
                },
            ],
            storage_capacity_bucket: StorageCapacityBucket::From1TbTo4Tb,
            projected_retention_bucket: ProjectedRetentionBucket::From7To30Days,
            enabled_integrations: vec![IntegrationId::MqttForwarder, IntegrationId::GuestSharing],
            event_source_types: vec![
                EventSourceTypeId::ReolinkEvents,
                EventSourceTypeId::OperationalEvents,
            ],
        }),
    }
}

#[test]
fn schema_file_matches_rust_allowlists_and_closed_object_contract() {
    let raw = std::fs::read_to_string("schemas/usage-reporting/v1.schema.json").unwrap();
    let schema: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(schema["additionalProperties"], Value::Bool(false));
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        Value::from(1)
    );
    assert_eq!(
        schema["properties"]["disclosure_revision"]["const"],
        Value::from(1)
    );

    let schema_brands: Vec<&str> = schema["$defs"]["cameraManufacturer"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap())
        .collect();
    assert_eq!(schema_brands, ALLOWED_MANUFACTURERS);

    let schema_models: Vec<&str> = schema["$defs"]["cameraModel"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap())
        .collect();
    let mut expected_models: Vec<&str> = ALLOWED_MODELS.iter().map(|(_, model)| *model).collect();
    expected_models.push(UNKNOWN_SLUG);
    assert_eq!(schema_models, expected_models);

    for def_name in [
        "basicSystemData",
        "cameraCompatibilityData",
        "cameraCompatibilityGroup",
        "featureAdoptionData",
    ] {
        assert_eq!(
            schema["$defs"][def_name]["additionalProperties"],
            Value::Bool(false),
            "definition {def_name} must forbid unknown fields"
        );
    }
}

#[test]
fn disabled_categories_are_omitted_rather_than_null_filled() {
    let basic_only = sample_report(false, false);
    let json = basic_only.to_bounded_json().unwrap();
    let parsed: Value = serde_json::from_str(&json).unwrap();
    let object = parsed.as_object().unwrap();
    assert!(object.contains_key("basic"));
    assert!(!object.contains_key("camera_compatibility"));
    assert!(!object.contains_key("feature_adoption"));

    let with_camera = sample_report(true, false);
    let camera_json = with_camera.to_bounded_json().unwrap();
    let camera_obj: Value = serde_json::from_str(&camera_json).unwrap();
    assert!(camera_obj.get("camera_compatibility").is_some());
    assert!(camera_obj.get("feature_adoption").is_none());

    let roundtrip = UsageReportV1::from_bounded_json(camera_json.as_bytes()).unwrap();
    assert_eq!(roundtrip, with_camera);
}

#[test]
fn unknown_fields_enums_and_unlisted_camera_values_are_rejected() {
    let valid = sample_report(true, true).to_bounded_json().unwrap();
    let mut with_extra: Value = serde_json::from_str(&valid).unwrap();
    with_extra["unexpected"] = Value::String("canary".to_owned());
    assert!(UsageReportV1::from_bounded_json(with_extra.to_string().as_bytes()).is_err());

    let mut with_firmware: Value = serde_json::from_str(&valid).unwrap();
    with_firmware["camera_compatibility"]["groups"][0]["firmware_version"] =
        Value::String("v3.1.0".to_owned());
    assert!(UsageReportV1::from_bounded_json(with_firmware.to_string().as_bytes()).is_err());

    let mut bad_brand: Value = serde_json::from_str(&valid).unwrap();
    bad_brand["camera_compatibility"]["groups"][0]["manufacturer"] =
        Value::String("private_custom_vendor".to_owned());
    assert_eq!(
        UsageReportV1::from_bounded_json(bad_brand.to_string().as_bytes()),
        Err(SchemaValidationError::InvalidCameraManufacturer)
    );

    let mut bad_model: Value = serde_json::from_str(&valid).unwrap();
    bad_model["camera_compatibility"]["groups"][0]["model"] =
        Value::String("front_porch_secret_cam".to_owned());
    assert_eq!(
        UsageReportV1::from_bounded_json(bad_model.to_string().as_bytes()),
        Err(SchemaValidationError::InvalidCameraModel)
    );

    let mut mismatched_model: Value = serde_json::from_str(&valid).unwrap();
    mismatched_model["camera_compatibility"]["groups"][0]["manufacturer"] =
        Value::String("hikvision".to_owned());
    assert_eq!(
        UsageReportV1::from_bounded_json(mismatched_model.to_string().as_bytes()),
        Err(SchemaValidationError::InvalidCameraModel)
    );
}

#[test]
fn payload_size_boundary_checks_65535_65536_and_65537_bytes() {
    let base = sample_report(true, true).to_bounded_json().unwrap();
    assert!(base.len() < MAX_PAYLOAD_BYTES);

    let pad = |target_len: usize| -> Vec<u8> {
        let mut bytes = base.as_bytes().to_vec();
        assert!(bytes.len() <= target_len);
        // Trailing ASCII spaces remain valid JSON and let us test the exact byte boundary.
        bytes.resize(target_len, b' ');
        bytes
    };

    assert!(UsageReportV1::from_bounded_json(&pad(MAX_PAYLOAD_BYTES - 1)).is_ok());
    assert!(UsageReportV1::from_bounded_json(&pad(MAX_PAYLOAD_BYTES)).is_ok());
    assert_eq!(
        UsageReportV1::from_bounded_json(&pad(MAX_PAYLOAD_BYTES + 1)),
        Err(SchemaValidationError::PayloadTooLarge {
            bytes: MAX_PAYLOAD_BYTES + 1,
            max_bytes: MAX_PAYLOAD_BYTES,
        })
    );
}

#[test]
fn camera_group_limit_enforces_256_max() {
    let mut report = sample_report(true, false);
    let groups = &mut report.camera_compatibility.as_mut().unwrap().groups;
    groups.clear();
    for index in 0..MAX_CAMERA_GROUPS {
        groups.push(CameraCompatibilityGroup {
            manufacturer: "reolink".to_owned(),
            model: "reolink_rlc_810a".to_owned(),
            backend: TelemetryCameraBackend::ReoProto,
            transport: TelemetryCameraTransport::Tcp,
            video_codec: VideoCodec::H265,
            resolution_bucket: ResolutionBucket::Uhd4k,
            capabilities: vec![CameraCapabilityId::Recording],
            count: (index as u32) + 1,
        });
    }
    let json = report.to_bounded_json().unwrap();
    assert!(json.len() <= MAX_PAYLOAD_BYTES);

    report
        .camera_compatibility
        .as_mut()
        .unwrap()
        .groups
        .push(CameraCompatibilityGroup {
            manufacturer: "reolink".to_owned(),
            model: "reolink_rlc_810a".to_owned(),
            backend: TelemetryCameraBackend::ReoProto,
            transport: TelemetryCameraTransport::Tcp,
            video_codec: VideoCodec::H265,
            resolution_bucket: ResolutionBucket::Uhd4k,
            capabilities: vec![CameraCapabilityId::Recording],
            count: 1,
        });
    assert_eq!(
        report.validate(),
        Err(SchemaValidationError::TooManyCameraGroups)
    );
}

#[test]
fn normalization_maps_known_values_and_collapses_arbitrary_strings_to_finite_outputs() {
    assert_eq!(
        normalize_camera_identity(None, Some("Reolink"), Some("RLC-810A")),
        ("reolink", "reolink_rlc_810a")
    );
    assert_eq!(
        normalize_camera_identity(None, Some("TP-Link"), Some("Tapo C210")),
        ("tapo", "tapo_c210")
    );
    assert_eq!(
        normalize_camera_identity(None, Some("Foscam"), Some("C100")),
        ("foscam", "unknown")
    );
    assert_eq!(
        normalize_cpu(Some("Intel(R) Core(TM) i7-13700K CPU @ 3.40GHz")),
        (CpuVendor::Intel, CpuModelFamily::IntelCoreI)
    );
    assert_eq!(
        normalize_cpu(Some("Intel(R) N100")),
        (CpuVendor::Intel, CpuModelFamily::IntelNSeries)
    );
    assert_eq!(
        normalize_cpu(Some("Apple M3 Max")),
        (CpuVendor::Apple, CpuModelFamily::AppleM3)
    );
    assert_eq!(
        normalize_os_version(Some("Ubuntu 24.04.2 LTS (Noble) host-secret")),
        "24.04"
    );

    // Deterministic property sweep over adversarial strings proves finite allowlisted outputs.
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let alphabet =
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_./:@#?=& \t\n\"'\\<>{}[]";
    for iteration in 0..512 {
        let len = (iteration % 97) + 1;
        let mut s = String::with_capacity(len);
        for _ in 0..len {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let idx = ((state >> 32) as usize) % alphabet.len();
            s.push(alphabet[idx] as char);
        }
        let (brand, model) = normalize_camera_identity(None, Some(&s), Some(&s));
        assert!(is_allowed_manufacturer(brand));
        assert!(is_allowed_model_for_manufacturer(brand, model));

        let os_ver = normalize_os_version(Some(&s));
        assert!(os_ver.len() <= 16);
        let _ = normalize_cpu(Some(&s));
        let _ = normalize_os_family(&s);
        let _ = normalize_architecture(&s);
        let _ = normalize_release_channel(&s);
        let _ = normalize_video_acceleration(&s);
        let _ = normalize_camera_backend(&s);
        let _ = normalize_camera_transport(&s);
        let _ = normalize_video_codec(Some(&s));
        let _ = normalize_resolution_bucket(Some(&s));
        let _ = normalize_recording_mode(&s);
        let _ = normalize_memory_bucket(state);
        let _ = normalize_storage_capacity_bucket(state);
        let _ = normalize_projected_retention_bucket(Some((iteration as f64) - 5.0));
    }
}
