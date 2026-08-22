use crate::cameras::{CameraCapabilities, CameraPorts};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Deserialize, Serialize)]
pub struct Health {
    pub status: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Ready {
    pub ready: bool,
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
pub struct RecordingSegment {
    pub stream: String,
    pub date: String,
    pub hour: String,
    pub filename: String,
    pub url: String,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecordingsResponse {
    pub camera_id: String,
    pub date: Option<String>,
    pub dates: Vec<String>,
    pub segments: Vec<RecordingSegment>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecordingEvent {
    pub id: String,
    pub source: String,
    pub kind: String,
    pub start_time_ms: i64,
    pub end_time_ms: Option<i64>,
    pub confidence: Option<f64>,
    pub bbox: Option<[f32; 4]>,
    pub zone: Option<String>,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecordingEventsResponse {
    pub camera_id: String,
    pub date: String,
    pub events: Vec<RecordingEvent>,
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
    pub storage: SanitizedStorage,
    pub camera_count: usize,
    pub recording_estimate: RecordingCapacityEstimate,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HomeKitSettings {
    pub enabled: bool,
    pub name: String,
    pub bind: String,
    pub port: u16,
    pub exported_camera_count: usize,
    pub accessories: Vec<HomeKitAccessorySettings>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HomeKitAccessorySettings {
    pub camera_id: String,
    pub name: String,
    pub paired: bool,
    pub pairing_count: usize,
    pub port: u16,
    pub setup_code: Option<String>,
    pub setup_qr_svg_base64: Option<String>,
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
