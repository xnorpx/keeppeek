use keeppeek::{
    api::{AudioProfileSummary, CameraInfo, ProfileSummary},
    cameras::{CameraAudioAsset, CameraCapabilities, CameraPorts},
    config::{self, Config},
    usage_reporting::{
        model::{
            CURRENT_DISCLOSURE_REVISION, CameraCapabilityId, MAX_CAMERA_GROUPS,
            SchemaValidationError, TelemetryCameraBackend, TelemetryCameraTransport, VideoCodec,
        },
        projection::{ConsentSelection, ProjectionHealthInput, ProjectionSnapshot, project_report},
    },
};
use uuid::Uuid;

const INSTALL_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

fn canary_health() -> ProjectionHealthInput {
    ProjectionHealthInput {
        version: "0.1.0-alpha.1".to_owned(),
        os_name: Some("Ubuntu Linux CANARY_OS_HOST".to_owned()),
        os_version: Some("24.04.2 LTS CANARY_OS_BUILD".to_owned()),
        architecture: "x86_64".to_owned(),
        cpu_brand: Some("Intel(R) Core(TM) i7-13700K CANARY_CPU_SERIAL".to_owned()),
        physical_cores: Some(4),
        logical_cores: 8,
        memory_total_bytes: 16 * 1024 * 1024 * 1024,
        configured_cameras: 3,
        connected_cameras: 3,
        decodable_cameras: 3,
        recording_cameras: 2,
        long_term_max_bytes: 2 * 1024 * 1024 * 1024 * 1024,
    }
}

fn canary_cameras() -> Vec<CameraInfo> {
    vec![
        CameraInfo {
            id: "CANARY_CAM_ID_1".to_owned(),
            ip: "192.168.77.88".to_owned(),
            name: Some("CANARY_CAM_NAME_FRONT_DOOR".to_owned()),
            manufacturer: Some("Reolink".to_owned()),
            model: Some("RLC-810A".to_owned()),
            firmware_version: Some("v3.1.0.9999_CANARY_FW".to_owned()),
            serial_number: Some("CANARY_SERIAL_987654".to_owned()),
            hardware_id: Some("CANARY_HWID_IPC523128M8MP".to_owned()),
            hostname: Some("front-door.CANARY_DOMAIN.local".to_owned()),
            mac_address: Some("AA:BB:CC:DD:EE:FF".to_owned()),
            is_reolink: true,
            backend: "reo-proto".to_owned(),
            transport: "tcp".to_owned(),
            web_url: "https://user:CANARY_PASS@192.168.77.88:8443/".to_owned(),
            ports: CameraPorts {
                http: Some(8080),
                https: Some(8443),
                rtsp: Some(8554),
                onvif: Some(8000),
            },
            capabilities: CameraCapabilities {
                ptz: false,
                audio: true,
                events: true,
                recording: true,
                analytics: true,
                imaging: false,
                two_way_audio: false,
                quick_replies: vec![CameraAudioAsset {
                    asset_id: "CANARY_ASSET_ID".to_owned(),
                    display_name: "CANARY_ASSET_LABEL".to_owned(),
                }],
                chimes: Vec::new(),
            },
            profiles: vec![ProfileSummary {
                name: "CANARY_PROFILE_NAME".to_owned(),
                stream: "main".to_owned(),
                encoding: Some("H265".to_owned()),
                resolution: Some("3840x2160".to_owned()),
                framerate: Some(25.0),
                bitrate_kbps: Some(6144),
                gop: Some(50),
                h264_profile: None,
                audio: Some(AudioProfileSummary {
                    encoding: "AAC".to_owned(),
                    sample_rate: Some(16_000),
                    bitrate_kbps: Some(64),
                }),
            }],
        },
        CameraInfo {
            id: "CANARY_CAM_ID_2".to_owned(),
            ip: "10.99.88.77".to_owned(),
            name: Some("CANARY_CAM_NAME_CUSTOM".to_owned()),
            manufacturer: Some("CANARY_PRIVATE_VENDOR_XYZ".to_owned()),
            model: Some("CANARY_PRIVATE_MODEL_9000".to_owned()),
            firmware_version: Some("CANARY_FW_PRIVATE".to_owned()),
            serial_number: Some("CANARY_SERIAL_2".to_owned()),
            hardware_id: Some("CANARY_HWID_2".to_owned()),
            hostname: Some("garage.CANARY_DOMAIN.local".to_owned()),
            mac_address: Some("11:22:33:44:55:66".to_owned()),
            is_reolink: false,
            backend: "retina".to_owned(),
            transport: "udp".to_owned(),
            web_url: "http://10.99.88.77/".to_owned(),
            ports: CameraPorts::default(),
            capabilities: CameraCapabilities {
                events: true,
                recording: true,
                ..CameraCapabilities::default()
            },
            profiles: vec![ProfileSummary {
                name: "main".to_owned(),
                stream: "main".to_owned(),
                encoding: Some("H264".to_owned()),
                resolution: Some("1920x1080".to_owned()),
                framerate: Some(15.0),
                bitrate_kbps: Some(2048),
                gop: Some(30),
                h264_profile: None,
                audio: None,
            }],
        },
    ]
}

#[test]
fn projection_strips_every_private_canary_and_normalizes_unknown_cameras() {
    let dir = std::env::temp_dir().join(format!("keeppeek-proj-canary-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg_path = dir.join("config.toml");
    std::fs::write(
        &cfg_path,
        r#"
host = "192.168.99.10"
port = 8081

[direct_card]
allowed_origins = ["https://CANARY_HA_ORIGIN.example"]

[event_forwarder.mqtt]
enabled = true
broker_url = "mqtts://CANARY_BROKER_HOST:8883"
topic_prefix = "CANARY_TOPIC_PREFIX/keeppeek"
username = "CANARY_MQTT_USER"

[storage]
medium_term_path = "/var/lib/CANARY_MEDIUM_PATH"
long_term_path = "/var/lib/CANARY_LONG_PATH"
"#,
    )
    .unwrap();
    let config = config::load_config(&cfg_path).unwrap();

    let health = canary_health();
    let cameras = canary_cameras();
    let snapshot = ProjectionSnapshot {
        config: &config,
        health: &health,
        cameras: &cameras,
        camera_database: None,
        estimated_retention_days: Some(14.5),
        is_container: true,
        video_accelerations: &["vaapi", "CANARY_PRIVATE_ACCEL"],
        has_notifications_pushover: true,
        has_notifications_webhook: false,
        has_external_api_events: true,
    };

    let report = project_report(
        &snapshot,
        ConsentSelection {
            basic: true,
            camera_compatibility: true,
            feature_adoption: true,
            consented_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
        },
        INSTALL_ID,
        1,
    )
    .unwrap();
    let json = report.to_bounded_json().unwrap();
    let debug = format!("{report:?}");

    let canaries = [
        "CANARY_",
        "192.168.77.88",
        "10.99.88.77",
        "192.168.99.10",
        "AA:BB:CC:DD:EE:FF",
        "11:22:33:44:55:66",
        "v3.1.0.9999",
        "firmware",
        "serial_number",
        "hardware_id",
        "8443",
        "8554",
        "8883",
    ];
    for canary in canaries {
        assert!(
            !json.contains(canary),
            "serialized payload leaked canary `{canary}`: {json}"
        );
        assert!(
            !debug.contains(canary),
            "debug output leaked canary `{canary}`"
        );
    }

    let camera_data = report.camera_compatibility.unwrap();
    assert_eq!(camera_data.groups.len(), 2);
    assert!(camera_data.groups.iter().any(|g| {
        g.manufacturer == "reolink"
            && g.model == "reolink_rlc_810a"
            && g.backend == TelemetryCameraBackend::ReoProto
            && g.transport == TelemetryCameraTransport::Tcp
            && g.video_codec == VideoCodec::H265
            && g.capabilities.contains(&CameraCapabilityId::QuickReplies)
            && g.count == 1
    }));
    assert!(camera_data.groups.iter().any(|g| {
        g.manufacturer == "unknown"
            && g.model == "unknown"
            && g.backend == TelemetryCameraBackend::Retina
            && g.transport == TelemetryCameraTransport::Udp
            && g.video_codec == VideoCodec::H264
            && g.count == 1
    }));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn projection_enforces_256_group_bound() {
    let config = Config::default();
    let health = canary_health();
    let mut cameras = Vec::new();
    let codecs = ["H264", "H265", "MJPEG", "VP8", "VP9", "AV1"];
    let transports = ["tcp", "udp"];
    let resolutions = ["640x480", "1280x720", "1920x1080", "2560x1440", "3840x2160"];
    let backends = ["retina", "reo-proto", "auto"];
    'outer: for codec in codecs {
        for transport in transports {
            for resolution in resolutions {
                for backend in backends {
                    for ptz in [false, true] {
                        for audio in [false, true] {
                            cameras.push(CameraInfo {
                                id: format!("cam-{}", cameras.len()),
                                ip: "192.0.2.1".to_owned(),
                                name: None,
                                manufacturer: Some("Reolink".to_owned()),
                                model: Some("RLC-810A".to_owned()),
                                firmware_version: None,
                                serial_number: None,
                                hardware_id: None,
                                hostname: None,
                                mac_address: None,
                                is_reolink: true,
                                backend: backend.to_owned(),
                                transport: transport.to_owned(),
                                web_url: String::new(),
                                ports: CameraPorts::default(),
                                capabilities: CameraCapabilities {
                                    ptz,
                                    audio,
                                    ..CameraCapabilities::default()
                                },
                                profiles: vec![ProfileSummary {
                                    name: "main".to_owned(),
                                    stream: "main".to_owned(),
                                    encoding: Some(codec.to_owned()),
                                    resolution: Some(resolution.to_owned()),
                                    framerate: None,
                                    bitrate_kbps: None,
                                    gop: None,
                                    h264_profile: None,
                                    audio: None,
                                }],
                            });
                            if cameras.len() > MAX_CAMERA_GROUPS {
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cameras.len(), MAX_CAMERA_GROUPS + 1);

    let snapshot = ProjectionSnapshot {
        config: &config,
        health: &health,
        cameras: &cameras,
        camera_database: None,
        estimated_retention_days: None,
        is_container: false,
        video_accelerations: &[],
        has_notifications_pushover: false,
        has_notifications_webhook: false,
        has_external_api_events: false,
    };
    let error = project_report(
        &snapshot,
        ConsentSelection {
            basic: true,
            camera_compatibility: true,
            feature_adoption: false,
            consented_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
        },
        INSTALL_ID,
        1,
    )
    .unwrap_err();
    assert_eq!(error, SchemaValidationError::TooManyCameraGroups);
}
