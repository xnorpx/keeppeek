use crate::cameras::{CameraCapabilities, CameraPorts};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Deserialize, Serialize)]
pub struct Health {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
pub struct CameraId(String);

impl CameraId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for CameraId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraLifecycle {
    Starting,
    Connected,
    Degraded,
    Reconnecting,
    ShuttingDown,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CameraStatus {
    pub id: CameraId,
    pub lifecycle: CameraLifecycle,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_streams: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connected_streams: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProfileSummary {
    pub name: String,
    pub stream: String,
    pub encoding: Option<String>,
    pub resolution: Option<String>,
    pub framerate: Option<f64>,
    pub bitrate_kbps: Option<u32>,
    pub gop: Option<u32>,
    pub h264_profile: Option<String>,
    pub audio: Option<AudioProfileSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AudioProfileSummary {
    pub encoding: String,
    pub sample_rate: Option<u32>,
    pub bitrate_kbps: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MotionDetection {
    pub supported: bool,
    pub controllable: bool,
    pub enabled: Option<bool>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CameraInfo {
    pub id: String,
    pub ip: String,
    pub name: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub firmware_version: Option<String>,
    pub serial_number: Option<String>,
    pub hardware_id: Option<String>,
    pub hostname: Option<String>,
    pub mac_address: Option<String>,
    pub is_reolink: bool,
    pub backend: String,
    pub transport: String,
    pub web_url: String,
    pub ports: CameraPorts,
    pub capabilities: CameraCapabilities,
    pub profiles: Vec<ProfileSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SanitizedStorage {
    pub medium_term_path: String,
    pub long_term_path: String,
    pub recording_catalog_path: String,
    pub event_thumbnail_path: String,
    pub event_thumbnail_max_mb: u64,
    pub short_term_secs: u64,
    pub medium_term_secs: u64,
    pub flush_interval_secs: u64,
    pub write_buffer_bytes: usize,
    pub long_term_max_gb: u64,
    pub minimum_free_gb: u64,
    pub maximum_used_percent: Option<u8>,
    pub warning_free_gb: u64,
    pub critical_free_gb: u64,
    pub cleanup_hysteresis_gb: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecordingCapacityEstimate {
    pub estimated_bitrate_bps: u64,
    pub bytes_per_day: u64,
    pub known_streams: usize,
    pub unknown_streams: usize,
    pub estimated_retention_days: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SanitizedConfig {
    pub host: String,
    pub port: u16,
    pub configuration_revision: String,
    pub storage: SanitizedStorage,
    pub camera_count: usize,
    pub recording_estimate: RecordingCapacityEstimate,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateRequest {
    pub offer: SdpOffer,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateResponse {
    pub session_id: String,
    pub answer: SdpAnswer,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeleteRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Status {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SdpOffer {
    #[serde(rename = "type")]
    pub sdp_type: String,
    pub sdp: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SdpAnswer {
    #[serde(rename = "type")]
    pub sdp_type: String,
    pub sdp: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogEntry {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub level: String,
    pub target: String,
    pub message: String,
    pub fields: serde_json::Value,
    pub file: Option<String>,
    pub line: Option<u64>,
}

pub mod proto {
    #![allow(clippy::all, clippy::pedantic, clippy::nursery, warnings)]
    include!(concat!(env!("OUT_DIR"), "/keeppeek.webrtc.v1.rs"));
}

pub mod backup_proto {
    #![allow(clippy::all, clippy::pedantic, clippy::nursery, warnings)]
    include!(concat!(env!("OUT_DIR"), "/keeppeek.backup.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/keeppeek.backup.v1.serde.rs"));
}

#[cfg(test)]
mod backup_proto_json_tests {
    use super::backup_proto;

    #[test]
    fn create_request_uses_canonical_protojson() {
        let request = backup_proto::CreateBackupRequest {
            client_request_id: "request-1".to_owned(),
            sections: vec![backup_proto::BackupSection::RuntimeConfig as i32],
            expected_archive_bytes: 1_048_576,
        };

        let encoded = serde_json::to_value(&request).unwrap();

        assert_eq!(encoded["clientRequestId"], "request-1");
        assert_eq!(encoded["sections"][0], "BACKUP_SECTION_RUNTIME_CONFIG");
        assert_eq!(encoded["expectedArchiveBytes"], "1048576");
        assert_eq!(
            serde_json::from_value::<backup_proto::CreateBackupRequest>(encoded).unwrap(),
            request
        );
    }

    #[test]
    fn create_request_rejects_unknown_json_fields() {
        let error = serde_json::from_str::<backup_proto::CreateBackupRequest>(
            r#"{"clientRequestId":"request-1","unexpected":true}"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn create_request_rejects_unknown_enum_values() {
        let error = serde_json::from_str::<backup_proto::CreateBackupRequest>(
            r#"{"sections":["BACKUP_SECTION_FUTURE"]}"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn create_request_rejects_uint64_overflow() {
        let decoded = serde_json::from_str::<backup_proto::CreateBackupRequest>(
            r#"{"expectedArchiveBytes":"18446744073709551616"}"#,
        );

        assert!(decoded.is_err());
    }

    #[test]
    fn restore_plan_round_trips_http_json_evidence() {
        let encoded = serde_json::json!({
            "planId": "plan-1",
            "backupId": "backup-1",
            "archiveSha256": "ab",
            "createdAtUnixMs": "10",
            "expiresAtUnixMs": "20",
            "targetRevision": "revision-3",
            "selectedSections": ["BACKUP_SECTION_RUNTIME_CONFIG"],
            "pathMappings": [{
                "kind": "BACKUP_PATH_KIND_CONFIG_DIRECTORY",
                "sourcePath": "/source",
                "targetPath": "/target"
            }],
            "issues": [{
                "severity": "RESTORE_ISSUE_SEVERITY_WARNING",
                "code": "missing_secret",
                "message": "A required secret is unavailable.",
                "section": "BACKUP_SECTION_RUNTIME_CONFIG",
                "field": "camera.password"
            }],
            "restartImpact": {
                "serverRestartRequired": true,
                "components": ["recording"],
                "consequence": "Recording restarts after activation."
            },
            "canActivate": false
        });

        let plan: backup_proto::RestorePlan = serde_json::from_value(encoded.clone()).unwrap();

        assert!(!plan.can_activate);
        let mut canonical = encoded;
        canonical.as_object_mut().unwrap().remove("canActivate");
        assert_eq!(serde_json::to_value(plan).unwrap(), canonical);
    }
}

#[cfg(test)]
mod tests {
    use super::proto;
    use prost::Message as _;

    #[test]
    fn configuration_plan_round_trips_exact_targets_and_inheritance_clear() {
        let request = proto::Request {
            request_id: 17,
            command: Some(proto::request::Command::ConfigurationCommand(
                proto::ConfigurationCommand {
                    action: Some(proto::configuration_command::Action::Plan(
                        proto::PlanConfigurationChange {
                            expected_configuration_revision: "revision-7".to_owned(),
                            targets: Some(proto::ConfigurationTargetSelector {
                                selection: Some(
                                    proto::configuration_target_selector::Selection::CameraIds(
                                        proto::CameraIdList {
                                            camera_ids: vec![
                                                "192.0.2.10".to_owned(),
                                                "192.0.2.11".to_owned(),
                                            ],
                                        },
                                    ),
                                ),
                            }),
                            change: Some(proto::ConfigurationChange {
                                change: Some(proto::configuration_change::Change::Patch(
                                    proto::CameraConfigurationPatch {
                                        backend: Some(proto::OptionalCameraBackendUpdate {
                                            value: Some(
                                                proto::optional_camera_backend_update::Value::Clear(
                                                    true,
                                                ),
                                            ),
                                        }),
                                        ..Default::default()
                                    },
                                )),
                            }),
                        },
                    )),
                },
            )),
        };

        let decoded = proto::Request::decode(request.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, request);
    }
}
