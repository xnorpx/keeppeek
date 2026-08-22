use crate::tlv8::Tlv8Writer;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Serialize, Serializer};
use std::{collections::BTreeSet, error::Error as StdError, fmt};

const ADVERTISE_MULTI_TIER_RTP: bool = false;
const SERVICE_ACCESSORY_INFORMATION: &str = "0000003E-0000-1000-8000-0026BB765291";
const SERVICE_CAMERA_CAPABILITIES: &str = "00008010-0000-1000-8000-0026BB765291";
const SERVICE_CAMERA_MULTI_TIER_RTP: &str = "00008031-0000-1000-8000-0026BB765291";
const SERVICE_CAMERA_GLOBAL_OPERATING_MODE: &str = "00008032-0000-1000-8000-0026BB765291";
const SERVICE_CAMERA_WEBRTC: &str = "00008033-0000-1000-8000-0026BB765291";
const SERVICE_CAMERA_RTP_STREAM_MANAGEMENT: &str = "00000110-0000-1000-8000-0026BB765291";
const SERVICE_MICROPHONE: &str = "00000112-0000-1000-8000-0026BB765291";
const CHAR_IDENTIFY: &str = "00000014-0000-1000-8000-0026BB765291";
const CHAR_MANUFACTURER: &str = "00000020-0000-1000-8000-0026BB765291";
const CHAR_MODEL: &str = "00000021-0000-1000-8000-0026BB765291";
const CHAR_NAME: &str = "00000023-0000-1000-8000-0026BB765291";
const CHAR_SERIAL: &str = "00000030-0000-1000-8000-0026BB765291";
const CHAR_FIRMWARE: &str = "00000052-0000-1000-8000-0026BB765291";
const CHAR_VERSION: &str = "00000037-0000-1000-8000-0026BB765291";
const CHAR_CAMERA_CAPABILITIES: &str = "00008011-0000-1000-8000-0026BB765291";
const CHAR_STATUS_ACTIVE: &str = "00000075-0000-1000-8000-0026BB765291";
const CHAR_ACTIVE: &str = "000000B0-0000-1000-8000-0026BB765291";
const CHAR_SUPPORTED_VIDEO_STREAM_CONFIGURATION: &str = "00000114-0000-1000-8000-0026BB765291";
const CHAR_SUPPORTED_AUDIO_STREAM_CONFIGURATION: &str = "00000115-0000-1000-8000-0026BB765291";
const CHAR_SUPPORTED_RTP_CONFIGURATION: &str = "00000116-0000-1000-8000-0026BB765291";
const CHAR_SELECTED_RTP_STREAM_CONFIGURATION: &str = "00000117-0000-1000-8000-0026BB765291";
const CHAR_SETUP_ENDPOINTS: &str = "00000118-0000-1000-8000-0026BB765291";
const CHAR_MUTE: &str = "0000011A-0000-1000-8000-0026BB765291";
const CHAR_STREAMING_STATUS: &str = "00000120-0000-1000-8000-0026BB765291";
const CHAR_HOMEKIT_CAMERA_ACTIVE: &str = "0000021B-0000-1000-8000-0026BB765291";
const CHAR_CAMERA_OPERATING_MODE_INDICATOR: &str = "0000021D-0000-1000-8000-0026BB765291";
const CHAR_STREAMING_ENABLED: &str = "00008041-0000-1000-8000-0026BB765291";
const CHAR_SUPPORTED_VIDEO_STREAM_TIERS: &str = "00008043-0000-1000-8000-0026BB765291";
const CHAR_SUPPORTED_AUDIO_STREAM_TIERS: &str = "00008044-0000-1000-8000-0026BB765291";
const CHAR_RTP_STREAMING_CONTROL: &str = "00008045-0000-1000-8000-0026BB765291";
const CHAR_WEBRTC_SOLICIT_OFFER: &str = "00008053-0000-1000-8000-0026BB765291";
const CHAR_WEBRTC_PROVIDE_ANSWER: &str = "00008054-0000-1000-8000-0026BB765291";
const CHAR_WEBRTC_UPDATE_SESSION: &str = "0000805C-0000-1000-8000-0026BB765291";
const CHAR_WEBRTC_STREAMING_CONTROL: &str = "00008056-0000-1000-8000-0026BB765291";
const CHAR_WEBRTC_ACTIVE_SESSIONS: &str = "00008057-0000-1000-8000-0026BB765291";
const CHAR_WEBRTC_REOFFER: &str = "00008058-0000-1000-8000-0026BB765291";
const CHAR_WEBRTC_VIDEO_TIERS: &str = "00008059-0000-1000-8000-0026BB765291";
const CHAR_WEBRTC_AUDIO_TIERS: &str = "0000805A-0000-1000-8000-0026BB765291";
const CHAR_SENSOR_UUID: &str = "0000805B-0000-1000-8000-0026BB765291";

/// Common identity characteristics for a HAP accessory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessoryInformation {
    pub name: String,
    pub manufacturer: String,
    pub model: String,
    pub serial_number: String,
    pub firmware_revision: String,
}

/// Video codec advertised by Apple's camera tier characteristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VideoCodec {
    H264 = 1,
    H265 = 2,
}

/// Quality role assigned to an encoded camera tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum VideoQuality {
    Highest = 1,
    High = 2,
    Medium = 3,
    Low = 4,
}

/// One encoded video tier exposed through HomeKit WebRTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTier {
    pub identifier: u32,
    pub quality: VideoQuality,
    pub target_average_bitrate_kbps: u32,
    pub width: u16,
    pub height: u16,
    pub frame_rate: u8,
}

/// Required single Opus audio tier exposed through HomeKit WebRTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTier {
    pub identifier: u32,
    pub target_average_bitrate_bps: u32,
}

/// Configuration for one standalone KeepPeek camera accessory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraConfig {
    pub information: AccessoryInformation,
    pub sensor_uuid: [u8; 16],
    pub video_codec: VideoCodec,
    pub video_payload_type: u8,
    pub video_tiers: Vec<VideoTier>,
    pub opus_payload_type: u8,
    pub audio_tier: AudioTier,
}

/// Serialized standalone HAP camera accessory database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccessoryDatabase {
    accessories: Vec<Accessory>,
}

impl AccessoryDatabase {
    /// Builds one standalone camera accessory at HAP AID 1.
    pub fn camera(camera: CameraConfig) -> Result<Self, ModelError> {
        validate_stream_configuration(&camera)?;
        let mut services = vec![
            information_service(&camera.information),
            camera_webrtc_service(&camera),
            camera_capabilities_service(&camera),
            camera_global_operating_mode_service(),
            microphone_service(),
            camera_rtp_stream_service(&camera, 37),
        ];
        if ADVERTISE_MULTI_TIER_RTP {
            services.push(camera_multi_tier_rtp_service(&camera));
        }
        Ok(Self {
            accessories: vec![Accessory { aid: 1, services }],
        })
    }

    /// Builds the baseline HAP IP camera profile with two simultaneous RTP slots.
    pub fn legacy_camera(camera: CameraConfig) -> Result<Self, ModelError> {
        validate_stream_configuration(&camera)?;
        Ok(Self {
            accessories: vec![Accessory {
                aid: 1,
                services: vec![
                    information_service(&camera.information),
                    microphone_service(),
                    camera_rtp_stream_service(&camera, 37),
                    camera_rtp_stream_service(&camera, 45),
                ],
            }],
        })
    }

    /// Returns the number of accessories in the database.
    pub const fn len(&self) -> usize {
        self.accessories.len()
    }

    /// Returns whether the database contains no accessories.
    pub const fn is_empty(&self) -> bool {
        self.accessories.is_empty()
    }

    /// Encodes the exact `GET /accessories` JSON response body.
    pub fn to_json(&self) -> Result<Vec<u8>, ModelError> {
        serde_json::to_vec(self).map_err(|_| ModelError::Serialize)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Accessory {
    aid: u64,
    services: Vec<Service>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Service {
    iid: u64,
    #[serde(rename = "type")]
    service_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary: Option<bool>,
    characteristics: Vec<Characteristic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Characteristic {
    iid: u64,
    #[serde(rename = "type")]
    characteristic_type: &'static str,
    perms: Vec<Permission>,
    format: Format,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<CharacteristicValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum Permission {
    #[serde(rename = "pr")]
    Read,
    #[serde(rename = "pw")]
    Write,
    #[serde(rename = "ev")]
    Events,
    #[serde(rename = "tw")]
    TimedWrite,
    #[serde(rename = "wr")]
    WriteResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Format {
    Bool,
    Uint8,
    String,
    Tlv8,
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CharacteristicValue {
    Bool(bool),
    Uint8(u8),
    String(String),
    Bytes(Vec<u8>),
}

impl Serialize for CharacteristicValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Uint8(value) => serializer.serialize_u8(*value),
            Self::String(value) => serializer.serialize_str(value),
            Self::Bytes(value) => serializer.serialize_str(&STANDARD.encode(value)),
        }
    }
}

/// Accessory database validation or serialization failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    InvalidPayloadType(u8),
    InvalidVideoTierCount(usize),
    MissingVideoQuality(VideoQuality),
    DuplicateVideoTierIdentifier(u32),
    InvalidVideoTier(u32),
    InvalidAudioTier,
    Serialize,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPayloadType(payload_type) => {
                write!(f, "invalid RTP payload type {payload_type}")
            }
            Self::InvalidVideoTierCount(count) => write!(f, "camera has {count} video tiers"),
            Self::MissingVideoQuality(quality) => write!(f, "missing {quality:?} video tier"),
            Self::DuplicateVideoTierIdentifier(identifier) => {
                write!(f, "duplicate video tier identifier {identifier}")
            }
            Self::InvalidVideoTier(identifier) => write!(f, "invalid video tier {identifier}"),
            Self::InvalidAudioTier => f.write_str("invalid Opus audio tier"),
            Self::Serialize => f.write_str("unable to serialize accessory database"),
        }
    }
}

impl StdError for ModelError {}

fn validate_stream_configuration(camera: &CameraConfig) -> Result<(), ModelError> {
    if camera.video_payload_type > 127 {
        return Err(ModelError::InvalidPayloadType(camera.video_payload_type));
    }
    if camera.opus_payload_type > 127 {
        return Err(ModelError::InvalidPayloadType(camera.opus_payload_type));
    }
    if !(3..=4).contains(&camera.video_tiers.len()) {
        return Err(ModelError::InvalidVideoTierCount(camera.video_tiers.len()));
    }
    for required in [VideoQuality::High, VideoQuality::Medium, VideoQuality::Low] {
        if !camera
            .video_tiers
            .iter()
            .any(|tier| tier.quality == required)
        {
            return Err(ModelError::MissingVideoQuality(required));
        }
    }
    let mut identifiers = BTreeSet::new();
    for tier in &camera.video_tiers {
        if !identifiers.insert(tier.identifier) {
            return Err(ModelError::DuplicateVideoTierIdentifier(tier.identifier));
        }
        if tier.target_average_bitrate_kbps == 0
            || tier.width == 0
            || tier.height == 0
            || tier.frame_rate == 0
        {
            return Err(ModelError::InvalidVideoTier(tier.identifier));
        }
    }
    if camera.audio_tier.target_average_bitrate_bps == 0 {
        return Err(ModelError::InvalidAudioTier);
    }
    Ok(())
}

fn information_service(information: &AccessoryInformation) -> Service {
    Service {
        iid: 1,
        service_type: SERVICE_ACCESSORY_INFORMATION,
        primary: None,
        characteristics: vec![
            characteristic(
                2,
                CHAR_IDENTIFY,
                vec![Permission::Write],
                Format::Bool,
                CharacteristicValue::Bool(false),
            ),
            readable_string(3, CHAR_MANUFACTURER, &information.manufacturer),
            readable_string(4, CHAR_MODEL, &information.model),
            readable_string(5, CHAR_NAME, &information.name),
            readable_string(6, CHAR_SERIAL, &information.serial_number),
            readable_string(7, CHAR_FIRMWARE, &information.firmware_revision),
        ],
    }
}

fn camera_webrtc_service(camera: &CameraConfig) -> Service {
    let control_permissions = vec![
        Permission::Read,
        Permission::Write,
        Permission::WriteResponse,
    ];
    let supported_permissions = vec![Permission::Read, Permission::Events];
    Service {
        iid: 8,
        service_type: SERVICE_CAMERA_WEBRTC,
        // Let's try making it primary, since maybe Home requires the primary service to be the one it uses?
        primary: Some(true),
        characteristics: vec![
            characteristic(
                9,
                CHAR_STREAMING_ENABLED,
                vec![
                    Permission::Read,
                    Permission::Write,
                    Permission::Events,
                    Permission::TimedWrite,
                ],
                Format::Bool,
                CharacteristicValue::Bool(true),
            ),
            characteristic(
                10,
                CHAR_WEBRTC_SOLICIT_OFFER,
                control_permissions.clone(),
                Format::Tlv8,
                CharacteristicValue::Bytes(Vec::new()),
            ),
            characteristic(
                11,
                CHAR_WEBRTC_PROVIDE_ANSWER,
                control_permissions.clone(),
                Format::Tlv8,
                CharacteristicValue::Bytes(Vec::new()),
            ),
            characteristic(
                12,
                CHAR_WEBRTC_STREAMING_CONTROL,
                control_permissions.clone(),
                Format::Tlv8,
                CharacteristicValue::Bytes(Vec::new()),
            ),
            characteristic(
                13,
                CHAR_WEBRTC_ACTIVE_SESSIONS,
                vec![Permission::Read, Permission::Events],
                Format::Uint8,
                CharacteristicValue::Uint8(0),
            ),
            characteristic(
                14,
                CHAR_WEBRTC_REOFFER,
                control_permissions.clone(),
                Format::Tlv8,
                CharacteristicValue::Bytes(Vec::new()),
            ),
            characteristic(
                15,
                CHAR_WEBRTC_UPDATE_SESSION,
                control_permissions,
                Format::Tlv8,
                CharacteristicValue::Bytes(Vec::new()),
            ),
            characteristic(
                16,
                CHAR_WEBRTC_VIDEO_TIERS,
                supported_permissions.clone(),
                Format::Tlv8,
                CharacteristicValue::Bytes(encode_video_tiers(camera)),
            ),
            characteristic(
                17,
                CHAR_WEBRTC_AUDIO_TIERS,
                supported_permissions,
                Format::Tlv8,
                CharacteristicValue::Bytes(encode_audio_tier(camera)),
            ),
            characteristic(
                18,
                CHAR_SENSOR_UUID,
                vec![Permission::Read],
                Format::Data,
                CharacteristicValue::Bytes(camera.sensor_uuid.to_vec()),
            ),
        ],
    }
}

fn camera_capabilities_service(camera: &CameraConfig) -> Service {
    Service {
        iid: 19,
        service_type: SERVICE_CAMERA_CAPABILITIES,
        primary: None,
        characteristics: vec![
            readable_string(20, CHAR_VERSION, "17.99"),
            characteristic(
                21,
                CHAR_CAMERA_CAPABILITIES,
                vec![Permission::Read],
                Format::Tlv8,
                CharacteristicValue::Bytes(encode_camera_capabilities(camera)),
            ),
        ],
    }
}

fn camera_global_operating_mode_service() -> Service {
    let operating_permissions = vec![Permission::Read, Permission::Write, Permission::Events];
    let streaming_permissions = vec![
        Permission::Read,
        Permission::Write,
        Permission::Events,
        Permission::TimedWrite,
    ];
    Service {
        iid: 22,
        service_type: SERVICE_CAMERA_GLOBAL_OPERATING_MODE,
        primary: None,
        characteristics: vec![
            characteristic(
                23,
                CHAR_HOMEKIT_CAMERA_ACTIVE,
                operating_permissions.clone(),
                Format::Bool,
                CharacteristicValue::Bool(true),
            ),
            characteristic(
                24,
                CHAR_STREAMING_ENABLED,
                streaming_permissions,
                Format::Bool,
                CharacteristicValue::Bool(true),
            ),
            characteristic(
                25,
                CHAR_CAMERA_OPERATING_MODE_INDICATOR,
                operating_permissions,
                Format::Bool,
                CharacteristicValue::Bool(true),
            ),
        ],
    }
}

fn camera_multi_tier_rtp_service(camera: &CameraConfig) -> Service {
    let control_permissions = vec![
        Permission::Read,
        Permission::Write,
        Permission::WriteResponse,
    ];
    let supported_permissions = vec![Permission::Read, Permission::Events];
    Service {
        iid: 26,
        service_type: SERVICE_CAMERA_MULTI_TIER_RTP,
        primary: None,
        characteristics: vec![
            characteristic(
                27,
                CHAR_STREAMING_ENABLED,
                vec![
                    Permission::Read,
                    Permission::Write,
                    Permission::Events,
                    Permission::TimedWrite,
                ],
                Format::Bool,
                CharacteristicValue::Bool(true),
            ),
            characteristic(
                28,
                CHAR_STATUS_ACTIVE,
                vec![Permission::Read, Permission::Events],
                Format::Bool,
                CharacteristicValue::Bool(false),
            ),
            characteristic(
                29,
                CHAR_SUPPORTED_VIDEO_STREAM_TIERS,
                supported_permissions.clone(),
                Format::Tlv8,
                CharacteristicValue::Bytes(encode_video_tiers(camera)),
            ),
            characteristic(
                30,
                CHAR_SUPPORTED_AUDIO_STREAM_TIERS,
                supported_permissions,
                Format::Tlv8,
                CharacteristicValue::Bytes(encode_audio_tier(camera)),
            ),
            characteristic(
                31,
                CHAR_SUPPORTED_RTP_CONFIGURATION,
                vec![Permission::Read],
                Format::Tlv8,
                CharacteristicValue::Bytes(vec![2, 1, 0]),
            ),
            characteristic(
                32,
                CHAR_SETUP_ENDPOINTS,
                vec![Permission::Read, Permission::Write],
                Format::Tlv8,
                CharacteristicValue::Bytes(Vec::new()),
            ),
            characteristic(
                33,
                CHAR_RTP_STREAMING_CONTROL,
                control_permissions,
                Format::Tlv8,
                CharacteristicValue::Bytes(Vec::new()),
            ),
            characteristic(
                34,
                CHAR_SENSOR_UUID,
                vec![Permission::Read],
                Format::Data,
                CharacteristicValue::Bytes(camera.sensor_uuid.to_vec()),
            ),
        ],
    }
}

fn microphone_service() -> Service {
    Service {
        iid: 35,
        service_type: SERVICE_MICROPHONE,
        primary: None,
        characteristics: vec![characteristic(
            36,
            CHAR_MUTE,
            vec![Permission::Read, Permission::Write, Permission::Events],
            Format::Bool,
            CharacteristicValue::Bool(false),
        )],
    }
}

fn camera_rtp_stream_service(_camera: &CameraConfig, iid: u64) -> Service {
    Service {
        iid,
        service_type: SERVICE_CAMERA_RTP_STREAM_MANAGEMENT,
        primary: None,
        characteristics: vec![
            characteristic(
                iid + 1,
                CHAR_STREAMING_STATUS,
                vec![Permission::Read, Permission::Events],
                Format::Tlv8,
                CharacteristicValue::Bytes(vec![1, 1, 0]),
            ),
            characteristic(
                iid + 2,
                CHAR_SUPPORTED_VIDEO_STREAM_CONFIGURATION,
                vec![Permission::Read],
                Format::Tlv8,
                CharacteristicValue::Bytes(encode_legacy_video_configuration()),
            ),
            characteristic(
                iid + 3,
                CHAR_SUPPORTED_AUDIO_STREAM_CONFIGURATION,
                vec![Permission::Read],
                Format::Tlv8,
                CharacteristicValue::Bytes(encode_legacy_audio_configuration()),
            ),
            characteristic(
                iid + 4,
                CHAR_SUPPORTED_RTP_CONFIGURATION,
                vec![Permission::Read],
                Format::Tlv8,
                CharacteristicValue::Bytes(vec![2, 1, 0]),
            ),
            characteristic(
                iid + 5,
                CHAR_ACTIVE,
                vec![Permission::Read, Permission::Write, Permission::Events],
                Format::Uint8,
                CharacteristicValue::Uint8(1),
            ),
            characteristic(
                iid + 6,
                CHAR_SELECTED_RTP_STREAM_CONFIGURATION,
                vec![Permission::Read, Permission::Write],
                Format::Tlv8,
                CharacteristicValue::Bytes(Vec::new()),
            ),
            characteristic(
                iid + 7,
                CHAR_SETUP_ENDPOINTS,
                vec![Permission::Read, Permission::Write],
                Format::Tlv8,
                CharacteristicValue::Bytes(Vec::new()),
            ),
        ],
    }
}

fn encode_camera_capabilities(camera: &CameraConfig) -> Vec<u8> {
    let sensor_width = camera
        .video_tiers
        .iter()
        .map(|tier| tier.width)
        .max()
        .unwrap_or_default();
    let sensor_height = camera
        .video_tiers
        .iter()
        .map(|tier| tier.height)
        .max()
        .unwrap_or_default();

    let mut dimensions = Vec::new();
    let mut dimensions_writer = Tlv8Writer::new(&mut dimensions);
    dimensions_writer.push_u16(1, sensor_width);
    dimensions_writer.push_u16(2, sensor_height);

    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push_u8(1, 1);
    for (index, tier) in camera.video_tiers.iter().enumerate() {
        if index > 0 {
            writer.push_list_separator();
        }
        let mut stream = Vec::new();
        let mut stream_writer = Tlv8Writer::new(&mut stream);
        stream_writer.push(
            1,
            &video_configuration_uuid(camera.sensor_uuid, tier.identifier),
        );
        stream_writer.push_u8(2, tier.quality as u8);
        stream_writer.push_u16(3, tier.width);
        stream_writer.push_u16(4, tier.height);
        stream_writer.push_u8(5, tier.frame_rate);
        stream_writer.push_u32(6, tier.target_average_bitrate_kbps);
        stream_writer.push_u32(7, tier.target_average_bitrate_kbps);
        writer.push(2, &stream);
    }

    let mut sensor = Vec::new();
    Tlv8Writer::new(&mut sensor).push(1, &dimensions);
    let mut sensors = Vec::new();
    Tlv8Writer::new(&mut sensors).push(1, &sensor);
    writer.push(3, &sensors);
    value
}

fn video_configuration_uuid(sensor_uuid: [u8; 16], identifier: u32) -> [u8; 16] {
    let mut value = sensor_uuid;
    value[6] = (value[6] & 0x0f) | 0x50;
    value[8] = (value[8] & 0x3f) | 0x80;
    value[12..].copy_from_slice(&identifier.to_be_bytes());
    value
}

fn encode_video_tiers(camera: &CameraConfig) -> Vec<u8> {
    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push_u8(1, camera.video_codec as u8);
    writer.push_u8(2, camera.video_payload_type);
    for (index, tier) in camera.video_tiers.iter().enumerate() {
        if index > 0 {
            writer.push_list_separator();
        }
        let mut encoded_tier = Vec::new();
        let mut tier_writer = Tlv8Writer::new(&mut encoded_tier);
        tier_writer.push_u32(1, tier.identifier);
        tier_writer.push_u8(2, tier.quality as u8);
        tier_writer.push_u32(3, tier.target_average_bitrate_kbps);
        tier_writer.push_u16(4, tier.width);
        tier_writer.push_u16(5, tier.height);
        tier_writer.push_u8(6, tier.frame_rate);
        writer.push(3, &encoded_tier);
    }
    value
}

fn encode_audio_tier(camera: &CameraConfig) -> Vec<u8> {
    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push_u8(1, 3);
    writer.push_u8(2, camera.opus_payload_type);
    let mut tier = Vec::new();
    let mut tier_writer = Tlv8Writer::new(&mut tier);
    tier_writer.push_u32(1, camera.audio_tier.identifier);
    tier_writer.push_u32(2, camera.audio_tier.target_average_bitrate_bps);
    tier_writer.push_u8(3, 4);
    tier_writer.push_u8(4, 2);
    tier_writer.push_u8(5, 20);
    tier_writer.push_u8(6, 1);
    writer.push(3, &tier);
    value
}

fn encode_legacy_video_configuration() -> Vec<u8> {
    let mut parameters = Vec::new();
    let mut parameters_writer = Tlv8Writer::new(&mut parameters);
    parameters_writer.push_u8(1, 1);
    for level in [0, 1, 2] {
        if level > 0 {
            parameters_writer.push_list_separator();
        }
        parameters_writer.push_u8(2, level);
    }
    parameters_writer.push_u8(3, 0);

    let mut codec = Vec::new();
    let mut codec_writer = Tlv8Writer::new(&mut codec);
    codec_writer.push_u8(1, 0);
    codec_writer.push(2, &parameters);
    for (index, (width, height, frame_rate)) in [
        (1920, 1080, 30),
        (1280, 720, 30),
        (640, 360, 30),
        (320, 240, 15),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            codec_writer.push_list_separator();
        }
        let mut attributes = Vec::new();
        let mut attributes_writer = Tlv8Writer::new(&mut attributes);
        attributes_writer.push_u16(1, width);
        attributes_writer.push_u16(2, height);
        attributes_writer.push_u8(3, frame_rate);
        codec_writer.push(3, &attributes);
    }

    let mut value = Vec::new();
    Tlv8Writer::new(&mut value).push(1, &codec);
    value
}

fn encode_legacy_audio_configuration() -> Vec<u8> {
    let mut parameters = Vec::new();
    let mut parameters_writer = Tlv8Writer::new(&mut parameters);
    parameters_writer.push_u8(1, 1);
    parameters_writer.push_u8(2, 0);
    parameters_writer.push_u8(3, 1);
    parameters_writer.push_list_separator();
    parameters_writer.push_u8(3, 2);
    parameters_writer.push_u8(4, 20);

    let mut codec = Vec::new();
    let mut codec_writer = Tlv8Writer::new(&mut codec);
    codec_writer.push_u8(1, 3);
    codec_writer.push(2, &parameters);

    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push(1, &codec);
    writer.push_u8(2, 0);
    value
}

fn characteristic(
    iid: u64,
    characteristic_type: &'static str,
    perms: Vec<Permission>,
    format: Format,
    value: CharacteristicValue,
) -> Characteristic {
    let value = if perms.contains(&Permission::Read) {
        Some(value)
    } else {
        None
    };
    Characteristic {
        iid,
        characteristic_type,
        perms,
        format,
        value,
    }
}

fn readable_string(iid: u64, characteristic_type: &'static str, value: &str) -> Characteristic {
    characteristic(
        iid,
        characteristic_type,
        vec![Permission::Read],
        Format::String,
        CharacteristicValue::String(value.to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn information(name: &str) -> AccessoryInformation {
        AccessoryInformation {
            name: name.to_owned(),
            manufacturer: "KeepPeek".to_owned(),
            model: "Bridge".to_owned(),
            serial_number: format!("serial-{name}"),
            firmware_revision: "0.1.0".to_owned(),
        }
    }

    fn camera() -> CameraConfig {
        CameraConfig {
            information: information("Front Door"),
            sensor_uuid: [7; 16],
            video_codec: VideoCodec::H265,
            video_payload_type: 99,
            video_tiers: vec![
                VideoTier {
                    identifier: 1,
                    quality: VideoQuality::High,
                    target_average_bitrate_kbps: 1700,
                    width: 1920,
                    height: 1080,
                    frame_rate: 30,
                },
                VideoTier {
                    identifier: 2,
                    quality: VideoQuality::Medium,
                    target_average_bitrate_kbps: 768,
                    width: 1280,
                    height: 720,
                    frame_rate: 30,
                },
                VideoTier {
                    identifier: 3,
                    quality: VideoQuality::Low,
                    target_average_bitrate_kbps: 180,
                    width: 640,
                    height: 360,
                    frame_rate: 15,
                },
            ],
            opus_payload_type: 111,
            audio_tier: AudioTier {
                identifier: 1,
                target_average_bitrate_bps: 24_000,
            },
        }
    }

    #[test]
    fn serializes_standalone_camera_and_required_webrtc_service() {
        let database = AccessoryDatabase::camera(camera()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&database.to_json().unwrap()).unwrap();

        assert_eq!(database.len(), 1);
        let camera_json = &json["accessories"][0];
        assert_eq!(camera_json["aid"], 1);
        let service = camera_json["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|service| service["type"] == SERVICE_CAMERA_WEBRTC)
            .unwrap();
        let types: BTreeSet<&str> = service["characteristics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|characteristic| characteristic["type"].as_str().unwrap())
            .collect();
        for required in [
            CHAR_STREAMING_ENABLED,
            CHAR_WEBRTC_SOLICIT_OFFER,
            CHAR_WEBRTC_PROVIDE_ANSWER,
            CHAR_WEBRTC_STREAMING_CONTROL,
            CHAR_WEBRTC_ACTIVE_SESSIONS,
            CHAR_WEBRTC_REOFFER,
            CHAR_WEBRTC_UPDATE_SESSION,
            CHAR_WEBRTC_VIDEO_TIERS,
            CHAR_WEBRTC_AUDIO_TIERS,
            CHAR_SENSOR_UUID,
        ] {
            assert!(types.contains(required), "missing {required}");
        }

        let capabilities = camera_json["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|service| service["type"] == SERVICE_CAMERA_CAPABILITIES)
            .unwrap();
        assert_eq!(capabilities["iid"], 19);
        assert_eq!(capabilities["characteristics"][0]["value"], "17.99");
        let encoded = STANDARD
            .decode(
                capabilities["characteristics"][1]["value"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let top_level = crate::tlv8::Tlv8Map::parse(&encoded).unwrap();
        assert_eq!(top_level.get_u8(1).unwrap(), Some(1));
        let streams = top_level
            .items()
            .iter()
            .filter(|(item_type, _)| *item_type == 2)
            .collect::<Vec<_>>();
        assert_eq!(streams.len(), 3);
        for (_, stream) in streams {
            let stream = crate::tlv8::Tlv8Map::parse(stream).unwrap();
            assert_eq!(stream.get_unique(1).unwrap().unwrap().len(), 16);
            assert!(stream.get_unique(2).unwrap().is_some());
            assert!(stream.get_unique(3).unwrap().is_some());
            assert!(stream.get_unique(4).unwrap().is_some());
            assert!(stream.get_unique(5).unwrap().is_some());
            assert!(stream.get_unique(6).unwrap().is_some());
            assert!(stream.get_unique(7).unwrap().is_some());
        }
        let sensors =
            crate::tlv8::Tlv8Map::parse(top_level.get_unique(3).unwrap().unwrap()).unwrap();
        let sensor = crate::tlv8::Tlv8Map::parse(sensors.get_unique(1).unwrap().unwrap()).unwrap();
        assert_eq!(sensor.items().len(), 1);
        let dimensions =
            crate::tlv8::Tlv8Map::parse(sensor.get_unique(1).unwrap().unwrap()).unwrap();
        assert_eq!(
            dimensions.get_unique(1).unwrap(),
            Some(1920_u16.to_le_bytes().as_slice())
        );
        assert_eq!(
            dimensions.get_unique(2).unwrap(),
            Some(1080_u16.to_le_bytes().as_slice())
        );
        let video_tiers = encode_video_tiers(&camera());
        assert_eq!(
            crate::tlv8::Tlv8Map::parse(&video_tiers)
                .unwrap()
                .items()
                .iter()
                .filter(|(item_type, value)| *item_type == 0 && value.is_empty())
                .count(),
            2
        );
        let operating_mode = camera_json["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|service| service["type"] == SERVICE_CAMERA_GLOBAL_OPERATING_MODE)
            .unwrap();
        let operating_mode_types: BTreeSet<&str> = operating_mode["characteristics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|characteristic| characteristic["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            operating_mode_types,
            BTreeSet::from([
                CHAR_HOMEKIT_CAMERA_ACTIVE,
                CHAR_STREAMING_ENABLED,
                CHAR_CAMERA_OPERATING_MODE_INDICATOR,
            ])
        );

        let services = camera_json["services"].as_array().unwrap();
        assert_eq!(
            services
                .iter()
                .filter(|service| service["type"] == SERVICE_CAMERA_MULTI_TIER_RTP)
                .count(),
            0
        );
        assert_eq!(
            services
                .iter()
                .filter(|service| service["type"] == SERVICE_CAMERA_RTP_STREAM_MANAGEMENT)
                .count(),
            1
        );
        assert!(
            services
                .iter()
                .any(|service| service["type"] == SERVICE_MICROPHONE)
        );

        for accessory in json["accessories"].as_array().unwrap() {
            for service in accessory["services"].as_array().unwrap() {
                for characteristic in service["characteristics"].as_array().unwrap() {
                    let readable = characteristic["perms"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|permission| permission == "pr");
                    assert_eq!(
                        characteristic.get("value").is_some(),
                        readable,
                        "characteristic {} has invalid value visibility",
                        characteristic["type"]
                    );
                }
            }
        }
    }

    #[test]
    fn enforces_required_quality_tiers() {
        let mut camera = camera();
        camera.video_tiers.pop();

        assert_eq!(
            AccessoryDatabase::camera(camera),
            Err(ModelError::InvalidVideoTierCount(2))
        );
    }

    #[test]
    fn serializes_baseline_camera_without_preview_services() {
        let database = AccessoryDatabase::legacy_camera(camera()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&database.to_json().unwrap()).unwrap();
        let services = json["accessories"][0]["services"].as_array().unwrap();

        assert_eq!(
            services
                .iter()
                .filter(|service| service["type"] == SERVICE_CAMERA_RTP_STREAM_MANAGEMENT)
                .count(),
            2
        );
        assert!(
            services
                .iter()
                .any(|service| service["type"] == SERVICE_MICROPHONE)
        );
        for unsupported in [
            SERVICE_CAMERA_CAPABILITIES,
            SERVICE_CAMERA_MULTI_TIER_RTP,
            SERVICE_CAMERA_GLOBAL_OPERATING_MODE,
            SERVICE_CAMERA_WEBRTC,
        ] {
            assert!(
                services
                    .iter()
                    .all(|service| service["type"] != unsupported)
            );
        }
    }

    #[test]
    fn legacy_supported_video_stays_h264() {
        let encoded = encode_legacy_video_configuration();
        let map = crate::tlv8::Tlv8Map::parse(&encoded).unwrap();
        let codec = crate::tlv8::Tlv8Map::parse(map.get_unique(1).unwrap().unwrap()).unwrap();
        assert_eq!(codec.get_u8(1).unwrap(), Some(0));
    }
}
