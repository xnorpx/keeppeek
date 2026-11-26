//! Versioned, allowlisted DTOs for opt-in KeepPeek usage reporting.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const SCHEMA_VERSION_V1: u32 = 1;
pub const CURRENT_DISCLOSURE_REVISION: u32 = 1;
pub const MAX_PAYLOAD_BYTES: usize = 65_536;
pub const MAX_CAMERA_GROUPS: usize = 256;
pub const MAX_CAMERA_COUNT: u32 = 10_000;
pub const MAX_PHYSICAL_CORES: u32 = 1_024;
pub const MAX_LOGICAL_CORES: u32 = 2_048;
pub const MAX_VIDEO_ACCELERATIONS: usize = 8;
pub const MAX_CAMERA_CAPABILITIES: usize = 12;
pub const MAX_RECORDING_MODE_COUNTS: usize = 8;
pub const MAX_ENABLED_INTEGRATIONS: usize = 16;
pub const MAX_EVENT_SOURCE_TYPES: usize = 12;
pub const MAX_RETENTION_DAYS: u32 = 60;
pub const MIN_AGGREGATE_INSTALLATIONS: u32 = 10;
pub const MAX_REPORT_SEQUENCE: u64 = 9_007_199_254_740_991;

pub const PUBLISHED_SCHEMA_URL: &str =
    "https://github.com/xnorpx/keeppeek/blob/main/schemas/usage-reporting/v1.schema.json";
pub const PUBLISHED_DOCUMENTATION_URL: &str =
    "https://github.com/xnorpx/keeppeek/blob/main/docs/usage-reporting.md";
pub const PUBLISHED_RECEIVER_SOURCE_URL: &str =
    "https://github.com/xnorpx/keeppeek/tree/main/examples/usage_reporting_receiver";
pub const PUBLISHED_AGGREGATE_DATA_URL: &str = "https://github.com/xnorpx/keeppeek/blob/main/examples/usage_reporting_receiver/public/aggregate-example.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaValidationError {
    UnsupportedSchemaVersion,
    UnsupportedDisclosureRevision,
    InvalidReportSequence,
    InvalidInstallationId,
    InvalidKeeppeekVersion,
    InvalidOsVersion,
    InvalidCoreCounts,
    DuplicateVideoAcceleration,
    TooManyVideoAccelerations,
    CameraCountOutOfRange,
    TooManyCameraGroups,
    InvalidCameraManufacturer,
    InvalidCameraModel,
    DuplicateCameraCapability,
    TooManyCameraCapabilities,
    DuplicateRecordingMode,
    TooManyRecordingModes,
    DuplicateIntegration,
    TooManyIntegrations,
    DuplicateEventSourceType,
    TooManyEventSourceTypes,
    PayloadTooLarge { bytes: usize, max_bytes: usize },
    SerializationFailed,
}

impl std::fmt::Display for SchemaValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion => write!(formatter, "unsupported schema version"),
            Self::UnsupportedDisclosureRevision => {
                write!(formatter, "unsupported disclosure revision")
            }
            Self::InvalidReportSequence => write!(formatter, "invalid report sequence"),
            Self::InvalidInstallationId => write!(formatter, "invalid installation UUID"),
            Self::InvalidKeeppeekVersion => write!(formatter, "invalid keeppeek version"),
            Self::InvalidOsVersion => write!(formatter, "invalid normalized OS version"),
            Self::InvalidCoreCounts => write!(formatter, "invalid CPU core count"),
            Self::DuplicateVideoAcceleration => write!(formatter, "duplicate video acceleration"),
            Self::TooManyVideoAccelerations => write!(formatter, "too many video accelerations"),
            Self::CameraCountOutOfRange => write!(formatter, "camera count exceeds schema bound"),
            Self::TooManyCameraGroups => write!(formatter, "camera groups exceed schema bound"),
            Self::InvalidCameraManufacturer => {
                write!(formatter, "camera manufacturer is not allowlisted")
            }
            Self::InvalidCameraModel => write!(formatter, "camera model is not allowlisted"),
            Self::DuplicateCameraCapability => write!(formatter, "duplicate camera capability"),
            Self::TooManyCameraCapabilities => write!(formatter, "too many camera capabilities"),
            Self::DuplicateRecordingMode => write!(formatter, "duplicate recording mode"),
            Self::TooManyRecordingModes => write!(formatter, "too many recording modes"),
            Self::DuplicateIntegration => write!(formatter, "duplicate enabled integration"),
            Self::TooManyIntegrations => write!(formatter, "too many enabled integrations"),
            Self::DuplicateEventSourceType => write!(formatter, "duplicate event source type"),
            Self::TooManyEventSourceTypes => write!(formatter, "too many event source types"),
            Self::PayloadTooLarge { bytes, max_bytes } => {
                write!(
                    formatter,
                    "serialized payload ({bytes} bytes) exceeds {max_bytes}-byte limit"
                )
            }
            Self::SerializationFailed => write!(formatter, "failed to serialize usage report"),
        }
    }
}

impl std::error::Error for SchemaValidationError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageReportV1 {
    pub schema_version: u32,
    pub disclosure_revision: u32,
    pub report_sequence: u64,
    pub installation_id: String,
    pub basic: BasicSystemData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_compatibility: Option<CameraCompatibilityData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_adoption: Option<FeatureAdoptionData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasicSystemData {
    pub keeppeek_version: String,
    pub release_channel: ReleaseChannel,
    pub deployment_type: DeploymentType,
    pub os_family: OsFamily,
    pub os_version: String,
    pub architecture: Architecture,
    pub cpu_vendor: CpuVendor,
    pub cpu_model_family: CpuModelFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_cores: Option<u32>,
    pub logical_cores: u32,
    pub memory_bucket: MemoryBucket,
    pub video_acceleration: Vec<VideoAcceleration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
    Beta,
    Alpha,
    Nightly,
    Dev,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentType {
    Native,
    Container,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsFamily {
    Linux,
    Macos,
    Windows,
    Bsd,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
    Arm,
    Riscv64,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuVendor {
    Intel,
    Amd,
    Apple,
    Arm,
    Qualcomm,
    Ampere,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuModelFamily {
    IntelCoreUltra,
    IntelCoreI,
    IntelXeon,
    IntelNSeries,
    IntelAtomCeleronPentium,
    AmdRyzen,
    AmdEpyc,
    AmdThreadripper,
    AmdAthlon,
    AppleM1,
    AppleM2,
    AppleM3,
    AppleM4,
    AppleMOther,
    ArmCortexA,
    ArmNeoverse,
    QualcommSnapdragon,
    AmpereAltra,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemoryBucket {
    #[serde(rename = "lt_2gb")]
    Lt2Gb,
    #[serde(rename = "2_4gb")]
    From2To4Gb,
    #[serde(rename = "4_8gb")]
    From4To8Gb,
    #[serde(rename = "8_16gb")]
    From8To16Gb,
    #[serde(rename = "16_32gb")]
    From16To32Gb,
    #[serde(rename = "32_64gb")]
    From32To64Gb,
    #[serde(rename = "64_128gb")]
    From64To128Gb,
    #[serde(rename = "gte_128gb")]
    Gte128Gb,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoAcceleration {
    Vaapi,
    Nvenc,
    Qsv,
    Videotoolbox,
    V4l2,
    D3d11va,
    Vulkan,
    None,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraCompatibilityData {
    pub configured_cameras: u32,
    pub connected_cameras: u32,
    pub decodable_cameras: u32,
    pub recording_cameras: u32,
    pub groups: Vec<CameraCompatibilityGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraCompatibilityGroup {
    pub manufacturer: String,
    pub model: String,
    pub backend: TelemetryCameraBackend,
    pub transport: TelemetryCameraTransport,
    pub video_codec: VideoCodec,
    pub resolution_bucket: ResolutionBucket,
    pub capabilities: Vec<CameraCapabilityId>,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryCameraBackend {
    Auto,
    Retina,
    ReoProto,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryCameraTransport {
    Tcp,
    Udp,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    H265,
    Mjpeg,
    Vp8,
    Vp9,
    Av1,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionBucket {
    Sd,
    #[serde(rename = "hd_720p")]
    Hd720p,
    #[serde(rename = "fhd_1080p")]
    Fhd1080p,
    #[serde(rename = "qhd_1440p")]
    Qhd1440p,
    #[serde(rename = "uhd_4k")]
    Uhd4k,
    #[serde(rename = "above_4k")]
    Above4k,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraCapabilityId {
    Ptz,
    Audio,
    Events,
    Recording,
    Analytics,
    Imaging,
    TwoWayAudio,
    QuickReplies,
    Chimes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureAdoptionData {
    pub recording_modes: Vec<RecordingModeCount>,
    pub storage_capacity_bucket: StorageCapacityBucket,
    pub projected_retention_bucket: ProjectedRetentionBucket,
    pub enabled_integrations: Vec<IntegrationId>,
    pub event_source_types: Vec<EventSourceTypeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingModeCount {
    pub mode: TelemetryRecordingMode,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryRecordingMode {
    Continuous,
    EventOnly,
    Disabled,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum StorageCapacityBucket {
    #[serde(rename = "lt_250gb")]
    Lt250Gb,
    #[serde(rename = "250gb_1tb")]
    From250GbTo1Tb,
    #[serde(rename = "1tb_4tb")]
    From1TbTo4Tb,
    #[serde(rename = "4tb_16tb")]
    From4TbTo16Tb,
    #[serde(rename = "16tb_64tb")]
    From16TbTo64Tb,
    #[serde(rename = "gte_64tb")]
    Gte64Tb,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ProjectedRetentionBucket {
    #[serde(rename = "lt_1d")]
    Lt1Day,
    #[serde(rename = "1_7d")]
    From1To7Days,
    #[serde(rename = "7_30d")]
    From7To30Days,
    #[serde(rename = "30_90d")]
    From30To90Days,
    #[serde(rename = "90_365d")]
    From90To365Days,
    #[serde(rename = "gte_365d")]
    Gte365Days,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationId {
    MqttForwarder,
    HomeAssistantCard,
    NotificationsPushover,
    NotificationsWebhook,
    ExternalAuthOidc,
    ExternalAuthProxy,
    GuestSharing,
    LocalArchive,
    NamedVolumes,
    BugReporting,
    BatteryWake,
    PrivacySchedules,
    RuntimeProfiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSourceTypeId {
    OnvifEvents,
    ReolinkEvents,
    HikvisionIsapi,
    GenericMotion,
    ExternalApiEvents,
    OperationalEvents,
}

impl UsageReportV1 {
    /// Validates every field, allowlist entry, cardinality bound, and exact UTF-8 byte limit.
    pub fn validate(&self) -> Result<(), SchemaValidationError> {
        if self.schema_version != SCHEMA_VERSION_V1 {
            return Err(SchemaValidationError::UnsupportedSchemaVersion);
        }
        if self.disclosure_revision != CURRENT_DISCLOSURE_REVISION {
            return Err(SchemaValidationError::UnsupportedDisclosureRevision);
        }
        if self.report_sequence == 0 || self.report_sequence > MAX_REPORT_SEQUENCE {
            return Err(SchemaValidationError::InvalidReportSequence);
        }
        if !is_canonical_uuid_v4(&self.installation_id) {
            return Err(SchemaValidationError::InvalidInstallationId);
        }
        validate_basic(&self.basic)?;
        if let Some(camera) = &self.camera_compatibility {
            validate_camera_compatibility(camera)?;
        }
        if let Some(feature) = &self.feature_adoption {
            validate_feature_adoption(feature)?;
        }
        Ok(())
    }

    /// Validates and serializes the report to compact UTF-8 JSON capped at 65,536 bytes.
    pub fn to_bounded_json(&self) -> Result<String, SchemaValidationError> {
        self.validate()?;
        let json =
            serde_json::to_string(self).map_err(|_| SchemaValidationError::SerializationFailed)?;
        if json.len() > MAX_PAYLOAD_BYTES {
            return Err(SchemaValidationError::PayloadTooLarge {
                bytes: json.len(),
                max_bytes: MAX_PAYLOAD_BYTES,
            });
        }
        Ok(json)
    }

    /// Parses and validates a raw UTF-8 JSON report payload against the 64 KiB cap and closed schema.
    pub fn from_bounded_json(bytes: &[u8]) -> Result<Self, SchemaValidationError> {
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(SchemaValidationError::PayloadTooLarge {
                bytes: bytes.len(),
                max_bytes: MAX_PAYLOAD_BYTES,
            });
        }
        let report: Self = serde_json::from_slice(bytes)
            .map_err(|_| SchemaValidationError::SerializationFailed)?;
        report.validate()?;
        Ok(report)
    }
}

pub fn is_canonical_uuid_v4(value: &str) -> bool {
    if value.len() != 36 || !value.bytes().all(|byte| !byte.is_ascii_uppercase()) {
        return false;
    }
    Uuid::parse_str(value).is_ok_and(|parsed| parsed.get_version_num() == 4)
}

fn validate_basic(basic: &BasicSystemData) -> Result<(), SchemaValidationError> {
    if !is_valid_keeppeek_version(&basic.keeppeek_version) {
        return Err(SchemaValidationError::InvalidKeeppeekVersion);
    }
    if !is_valid_os_version(&basic.os_version) {
        return Err(SchemaValidationError::InvalidOsVersion);
    }
    if basic.logical_cores == 0 || basic.logical_cores > MAX_LOGICAL_CORES {
        return Err(SchemaValidationError::InvalidCoreCounts);
    }
    if let Some(physical) = basic.physical_cores
        && (physical == 0 || physical > MAX_PHYSICAL_CORES)
    {
        return Err(SchemaValidationError::InvalidCoreCounts);
    }
    if basic.video_acceleration.len() > MAX_VIDEO_ACCELERATIONS {
        return Err(SchemaValidationError::TooManyVideoAccelerations);
    }
    let mut seen = BTreeSet::new();
    for item in &basic.video_acceleration {
        if !seen.insert(*item) {
            return Err(SchemaValidationError::DuplicateVideoAcceleration);
        }
    }
    Ok(())
}

fn validate_camera_compatibility(
    data: &CameraCompatibilityData,
) -> Result<(), SchemaValidationError> {
    if data.configured_cameras > MAX_CAMERA_COUNT
        || data.connected_cameras > MAX_CAMERA_COUNT
        || data.decodable_cameras > MAX_CAMERA_COUNT
        || data.recording_cameras > MAX_CAMERA_COUNT
    {
        return Err(SchemaValidationError::CameraCountOutOfRange);
    }
    if data.groups.len() > MAX_CAMERA_GROUPS {
        return Err(SchemaValidationError::TooManyCameraGroups);
    }
    for group in &data.groups {
        if !super::normalize::is_allowed_manufacturer(&group.manufacturer) {
            return Err(SchemaValidationError::InvalidCameraManufacturer);
        }
        if !super::normalize::is_allowed_model_for_manufacturer(&group.manufacturer, &group.model) {
            return Err(SchemaValidationError::InvalidCameraModel);
        }
        if group.count == 0 || group.count > MAX_CAMERA_COUNT {
            return Err(SchemaValidationError::CameraCountOutOfRange);
        }
        if group.capabilities.len() > MAX_CAMERA_CAPABILITIES {
            return Err(SchemaValidationError::TooManyCameraCapabilities);
        }
        let mut seen = BTreeSet::new();
        for cap in &group.capabilities {
            if !seen.insert(*cap) {
                return Err(SchemaValidationError::DuplicateCameraCapability);
            }
        }
    }
    Ok(())
}

fn validate_feature_adoption(data: &FeatureAdoptionData) -> Result<(), SchemaValidationError> {
    if data.recording_modes.len() > MAX_RECORDING_MODE_COUNTS {
        return Err(SchemaValidationError::TooManyRecordingModes);
    }
    let mut seen_modes = BTreeSet::new();
    for entry in &data.recording_modes {
        if entry.count == 0 || entry.count > MAX_CAMERA_COUNT {
            return Err(SchemaValidationError::CameraCountOutOfRange);
        }
        if !seen_modes.insert(entry.mode) {
            return Err(SchemaValidationError::DuplicateRecordingMode);
        }
    }
    if data.enabled_integrations.len() > MAX_ENABLED_INTEGRATIONS {
        return Err(SchemaValidationError::TooManyIntegrations);
    }
    let mut seen_integrations = BTreeSet::new();
    for integration in &data.enabled_integrations {
        if !seen_integrations.insert(*integration) {
            return Err(SchemaValidationError::DuplicateIntegration);
        }
    }
    if data.event_source_types.len() > MAX_EVENT_SOURCE_TYPES {
        return Err(SchemaValidationError::TooManyEventSourceTypes);
    }
    let mut seen_sources = BTreeSet::new();
    for source in &data.event_source_types {
        if !seen_sources.insert(*source) {
            return Err(SchemaValidationError::DuplicateEventSourceType);
        }
    }
    Ok(())
}

fn is_valid_keeppeek_version(value: &str) -> bool {
    if value.len() < 5 || value.len() > 64 {
        return false;
    }
    let (core, pre) = match value.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (value, None),
    };
    let mut parts = core.split('.');
    let valid_core = (0..3).all(|_| {
        parts.next().is_some_and(|part| {
            !part.is_empty() && part.len() <= 3 && part.bytes().all(|byte| byte.is_ascii_digit())
        })
    }) && parts.next().is_none();
    if !valid_core {
        return false;
    }
    pre.is_none_or(|pre| {
        !pre.is_empty()
            && pre.len() <= 32
            && pre
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
    })
}

fn is_valid_os_version(value: &str) -> bool {
    if value == "unknown" || value == "rolling" {
        return true;
    }
    if value.is_empty() || value.len() > 16 {
        return false;
    }
    let mut parts = value.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    if major.is_empty() || major.len() > 4 || !major.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    parts.next().is_none_or(|minor| {
        parts.next().is_none()
            && !minor.is_empty()
            && minor.len() <= 4
            && minor.bytes().all(|byte| byte.is_ascii_digit())
    })
}
