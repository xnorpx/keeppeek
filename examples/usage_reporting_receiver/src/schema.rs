//! Closed-schema validation for incoming KeepPeek v1 usage reports.

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
pub const MAX_REPORT_SEQUENCE: u64 = 9_007_199_254_740_991;

pub const ALLOWED_MANUFACTURERS: &[&str] = &[
    "amcrest",
    "annke",
    "aqara",
    "axis",
    "bosch",
    "dahua",
    "eufy",
    "ezviz",
    "foscam",
    "hanwha",
    "hikvision",
    "imou",
    "lorex",
    "reolink",
    "sonoff",
    "swann",
    "tapo",
    "tp_link",
    "ubiquiti",
    "uniview",
    "vivotek",
    "wyze",
    "unknown",
];

pub const ALLOWED_MODELS: &[(&str, &str)] = &[
    ("amcrest", "amcrest_ad410"),
    ("amcrest", "amcrest_ip4m_1041"),
    ("amcrest", "amcrest_ip5m_t1179e"),
    ("amcrest", "amcrest_ip8m_2496e"),
    ("amcrest", "amcrest_ip8m_t2599e"),
    ("annke", "annke_c500"),
    ("annke", "annke_c800"),
    ("annke", "annke_ncd800"),
    ("aqara", "aqara_g2h_pro"),
    ("aqara", "aqara_g3"),
    ("aqara", "aqara_g4"),
    ("axis", "axis_m1065_lw"),
    ("axis", "axis_m2025_le"),
    ("axis", "axis_p1448_le"),
    ("axis", "axis_p3245_lv"),
    ("bosch", "bosch_flexidome_ip"),
    ("dahua", "dahua_ipc_hdw2431t"),
    ("dahua", "dahua_ipc_hdw5442t"),
    ("dahua", "dahua_ipc_hfw2831t"),
    ("dahua", "dahua_ipc_t5442t"),
    ("dahua", "dahua_vto2202f"),
    ("eufy", "eufy_indoor_cam_2k"),
    ("eufy", "eufy_indoor_cam_c120"),
    ("eufy", "eufy_s330"),
    ("ezviz", "ezviz_c3n"),
    ("ezviz", "ezviz_c6n"),
    ("ezviz", "ezviz_db1"),
    ("foscam", "foscam_c1"),
    ("foscam", "foscam_fi9821p"),
    ("foscam", "foscam_r2"),
    ("hanwha", "hanwha_qno_6012r"),
    ("hanwha", "hanwha_xno_6080r"),
    ("hikvision", "hikvision_ds_2cd2043g2"),
    ("hikvision", "hikvision_ds_2cd2087g2"),
    ("hikvision", "hikvision_ds_2cd2143g2"),
    ("hikvision", "hikvision_ds_2cd2343g2"),
    ("hikvision", "hikvision_ds_2cd2347g2"),
    ("hikvision", "hikvision_ds_2cd2387g2"),
    ("imou", "imou_bullet_2c"),
    ("imou", "imou_ranger_2"),
    ("lorex", "lorex_e891ab"),
    ("lorex", "lorex_lne8924ab"),
    ("reolink", "reolink_argus_3_pro"),
    ("reolink", "reolink_cx410"),
    ("reolink", "reolink_cx810"),
    ("reolink", "reolink_duo_2_poe"),
    ("reolink", "reolink_duo_3_poe"),
    ("reolink", "reolink_e1_outdoor"),
    ("reolink", "reolink_e1_pro"),
    ("reolink", "reolink_e1_zoom"),
    ("reolink", "reolink_rlc_410"),
    ("reolink", "reolink_rlc_510a"),
    ("reolink", "reolink_rlc_520a"),
    ("reolink", "reolink_rlc_810a"),
    ("reolink", "reolink_rlc_811a"),
    ("reolink", "reolink_rlc_820a"),
    ("reolink", "reolink_rlc_823a"),
    ("reolink", "reolink_rlc_1212a"),
    ("reolink", "reolink_rlc_1224a"),
    ("reolink", "reolink_trackmix_poe"),
    ("reolink", "reolink_video_doorbell_poe"),
    ("reolink", "reolink_video_doorbell_wifi"),
    ("sonoff", "sonoff_gk_200mp2_b"),
    ("swann", "swann_nht_865"),
    ("tapo", "tapo_c110"),
    ("tapo", "tapo_c200"),
    ("tapo", "tapo_c210"),
    ("tapo", "tapo_c310"),
    ("tapo", "tapo_c320ws"),
    ("tapo", "tapo_c520ws"),
    ("tp_link", "tp_link_vigi_c340"),
    ("tp_link", "tp_link_vigi_c440"),
    ("ubiquiti", "ubiquiti_g3_flex"),
    ("ubiquiti", "ubiquiti_g4_bullet"),
    ("ubiquiti", "ubiquiti_g4_doorbell_pro"),
    ("ubiquiti", "ubiquiti_g4_instant"),
    ("ubiquiti", "ubiquiti_g4_pro"),
    ("ubiquiti", "ubiquiti_g5_bullet"),
    ("ubiquiti", "ubiquiti_g5_dome"),
    ("ubiquiti", "ubiquiti_g5_flex"),
    ("uniview", "uniview_ipc2124lr3"),
    ("uniview", "uniview_ipc3614le"),
    ("vivotek", "vivotek_fd9165"),
    ("vivotek", "vivotek_ib9365"),
    ("wyze", "wyze_cam_v3"),
    ("wyze", "wyze_cam_v4"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    PayloadTooLarge,
    InvalidJson,
    UnsupportedSchemaVersion,
    UnsupportedDisclosureRevision,
    InvalidReportSequence,
    InvalidInstallationId,
    InvalidFieldValue,
    BoundExceeded,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraCompatibilityGroup {
    pub manufacturer: String,
    pub model: String,
    pub backend: CameraBackend,
    pub transport: CameraTransport,
    pub video_codec: VideoCodec,
    pub resolution_bucket: ResolutionBucket,
    pub capabilities: Vec<CameraCapabilityId>,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraBackend {
    Auto,
    Retina,
    ReoProto,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraTransport {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingModeCount {
    pub mode: RecordingMode,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
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

pub fn parse_and_validate(bytes: &[u8]) -> Result<(UsageReportV1, String), ValidationError> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(ValidationError::PayloadTooLarge);
    }
    let report: UsageReportV1 =
        serde_json::from_slice(bytes).map_err(|_| ValidationError::InvalidJson)?;
    validate_report(&report)?;
    let canonical = serde_json::to_string(&report).map_err(|_| ValidationError::InvalidJson)?;
    if canonical.len() > MAX_PAYLOAD_BYTES {
        return Err(ValidationError::PayloadTooLarge);
    }
    Ok((report, canonical))
}

pub fn is_canonical_uuid_v4(value: &str) -> bool {
    if value.len() != 36 || !value.bytes().all(|byte| !byte.is_ascii_uppercase()) {
        return false;
    }
    Uuid::parse_str(value).is_ok_and(|parsed| parsed.get_version_num() == 4)
}

pub fn validate_report(report: &UsageReportV1) -> Result<(), ValidationError> {
    if report.schema_version != SCHEMA_VERSION_V1 {
        return Err(ValidationError::UnsupportedSchemaVersion);
    }
    if report.disclosure_revision != CURRENT_DISCLOSURE_REVISION {
        return Err(ValidationError::UnsupportedDisclosureRevision);
    }
    if report.report_sequence == 0 || report.report_sequence > MAX_REPORT_SEQUENCE {
        return Err(ValidationError::InvalidReportSequence);
    }
    if !is_canonical_uuid_v4(&report.installation_id) {
        return Err(ValidationError::InvalidInstallationId);
    }
    validate_basic(&report.basic)?;
    if let Some(camera) = &report.camera_compatibility {
        validate_camera(camera)?;
    }
    if let Some(feature) = &report.feature_adoption {
        validate_feature(feature)?;
    }
    Ok(())
}

fn validate_basic(basic: &BasicSystemData) -> Result<(), ValidationError> {
    if !valid_semver(&basic.keeppeek_version) || !valid_os_version(&basic.os_version) {
        return Err(ValidationError::InvalidFieldValue);
    }
    if basic.logical_cores == 0 || basic.logical_cores > MAX_LOGICAL_CORES {
        return Err(ValidationError::BoundExceeded);
    }
    if let Some(physical) = basic.physical_cores
        && (physical == 0 || physical > MAX_PHYSICAL_CORES)
    {
        return Err(ValidationError::BoundExceeded);
    }
    if basic.video_acceleration.len() > MAX_VIDEO_ACCELERATIONS {
        return Err(ValidationError::BoundExceeded);
    }
    let mut seen = BTreeSet::new();
    for accel in &basic.video_acceleration {
        if !seen.insert(*accel) {
            return Err(ValidationError::InvalidFieldValue);
        }
    }
    Ok(())
}

fn validate_camera(camera: &CameraCompatibilityData) -> Result<(), ValidationError> {
    if camera.configured_cameras > MAX_CAMERA_COUNT
        || camera.connected_cameras > MAX_CAMERA_COUNT
        || camera.decodable_cameras > MAX_CAMERA_COUNT
        || camera.recording_cameras > MAX_CAMERA_COUNT
        || camera.groups.len() > MAX_CAMERA_GROUPS
    {
        return Err(ValidationError::BoundExceeded);
    }
    for group in &camera.groups {
        if !ALLOWED_MANUFACTURERS.contains(&group.manufacturer.as_str()) {
            return Err(ValidationError::InvalidFieldValue);
        }
        if group.model != "unknown"
            && !ALLOWED_MODELS.contains(&(group.manufacturer.as_str(), group.model.as_str()))
        {
            return Err(ValidationError::InvalidFieldValue);
        }
        if group.count == 0
            || group.count > MAX_CAMERA_COUNT
            || group.capabilities.len() > MAX_CAMERA_CAPABILITIES
        {
            return Err(ValidationError::BoundExceeded);
        }
        let mut seen = BTreeSet::new();
        for cap in &group.capabilities {
            if !seen.insert(*cap) {
                return Err(ValidationError::InvalidFieldValue);
            }
        }
    }
    Ok(())
}

fn validate_feature(feature: &FeatureAdoptionData) -> Result<(), ValidationError> {
    if feature.recording_modes.len() > MAX_RECORDING_MODE_COUNTS
        || feature.enabled_integrations.len() > MAX_ENABLED_INTEGRATIONS
        || feature.event_source_types.len() > MAX_EVENT_SOURCE_TYPES
    {
        return Err(ValidationError::BoundExceeded);
    }
    let mut seen_modes = BTreeSet::new();
    for entry in &feature.recording_modes {
        if entry.count == 0 || entry.count > MAX_CAMERA_COUNT {
            return Err(ValidationError::BoundExceeded);
        }
        if !seen_modes.insert(entry.mode) {
            return Err(ValidationError::InvalidFieldValue);
        }
    }
    let mut seen_integrations = BTreeSet::new();
    for item in &feature.enabled_integrations {
        if !seen_integrations.insert(*item) {
            return Err(ValidationError::InvalidFieldValue);
        }
    }
    let mut seen_sources = BTreeSet::new();
    for item in &feature.event_source_types {
        if !seen_sources.insert(*item) {
            return Err(ValidationError::InvalidFieldValue);
        }
    }
    Ok(())
}

fn valid_semver(value: &str) -> bool {
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

fn valid_os_version(value: &str) -> bool {
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
