//! Parses legacy HomeKit RTP camera negotiation.
//!
//! The types model Setup Endpoints and Selected RTP Stream Configuration TLV8
//! values. Network sockets, SRTP transport, and media processes remain owned by
//! the caller.

use crate::tlv8::{Error as Tlv8Error, Tlv8Map, Tlv8Writer};
use std::{error::Error as StdError, fmt, net::IpAddr};

const SESSION_IDENTIFIER: u8 = 1;
const STATUS: u8 = 2;
const ADDRESS: u8 = 3;
const VIDEO_CRYPTO: u8 = 4;
const AUDIO_CRYPTO: u8 = 5;
const VIDEO_SSRC: u8 = 6;
const AUDIO_SSRC: u8 = 7;

/// The 16-byte identifier shared by one legacy camera stream negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LegacySessionId([u8; 16]);

impl LegacySessionId {
    /// Creates an identifier from its HAP wire representation.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the HAP wire representation.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// RTP endpoints for the video and audio streams on one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRtpAddress {
    /// Device address reachable by its peer.
    pub ip: IpAddr,
    /// UDP port carrying the video RTP and RTCP stream.
    pub video_port: u16,
    /// UDP port carrying the audio RTP and RTCP stream.
    pub audio_port: u16,
}

/// AES-128 SRTP key material negotiated for one media stream.
#[derive(Clone, PartialEq, Eq)]
pub struct LegacySrtpParameters {
    /// The 16-byte AES master key.
    pub master_key: [u8; 16],
    /// The 14-byte SRTP master salt.
    pub master_salt: [u8; 14],
}

impl fmt::Debug for LegacySrtpParameters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LegacySrtpParameters")
            .field("master_key", &"[redacted; 16]")
            .field("master_salt", &"[redacted; 14]")
            .finish()
    }
}

impl LegacySrtpParameters {
    /// Concatenates the key and salt for an SRTP inline-key parameter.
    pub fn inline_key(&self) -> [u8; 30] {
        let mut value = [0_u8; 30];
        value[..16].copy_from_slice(&self.master_key);
        value[16..].copy_from_slice(&self.master_salt);
        value
    }
}

/// Controller parameters decoded from a Setup Endpoints write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupEndpointsRequest {
    /// Negotiated stream session identifier.
    pub session_id: LegacySessionId,
    /// Controller media endpoints.
    pub controller: LegacyRtpAddress,
    /// Key material for accessory-to-controller video.
    pub video_srtp: LegacySrtpParameters,
    /// Key material for accessory-to-controller audio.
    pub audio_srtp: LegacySrtpParameters,
}

/// Accessory parameters encoded for a Setup Endpoints response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupEndpointsResponse {
    /// Negotiated stream session identifier.
    pub session_id: LegacySessionId,
    /// Accessory media endpoints.
    pub accessory: LegacyRtpAddress,
    /// Key material for controller-to-accessory video traffic.
    pub video_srtp: LegacySrtpParameters,
    /// Key material for controller-to-accessory audio traffic.
    pub audio_srtp: LegacySrtpParameters,
    /// Accessory video synchronization source identifier.
    pub video_ssrc: u32,
    /// Accessory audio synchronization source identifier.
    pub audio_ssrc: u32,
}

/// Controller command carried by Selected RTP Stream Configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyStreamCommand {
    /// Releases the prepared stream session.
    End,
    /// Starts media delivery.
    Start,
    /// Pauses media delivery while retaining the session.
    Suspend,
    /// Resumes media delivery with the retained configuration.
    Resume,
    /// Applies new media parameters to the active session.
    Reconfigure,
}

/// H.264 profile selected by the HomeKit controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyH264Profile {
    /// Constrained Baseline profile.
    ConstrainedBaseline,
    /// Main profile.
    Main,
    /// High profile.
    High,
}

/// H.264 level selected by the HomeKit controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyH264Level {
    /// H.264 Level 3.1.
    Level31,
    /// H.264 Level 3.2.
    Level32,
    /// H.264 Level 4.0.
    Level40,
}

/// Selected stream command and optional video parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedStreamConfiguration {
    /// Session identifier from Setup Endpoints.
    pub session_id: LegacySessionId,
    /// Requested session transition.
    pub command: LegacyStreamCommand,
    /// Selected video parameters, present for start and reconfigure commands.
    pub video: Option<LegacyVideoParameters>,
}

/// Controller-selected RTP output parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyVideoParameters {
    /// Codec written by the controller: 0 = H.264, 1 = reserved H.265.
    pub codec: u8,
    /// Selected H.264 profile.
    pub profile: LegacyH264Profile,
    /// Selected H.264 level.
    pub level: LegacyH264Level,
    /// Dynamic RTP payload type.
    pub payload_type: u8,
    /// Video synchronization source identifier.
    pub ssrc: u32,
    /// Maximum encoded bitrate in kilobits per second.
    pub maximum_bitrate_kbps: u16,
    /// Minimum RTCP sender-report interval in seconds.
    pub rtcp_interval_seconds: f32,
    /// Maximum RTP packet size when supplied by the controller.
    pub maximum_mtu: Option<u16>,
    /// Selected frame width in pixels.
    pub width: u16,
    /// Selected frame height in pixels.
    pub height: u16,
    /// Selected maximum frame rate.
    pub frame_rate: u8,
}

/// Failure while decoding legacy HomeKit camera TLV8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyRtpError {
    Tlv8(Tlv8Error),
    Missing(&'static str),
    InvalidLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    InvalidIpAddress,
    UnsupportedIpVersion(u8),
    UnsupportedCryptoSuite(u8),
    UnsupportedVideoCodec(u8),
    UnsupportedH264Profile(u8),
    UnsupportedH264Level(u8),
    UnsupportedPacketizationMode(u8),
    UnsupportedCommand(u8),
}

impl fmt::Display for LegacyRtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tlv8(error) => error.fmt(f),
            Self::Missing(field) => write!(f, "missing {field}"),
            Self::InvalidLength {
                field,
                expected,
                actual,
            } => write!(f, "{field} has {actual} bytes; expected {expected}"),
            Self::InvalidIpAddress => f.write_str("invalid controller IP address"),
            Self::UnsupportedIpVersion(version) => {
                write!(f, "unsupported controller IP version {version}")
            }
            Self::UnsupportedCryptoSuite(suite) => {
                write!(f, "unsupported SRTP crypto suite {suite}")
            }
            Self::UnsupportedVideoCodec(codec) => {
                write!(f, "unsupported legacy video codec {codec}")
            }
            Self::UnsupportedH264Profile(profile) => {
                write!(f, "unsupported H.264 profile {profile}")
            }
            Self::UnsupportedH264Level(level) => {
                write!(f, "unsupported H.264 level {level}")
            }
            Self::UnsupportedPacketizationMode(mode) => {
                write!(f, "unsupported H.264 packetization mode {mode}")
            }
            Self::UnsupportedCommand(command) => {
                write!(f, "unsupported legacy stream command {command}")
            }
        }
    }
}

impl StdError for LegacyRtpError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Tlv8(error) => Some(error),
            _ => None,
        }
    }
}

impl From<Tlv8Error> for LegacyRtpError {
    fn from(value: Tlv8Error) -> Self {
        Self::Tlv8(value)
    }
}

/// Decodes a controller Setup Endpoints write.
pub fn decode_setup_endpoints(value: &[u8]) -> Result<SetupEndpointsRequest, LegacyRtpError> {
    let map = Tlv8Map::parse(value)?;
    Ok(SetupEndpointsRequest {
        session_id: LegacySessionId(exact(
            required(&map, SESSION_IDENTIFIER, "session identifier")?,
            "session identifier",
        )?),
        controller: decode_address(required(&map, ADDRESS, "controller address")?)?,
        video_srtp: decode_srtp(required(&map, VIDEO_CRYPTO, "video SRTP parameters")?)?,
        audio_srtp: decode_srtp(required(&map, AUDIO_CRYPTO, "audio SRTP parameters")?)?,
    })
}

/// Encodes a successful accessory Setup Endpoints response.
pub fn encode_setup_endpoints_response(response: &SetupEndpointsResponse) -> Vec<u8> {
    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push(SESSION_IDENTIFIER, response.session_id.as_bytes());
    writer.push_u8(STATUS, 0);
    writer.push(ADDRESS, &encode_address(&response.accessory));
    writer.push(VIDEO_CRYPTO, &encode_srtp(&response.video_srtp));
    writer.push(AUDIO_CRYPTO, &encode_srtp(&response.audio_srtp));
    writer.push_u32(VIDEO_SSRC, response.video_ssrc);
    writer.push_u32(AUDIO_SSRC, response.audio_ssrc);
    value
}

/// Decodes a Selected RTP Stream Configuration write.
pub fn decode_selected_stream(value: &[u8]) -> Result<SelectedStreamConfiguration, LegacyRtpError> {
    let map = Tlv8Map::parse(value)?;
    let control = Tlv8Map::parse(required(&map, 1, "session control")?)?;
    let session_id = LegacySessionId(exact(
        required(&control, 1, "session identifier")?,
        "session identifier",
    )?);
    let command = match required_u8(&control, 2, "stream command")? {
        0 => LegacyStreamCommand::End,
        1 => LegacyStreamCommand::Start,
        2 => LegacyStreamCommand::Suspend,
        3 => LegacyStreamCommand::Resume,
        4 => LegacyStreamCommand::Reconfigure,
        command => return Err(LegacyRtpError::UnsupportedCommand(command)),
    };
    let video = map
        .get_unique(2)?
        .map(decode_video_configuration)
        .transpose()?;
    Ok(SelectedStreamConfiguration {
        session_id,
        command,
        video,
    })
}

fn decode_address(value: &[u8]) -> Result<LegacyRtpAddress, LegacyRtpError> {
    let map = Tlv8Map::parse(value)?;
    let version = required_u8(&map, 1, "IP version")?;
    let ip = std::str::from_utf8(required(&map, 2, "IP address")?)
        .ok()
        .and_then(|value| value.parse::<IpAddr>().ok())
        .ok_or(LegacyRtpError::InvalidIpAddress)?;
    if !matches!((version, ip), (0, IpAddr::V4(_)) | (1, IpAddr::V6(_))) {
        return Err(LegacyRtpError::UnsupportedIpVersion(version));
    }
    Ok(LegacyRtpAddress {
        ip,
        video_port: decode_u16(required(&map, 3, "video RTP port")?, "video RTP port")?,
        audio_port: decode_u16(required(&map, 4, "audio RTP port")?, "audio RTP port")?,
    })
}

fn encode_address(address: &LegacyRtpAddress) -> Vec<u8> {
    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push_u8(1, u8::from(address.ip.is_ipv6()));
    writer.push_str(2, &address.ip.to_string());
    writer.push_u16(3, address.video_port);
    writer.push_u16(4, address.audio_port);
    value
}

fn decode_srtp(value: &[u8]) -> Result<LegacySrtpParameters, LegacyRtpError> {
    let map = Tlv8Map::parse(value)?;
    let suite = required_u8(&map, 1, "SRTP crypto suite")?;
    if suite != 0 {
        return Err(LegacyRtpError::UnsupportedCryptoSuite(suite));
    }
    Ok(LegacySrtpParameters {
        master_key: exact(required(&map, 2, "SRTP master key")?, "SRTP master key")?,
        master_salt: exact(required(&map, 3, "SRTP master salt")?, "SRTP master salt")?,
    })
}

fn encode_srtp(parameters: &LegacySrtpParameters) -> Vec<u8> {
    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push_u8(1, 0);
    writer.push(2, &parameters.master_key);
    writer.push(3, &parameters.master_salt);
    value
}

fn decode_video_configuration(value: &[u8]) -> Result<LegacyVideoParameters, LegacyRtpError> {
    let map = Tlv8Map::parse(value)?;
    let codec = required_u8(&map, 1, "video codec")?;
    if !matches!(codec, 0 | 1) {
        return Err(LegacyRtpError::UnsupportedVideoCodec(codec));
    }
    let codec_parameters = match map.get_unique(2)? {
        Some(value) => Tlv8Map::parse(value)?,
        None if codec == 1 => Tlv8Map::parse(&[])?,
        None => return Err(LegacyRtpError::Missing("video codec parameters")),
    };
    let profile = match codec_parameters.get_u8(1)? {
        Some(0) => LegacyH264Profile::ConstrainedBaseline,
        Some(1) | None => LegacyH264Profile::Main,
        Some(2) => LegacyH264Profile::High,
        Some(profile) => return Err(LegacyRtpError::UnsupportedH264Profile(profile)),
    };
    let level = match codec_parameters.get_u8(2)? {
        Some(0) => LegacyH264Level::Level31,
        Some(1) => LegacyH264Level::Level32,
        Some(2) | None => LegacyH264Level::Level40,
        Some(level) => return Err(LegacyRtpError::UnsupportedH264Level(level)),
    };
    let packetization_mode = codec_parameters.get_u8(3)?.unwrap_or(0);
    if packetization_mode != 0 {
        return Err(LegacyRtpError::UnsupportedPacketizationMode(
            packetization_mode,
        ));
    }
    let attributes = Tlv8Map::parse(required(&map, 3, "video attributes")?)?;
    let rtp = Tlv8Map::parse(required(&map, 4, "video RTP parameters")?)?;
    let rtcp = required(&rtp, 4, "minimum RTCP interval")?;
    if rtcp.len() != 4 {
        return Err(LegacyRtpError::InvalidLength {
            field: "minimum RTCP interval",
            expected: 4,
            actual: rtcp.len(),
        });
    }
    Ok(LegacyVideoParameters {
        codec,
        profile,
        level,
        payload_type: required_u8(&rtp, 1, "video payload type")?,
        ssrc: decode_u32(required(&rtp, 2, "video SSRC")?, "video SSRC")?,
        maximum_bitrate_kbps: decode_u16(
            required(&rtp, 3, "maximum video bitrate")?,
            "maximum video bitrate",
        )?,
        rtcp_interval_seconds: f32::from_le_bytes(rtcp.try_into().expect("checked length")),
        maximum_mtu: rtp
            .get_unique(5)?
            .map(|value| decode_u16(value, "maximum MTU"))
            .transpose()?,
        width: decode_u16(required(&attributes, 1, "video width")?, "video width")?,
        height: decode_u16(required(&attributes, 2, "video height")?, "video height")?,
        frame_rate: required_u8(&attributes, 3, "video frame rate")?,
    })
}

fn required<'a>(
    map: &'a Tlv8Map,
    item_type: u8,
    field: &'static str,
) -> Result<&'a [u8], LegacyRtpError> {
    map.get_unique(item_type)?
        .ok_or(LegacyRtpError::Missing(field))
}

fn required_u8(map: &Tlv8Map, item_type: u8, field: &'static str) -> Result<u8, LegacyRtpError> {
    map.get_u8(item_type)?.ok_or(LegacyRtpError::Missing(field))
}

fn exact<const N: usize>(value: &[u8], field: &'static str) -> Result<[u8; N], LegacyRtpError> {
    value.try_into().map_err(|_| LegacyRtpError::InvalidLength {
        field,
        expected: N,
        actual: value.len(),
    })
}

fn decode_u16(value: &[u8], field: &'static str) -> Result<u16, LegacyRtpError> {
    Ok(u16::from_le_bytes(exact(value, field)?))
}

fn decode_u32(value: &[u8], field: &'static str) -> Result<u32, LegacyRtpError> {
    Ok(u32::from_le_bytes(exact(value, field)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(output: &mut Vec<u8>, item_type: u8, value: &[u8]) {
        Tlv8Writer::new(output).push(item_type, value);
    }

    fn srtp(seed: u8) -> LegacySrtpParameters {
        LegacySrtpParameters {
            master_key: [seed; 16],
            master_salt: [seed.wrapping_add(1); 14],
        }
    }

    #[test]
    fn decodes_setup_endpoints_and_encodes_response() {
        let mut address = Vec::new();
        let mut writer = Tlv8Writer::new(&mut address);
        writer.push_u8(1, 0);
        writer.push_str(2, "192.0.2.20");
        writer.push_u16(3, 50_000);
        writer.push_u16(4, 50_001);
        let mut request = Vec::new();
        let mut writer = Tlv8Writer::new(&mut request);
        writer.push(1, &[7; 16]);
        writer.push(3, &address);
        writer.push(4, &encode_srtp(&srtp(3)));
        writer.push(5, &encode_srtp(&srtp(5)));

        let decoded = decode_setup_endpoints(&request).unwrap();

        assert_eq!(decoded.session_id, LegacySessionId::new([7; 16]));
        assert_eq!(
            decoded.controller.ip,
            "192.0.2.20".parse::<IpAddr>().unwrap()
        );
        assert_eq!(decoded.controller.video_port, 50_000);
        assert_eq!(decoded.video_srtp, srtp(3));

        let response = encode_setup_endpoints_response(&SetupEndpointsResponse {
            session_id: decoded.session_id,
            accessory: LegacyRtpAddress {
                ip: "192.0.2.10".parse().unwrap(),
                video_port: 40_000,
                audio_port: 40_001,
            },
            video_srtp: srtp(9),
            audio_srtp: srtp(11),
            video_ssrc: 0x1122_3344,
            audio_ssrc: 0x5566_7788,
        });
        let map = Tlv8Map::parse(&response).unwrap();
        assert_eq!(map.get_u8(2).unwrap(), Some(0));
        assert_eq!(map.get_unique(1).unwrap(), Some([7; 16].as_slice()));
        assert_eq!(
            map.get_unique(6).unwrap(),
            Some(0x1122_3344_u32.to_le_bytes().as_slice())
        );
    }

    #[test]
    fn decodes_selected_video_start() {
        let mut control = Vec::new();
        let mut writer = Tlv8Writer::new(&mut control);
        writer.push(1, &[7; 16]);
        writer.push_u8(2, 1);
        let mut attributes = Vec::new();
        let mut writer = Tlv8Writer::new(&mut attributes);
        writer.push_u16(1, 1280);
        writer.push_u16(2, 720);
        writer.push_u8(3, 30);
        let mut rtp = Vec::new();
        let mut writer = Tlv8Writer::new(&mut rtp);
        writer.push_u8(1, 99);
        writer.push_u32(2, 0x1122_3344);
        writer.push_u16(3, 800);
        writer.push(4, &0.5_f32.to_le_bytes());
        writer.push_u16(5, 1200);
        let mut video = Vec::new();
        let mut writer = Tlv8Writer::new(&mut video);
        writer.push_u8(1, 0);
        writer.push(2, &[1, 1, 1, 2, 1, 0, 3, 1, 0]);
        writer.push(3, &attributes);
        writer.push(4, &rtp);
        let mut selected = Vec::new();
        push(&mut selected, 1, &control);
        push(&mut selected, 2, &video);

        let decoded = decode_selected_stream(&selected).unwrap();

        assert_eq!(decoded.command, LegacyStreamCommand::Start);
        assert_eq!(decoded.session_id, LegacySessionId::new([7; 16]));
        assert_eq!(
            decoded.video,
            Some(LegacyVideoParameters {
                codec: 0,
                profile: LegacyH264Profile::Main,
                level: LegacyH264Level::Level31,
                payload_type: 99,
                ssrc: 0x1122_3344,
                maximum_bitrate_kbps: 800,
                rtcp_interval_seconds: 0.5,
                maximum_mtu: Some(1200),
                width: 1280,
                height: 720,
                frame_rate: 30,
            })
        );
    }

    #[test]
    fn decodes_reserved_h265_selected_stream() {
        let mut control = Vec::new();
        let mut writer = Tlv8Writer::new(&mut control);
        writer.push(1, &[7; 16]);
        writer.push_u8(2, 1);
        let mut attributes = Vec::new();
        let mut writer = Tlv8Writer::new(&mut attributes);
        writer.push_u16(1, 1280);
        writer.push_u16(2, 720);
        writer.push_u8(3, 30);
        let mut rtp = Vec::new();
        let mut writer = Tlv8Writer::new(&mut rtp);
        writer.push_u8(1, 99);
        writer.push_u32(2, 0x1122_3344);
        writer.push_u16(3, 800);
        writer.push(4, &0.5_f32.to_le_bytes());
        let mut video = Vec::new();
        let mut writer = Tlv8Writer::new(&mut video);
        writer.push_u8(1, 1);
        writer.push(3, &attributes);
        writer.push(4, &rtp);
        let mut selected = Vec::new();
        push(&mut selected, 1, &control);
        push(&mut selected, 2, &video);

        let decoded = decode_selected_stream(&selected).unwrap();

        assert_eq!(decoded.video.unwrap().codec, 1);
    }

    #[test]
    fn debug_redacts_srtp_key_material() {
        let parameters = srtp(42);

        let debug = format!("{parameters:?}");

        assert!(debug.contains("LegacySrtpParameters"));
        assert!(!debug.contains("42, 42"));
        assert!(!debug.contains("43, 43"));
    }
}
