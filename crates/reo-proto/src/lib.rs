pub mod alarm;
pub mod auth;
pub mod device;
pub mod encryption;
pub mod error;
pub mod framing;
pub mod header;
pub mod magic;
pub mod media;
pub mod network_cfg;
pub mod notification;
pub mod ptz;
pub mod recording;
pub mod session;
pub mod stream;
pub mod talk;
pub mod udp;
pub mod video_cfg;
pub mod xml;

pub use alarm::{AlarmCommand, AlertEvent};
pub use auth::{CameraIdentity, EncryptionMode, LoginParams, LoginResult, NonceInfo};
pub use device::{DeviceCommand, DeviceEvent};
pub use error::BcError;
pub use framing::{RawMessage, ReadBuffer};
pub use header::PacketHeader;
pub use media::{
    AudioCodec, AudioFrameRef, BcTimestamp, MediaFrame, MediaFrameIter, MediaMagic, StreamMetadata,
    VideoCodec, VideoFrameRef,
};
pub use network_cfg::{NetworkCommand, NetworkEvent};
pub use notification::{NotificationCommand, NotificationEvent};
pub use ptz::{PtzCommand, PtzEvent};
pub use recording::{RecordingCommand, RecordingEvent};
pub use session::{
    BcSession, BcSessionConfig, Command, Event, Input, Output, Role, SessionState, SessionStats,
};
pub use stream::{SnapshotRequest, StreamRequest, StreamStop, StreamSubscription, StreamType};
pub use talk::{
    ImaAdpcmEncoder, TalkAbility, TalkAudioProfile, TalkCommand, TalkConfig, TalkEvent,
};
pub use udp::{
    BcUdpConfig, BcUdpConnection, BcUdpDiscovery, BcUdpDiscoveryConfig, BcUdpDiscoveryOutput,
    BcUdpOutput, BcUdpPacket, BcUdpTransport, UdpAck, UdpData, UdpDiscovery,
};
pub use video_cfg::{CompressionSettings, VideoCommand, VideoEvent};

/// Default TCP receive buffer size (512 KiB).
pub const TCP_RECV_BUF_SIZE: usize = 512 * 1024;

/// Default TCP send buffer size (64 KiB).
pub const TCP_SEND_BUF_SIZE: usize = 64 * 1024;

/// Maximum XML body size (8 KiB).
pub const MAX_XML_BODY: usize = 8 * 1024;

/// Initial caller-owned output buffer size for media events (256 KiB).
///
/// `BcSession::poll_output` reports `BufferTooSmall` without consuming an event,
/// so callers can grow this buffer when a larger frame arrives.
pub const DEFAULT_MEDIA_OUTPUT_BUFFER_SIZE: usize = 256 * 1024;

/// Maximum encoded video frame size accepted from a camera (4 MiB).
///
/// Physical-camera recordings contain encoded frames up to 2,292,974 bytes.
/// This bound uses the next binary size boundary and prevents unbounded
/// allocation from a camera-declared frame length.
pub const MAX_MEDIA_FRAME: usize = 4 * 1024 * 1024;

/// Maximum JPEG snapshot size accepted from a camera (16 MiB).
///
/// High-resolution battery cameras can produce JPEG snapshots above the
/// previous 12 MiB limit. This bound admits those images while preventing a
/// malformed camera reply from retaining unbounded memory.
pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

/// Capacity for username strings.
pub const USERNAME_CAP: usize = 32;

/// Capacity for password / credential hash strings.
pub const PASSWORD_CAP: usize = 64;

/// Capacity for nonce strings.
pub const NONCE_CAP: usize = 64;

/// Capacity for camera model strings.
pub const MODEL_CAP: usize = 64;

/// Capacity for serial number strings.
pub const SERIAL_CAP: usize = 64;

/// Capacity for firmware version strings.
pub const FIRMWARE_CAP: usize = 64;

/// Login (legacy and modern use same ID, differ by class flags).
pub const COMMAND_LOGIN: u32 = 1;

/// Logout.
pub const COMMAND_LOGOUT: u32 = 2;

/// Video stream request / video+audio data (binary).
pub const COMMAND_STREAM: u32 = 3;

/// Alias: same ID for stream and media data.
pub const COMMAND_MEDIA_DATA: u32 = 3;

/// Stop stream transmission.
pub const COMMAND_PREVIEW_STOP: u32 = 4;

/// File open (recording playback).
pub const COMMAND_FILE_OPEN: u32 = 5;

/// File read (recording data, binary response).
pub const COMMAND_FILE_READ: u32 = 6;

/// File close (recording playback).
pub const COMMAND_FILE_CLOSE: u32 = 7;

/// Talk ability query.
pub const COMMAND_TALK_CAPABILITIES: u32 = 10;

/// Reset talkback state.
pub const COMMAND_TALK_RESET: u32 = 11;

/// PTZ control.
pub const COMMAND_PTZ: u32 = 18;

/// PTZ preset management.
pub const COMMAND_PTZ_PRESET: u32 = 19;

/// Reboot.
pub const COMMAND_REBOOT: u32 = 23;

/// Video input write (brightness, contrast, etc).
pub const COMMAND_VIDEO_INPUT_WRITE: u32 = 25;

/// Video input read.
pub const COMMAND_VIDEO_INPUT_READ: u32 = 26;

/// Start motion alarm reporting.
pub const COMMAND_START_MOTION_ALARM: u32 = 31;

/// Alarm event list (motion detection notifications).
pub const COMMAND_ALARM_EVENT_LIST: u32 = 33;

/// Email config read.
pub const COMMAND_EMAIL_READ: u32 = 42;

/// Email config write.
pub const COMMAND_EMAIL_WRITE: u32 = 43;

/// OSD channel name read.
pub const COMMAND_OSD_READ: u32 = 44;

/// OSD channel name write.
pub const COMMAND_OSD_WRITE: u32 = 45;

/// Motion detection config read.
pub const COMMAND_MOTION_DETECT_READ: u32 = 46;

/// Motion detection config write.
pub const COMMAND_MOTION_DETECT_WRITE: u32 = 47;

/// Shelter (privacy mask) read.
pub const COMMAND_SHELTER_READ: u32 = 52;

/// Shelter (privacy mask) write.
pub const COMMAND_SHELTER_WRITE: u32 = 53;

/// Recording config read.
pub const COMMAND_RECORD_CFG_READ: u32 = 54;

/// Recording config write.
pub const COMMAND_RECORD_CFG_WRITE: u32 = 55;

/// Compression read.
pub const COMMAND_COMPRESSION_READ: u32 = 56;

/// Compression write.
pub const COMMAND_COMPRESSION_WRITE: u32 = 57;

/// Ability support query.
pub const COMMAND_ABILITY_SUPPORT: u32 = 58;

/// User list query.
pub const COMMAND_ACCOUNT_DIRECTORY: u32 = 59;

/// Config file info (firmware upgrade info).
pub const COMMAND_CONFIG_FILE_INFO: u32 = 67;

/// IP config read.
pub const COMMAND_IP_READ: u32 = 76;

/// IP config write.
pub const COMMAND_IP_WRITE: u32 = 77;

/// Video input advanced (ISP settings).
pub const COMMAND_VIDEO_INPUT_ADVANCED: u32 = 78;

/// Version info query.
pub const COMMAND_FIRMWARE_DETAILS: u32 = 80;

/// Recording schedule read.
pub const COMMAND_RECORD_SCHEDULE_READ: u32 = 81;

/// Recording schedule write.
pub const COMMAND_RECORD_SCHEDULE_WRITE: u32 = 82;

/// Link type query (ethernet/wifi).
pub const COMMAND_LINK_TYPE: u32 = 93;

/// TCP keepalive uses the link type command with an empty payload.
pub const COMMAND_PING: u32 = COMMAND_LINK_TYPE;

/// System general (time, timezone, language).
pub const COMMAND_SYSTEM_SETTINGS: u32 = 104;

/// HDD info list query.
pub const COMMAND_HDD_INFO_LIST: u32 = 102;

/// JPEG snapshot request.
pub const COMMAND_SNAP: u32 = 109;

/// WiFi signal strength query.
pub const COMMAND_WIFI_SIGNAL: u32 = 115;

/// WiFi list (available networks) query.
pub const COMMAND_WIFI_LIST: u32 = 116;

/// Push notification token registration.
pub const COMMAND_PUSH_INFO: u32 = 124;

/// Video input extra settings.
pub const COMMAND_VIDEO_INPUT_EXTRA: u32 = 132;

/// RF alarm read.
pub const COMMAND_RF_ALARM: u32 = 133;

/// Record cover.
pub const COMMAND_RECORD_COVER: u32 = 138;

/// Email test.
pub const COMMAND_EMAIL_TEST: u32 = 141;

/// Stream info list query.
pub const COMMAND_STREAM_CATALOG: u32 = 146;

/// Ability info (detailed capabilities).
pub const COMMAND_CAPABILITY_DETAILS: u32 = 151;

/// PTZ preset list query.
pub const COMMAND_PTZ_PRESET_LIST: u32 = 190;

/// Ability support (ID 199).
pub const COMMAND_SUPPORT: u32 = 199;

/// Talk config.
pub const COMMAND_TALK_CONFIG: u32 = 201;

/// Talk audio transmit.
pub const COMMAND_TALK: u32 = 202;

/// RF alarm config write.
pub const COMMAND_RF_ALARM_CFG_WRITE: u32 = 204;

/// LED state read.
pub const COMMAND_LED_READ: u32 = 208;

/// LED state write.
pub const COMMAND_LED_WRITE: u32 = 209;

/// PIR info read.
pub const COMMAND_PIR_READ: u32 = 212;

/// PIR info write.
pub const COMMAND_PIR_WRITE: u32 = 213;

/// Email task write.
pub const COMMAND_EMAIL_TASK_WRITE: u32 = 216;

/// Email task read.
pub const COMMAND_EMAIL_TASK_READ: u32 = 217;

/// Push task read.
pub const COMMAND_PUSH_TASK_READ: u32 = 219;

/// Audio task read.
pub const COMMAND_AUDIO_TASK_READ: u32 = 232;

/// Camera-initiated Baichuan UDP keepalive.
pub const COMMAND_UDP_KEEP_ALIVE: u32 = 234;

/// Battery list query.
pub const COMMAND_BATTERY_LIST: u32 = 252;

/// Battery info query.
pub const COMMAND_BATTERY_INFO: u32 = 253;

/// 3G/4G cellular info query.
pub const COMMAND_CELLULAR_INFO: u32 = 255;

/// Audio play info.
pub const COMMAND_AUDIO_PLAY_INFO: u32 = 264;

/// Cloud bind info query.
pub const COMMAND_CLOUD_BIND_INFO: u32 = 268;

/// Recording search.
pub const COMMAND_RECORDING_SEARCH: u32 = 272;

/// Recording search by month.
pub const COMMAND_RECORDING_SEARCH_MONTH: u32 = 273;

/// Recording calendar (day bitmask).
pub const COMMAND_RECORDING_CALENDAR: u32 = 274;

/// Cloud login key query.
pub const COMMAND_CLOUD_LOGIN_KEY: u32 = 282;

/// Time config / sync.
pub const COMMAND_TIME_CFG: u32 = 287;

/// Floodlight manual control.
pub const COMMAND_FLOODLIGHT: u32 = 288;

/// Floodlight task read.
pub const COMMAND_FLOODLIGHT_TASK_READ: u32 = 290;

/// Floodlight status list.
pub const COMMAND_FLOODLIGHT_STATUS_LIST: u32 = 291;

/// PTZ zoom/focus read.
pub const COMMAND_PTZ_ZOOM_FOCUS: u32 = 294;

/// Start zoom/focus operation.
pub const COMMAND_START_ZOOM_FOCUS: u32 = 295;

/// AI config read.
pub const COMMAND_AI_CFG_READ: u32 = 299;

/// Record thumbnail.
pub const COMMAND_RECORD_THUMBNAIL: u32 = 298;

/// AI alarm read.
pub const COMMAND_AI_ALARM_READ: u32 = 342;

/// AI alarm write.
pub const COMMAND_AI_ALARM_WRITE: u32 = 343;

/// PTZ guard config.
pub const COMMAND_PTZ_GUARD: u32 = 433;

/// Floodlight task write.
pub const COMMAND_FLOODLIGHT_TASK_WRITE: u32 = 438;

/// Cover file open.
pub const COMMAND_COVER_FILE_OPEN: u32 = 458;

/// Cover file read.
pub const COMMAND_COVER_FILE_READ: u32 = 459;

/// Cover file close.
pub const COMMAND_COVER_FILE_CLOSE: u32 = 460;

/// Cover thumbnail.
pub const COMMAND_COVER_THUMBNAIL: u32 = 461;

/// Cover thumbnail V2.
pub const COMMAND_COVER_THUMBNAIL_V2: u32 = 462;

/// Siren control.
pub const COMMAND_SIREN_CONTROL: u32 = 547;

/// Auto-tracking coordinate push event.
pub const COMMAND_COORDINATE_INFO: u32 = 723;
