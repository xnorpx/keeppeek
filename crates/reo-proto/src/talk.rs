//! Two-way audio negotiation and ADPCM packet framing.

use crate::{
    error::BcError,
    header::PacketHeader,
    magic::{BC_CLASS_MODERN_EXT, make_status},
    media::MEDIA_MAGIC_ADPCM,
    xml::{self, XmlVisit},
};
use arrayvec::{ArrayString, ArrayVec};

const TALK_TEXT_CAP: usize = 32;
const TALK_MODE_CAP: usize = 4;
const TALK_PROFILE_CAP: usize = 8;
const ADPCM_BLOCK_HEADER_LEN: usize = 4;
const TALK_FRAME_HEADER_LEN: usize = 12;
const TALK_ADPCM_MARKER: u16 = 0x0100;

const IMA_INDEX_TABLE: [i8; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const IMA_STEP_TABLE: [i16; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// One audio format accepted by a camera's talkback service.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TalkAudioProfile {
    /// Camera audio codec name, such as `adpcm`.
    pub audio_type: ArrayString<TALK_TEXT_CAP>,
    /// Audio sample rate in hertz.
    pub sample_rate: u32,
    /// Bits per PCM sample before encoding.
    pub sample_precision: u32,
    /// PCM samples required in one encoded block.
    pub length_per_encoder: u32,
    /// Camera channel layout, such as `mono`.
    pub sound_track: ArrayString<TALK_TEXT_CAP>,
}

/// Audio capabilities reported by a camera.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TalkAbility {
    /// Supported duplex modes in camera preference order.
    pub duplex_modes: ArrayVec<ArrayString<TALK_TEXT_CAP>, TALK_MODE_CAP>,
    /// Supported audio stream modes in camera preference order.
    pub audio_stream_modes: ArrayVec<ArrayString<TALK_TEXT_CAP>, TALK_MODE_CAP>,
    /// Supported input audio profiles in camera preference order.
    pub audio_profiles: ArrayVec<TalkAudioProfile, TALK_PROFILE_CAP>,
}

impl TalkAbility {
    /// Select the preferred ADPCM configuration for `channel`.
    ///
    /// # Errors
    ///
    /// Returns `BcError::Protocol` when no complete ADPCM profile is available.
    pub fn select_adpcm(&self, channel: u8) -> Result<TalkConfig, BcError> {
        let profile = self
            .audio_profiles
            .iter()
            .find(|profile| {
                profile.audio_type.eq_ignore_ascii_case("adpcm")
                    && profile.sample_rate > 0
                    && profile.sample_precision > 0
                    && profile.length_per_encoder >= 2
                    && profile.length_per_encoder.is_multiple_of(2)
            })
            .cloned()
            .ok_or(BcError::Protocol(
                "camera does not advertise ADPCM talkback",
            ))?;
        let duplex = preferred_mode(&self.duplex_modes, "fullDuplex")?;
        let audio_stream_mode = preferred_mode(&self.audio_stream_modes, "speaker")?;
        Ok(TalkConfig {
            channel,
            duplex,
            audio_stream_mode,
            audio_profile: profile,
        })
    }
}

/// Negotiated parameters for an active talkback session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TalkConfig {
    /// Camera channel receiving talkback audio.
    pub channel: u8,
    /// Selected duplex mode.
    pub duplex: ArrayString<TALK_TEXT_CAP>,
    /// Selected camera audio stream mode.
    pub audio_stream_mode: ArrayString<TALK_TEXT_CAP>,
    /// Selected ADPCM input profile.
    pub audio_profile: TalkAudioProfile,
}

/// Commands for an established external talkback connection.
#[derive(Debug)]
pub enum TalkCommand {
    /// Request the camera's supported talkback profiles.
    QueryAbility { channel: u8 },
    /// Configure the selected camera talkback profile.
    Configure(TalkConfig),
    /// Send one IMA ADPCM block using the supplied block sequence number.
    SendAdpcm {
        channel: u8,
        sequence: u16,
        data: Vec<u8>,
    },
    /// Reset the camera's talkback state.
    Reset { channel: u8 },
}

impl TalkCommand {
    pub(crate) const fn channel(&self) -> u8 {
        match self {
            Self::QueryAbility { channel } | Self::Reset { channel } => *channel,
            Self::Configure(config) => config.channel,
            Self::SendAdpcm { channel, .. } => *channel,
        }
    }
}

/// Events emitted while negotiating a talkback session.
#[derive(Debug, Clone)]
pub enum TalkEvent {
    /// Camera-reported talkback profiles.
    Ability(Box<TalkAbility>),
    /// Camera accepted a talkback configuration.
    Configured,
    /// Camera acknowledged a talkback reset.
    Reset,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TalkResponseKind {
    Ability,
    Configured,
    Reset,
}

/// Stateful IMA ADPCM encoder for camera talkback blocks.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImaAdpcmEncoder {
    predictor: i16,
    index: u8,
}

impl ImaAdpcmEncoder {
    /// Encode an even number of PCM samples into one IMA ADPCM block.
    ///
    /// The output includes the four-byte predictor header required by the camera.
    ///
    /// # Errors
    ///
    /// Returns `BcError::Protocol` for an empty or odd sample count, and
    /// `BcError::BufferTooSmall` when `output` cannot hold the encoded block.
    pub fn encode_block(&mut self, samples: &[i16], output: &mut [u8]) -> Result<usize, BcError> {
        if samples.is_empty() || !samples.len().is_multiple_of(2) {
            return Err(BcError::Protocol(
                "IMA ADPCM blocks require a non-empty even sample count",
            ));
        }
        let encoded_len = ADPCM_BLOCK_HEADER_LEN + samples.len() / 2;
        if output.len() < encoded_len {
            return Err(BcError::BufferTooSmall {
                needed: encoded_len,
                available: output.len(),
            });
        }

        output[..2].copy_from_slice(&self.predictor.to_le_bytes());
        output[2] = self.index;
        output[3] = 0;
        for (index, sample_pair) in samples.chunks_exact(2).enumerate() {
            let first = self.encode_nibble(sample_pair[0]);
            let second = self.encode_nibble(sample_pair[1]);
            output[ADPCM_BLOCK_HEADER_LEN + index] = (first << 4) | second;
        }
        Ok(encoded_len)
    }

    fn encode_nibble(&mut self, sample: i16) -> u8 {
        let step = i32::from(IMA_STEP_TABLE[usize::from(self.index)]);
        let mut difference = i32::from(sample) - i32::from(self.predictor);
        let mut nibble = 0_u8;
        if difference < 0 {
            nibble |= 8;
            difference = -difference;
        }

        let mut delta = step >> 3;
        if difference >= step {
            nibble |= 4;
            difference -= step;
            delta += step;
        }
        if difference >= step >> 1 {
            nibble |= 2;
            difference -= step >> 1;
            delta += step >> 1;
        }
        if difference >= step >> 2 {
            nibble |= 1;
            delta += step >> 2;
        }

        let predictor = if nibble & 8 == 0 {
            i32::from(self.predictor) + delta
        } else {
            i32::from(self.predictor) - delta
        };
        self.predictor = predictor.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        let next_index =
            (i16::from(self.index) + i16::from(IMA_INDEX_TABLE[usize::from(nibble)])).clamp(0, 88);
        self.index = next_index as u8;
        nibble
    }
}

pub(crate) const fn classify_response(msg_id: u32) -> Option<TalkResponseKind> {
    match msg_id {
        crate::COMMAND_TALK_CAPABILITIES => Some(TalkResponseKind::Ability),
        crate::COMMAND_TALK_CONFIG => Some(TalkResponseKind::Configured),
        crate::COMMAND_TALK_RESET => Some(TalkResponseKind::Reset),
        _ => None,
    }
}

pub(crate) fn parse_response(kind: TalkResponseKind, body: &[u8]) -> Result<TalkEvent, BcError> {
    match kind {
        TalkResponseKind::Ability => Ok(TalkEvent::Ability(Box::new(parse_ability(body)?))),
        TalkResponseKind::Configured => Ok(TalkEvent::Configured),
        TalkResponseKind::Reset => Ok(TalkEvent::Reset),
    }
}

pub(crate) fn build_extension(
    channel: u8,
    binary_data: bool,
    output: &mut [u8],
) -> Result<usize, BcError> {
    xml::build_versioned_document(output, "Extension", "1.1", |builder| {
        builder.u8_element("channelId", channel);
        if binary_data {
            builder.u8_element("binaryData", 1);
        }
    })
}

pub(crate) fn build_config(config: &TalkConfig, output: &mut [u8]) -> Result<usize, BcError> {
    xml::build_xml(output, |builder| {
        builder.start_versioned("TalkConfig", "1.1");
        builder.u8_element("channelId", config.channel);
        builder.text_element("duplex", config.duplex.as_str());
        builder.text_element("audioStreamMode", config.audio_stream_mode.as_str());
        builder.start("audioConfig");
        builder.text_element("audioType", config.audio_profile.audio_type.as_str());
        builder.u32_element("sampleRate", config.audio_profile.sample_rate);
        builder.u32_element("samplePrecision", config.audio_profile.sample_precision);
        builder.u32_element("lengthPerEncoder", config.audio_profile.length_per_encoder);
        builder.text_element("soundTrack", config.audio_profile.sound_track.as_str());
        builder.end();
        builder.end();
    })
}

pub(crate) fn build_adpcm_packet(
    adpcm: &[u8],
    sequence: u16,
    output: &mut [u8],
) -> Result<usize, BcError> {
    if adpcm.len() < ADPCM_BLOCK_HEADER_LEN + 1 {
        return Err(BcError::Protocol(
            "ADPCM block is missing its predictor data",
        ));
    }
    let payload_len = adpcm
        .len()
        .checked_add(4)
        .ok_or(BcError::Protocol("talkback payload length overflow"))?;
    let payload_len_u16 = u16::try_from(payload_len)
        .map_err(|_| BcError::Protocol("talkback payload exceeds protocol limit"))?;
    let packet_len = TALK_FRAME_HEADER_LEN
        .checked_add(adpcm.len())
        .ok_or(BcError::Protocol("talkback packet length overflow"))?;
    let aligned_len = align8(packet_len);
    if output.len() < aligned_len {
        return Err(BcError::BufferTooSmall {
            needed: aligned_len,
            available: output.len(),
        });
    }

    output[..4].copy_from_slice(&MEDIA_MAGIC_ADPCM.to_le_bytes());
    output[4..6].copy_from_slice(&payload_len_u16.to_le_bytes());
    output[6..8].copy_from_slice(&payload_len_u16.to_le_bytes());
    output[8..10].copy_from_slice(&TALK_ADPCM_MARKER.to_le_bytes());
    output[10..12].copy_from_slice(&sequence.to_le_bytes());
    output[TALK_FRAME_HEADER_LEN..packet_len].copy_from_slice(adpcm);
    output[packet_len..aligned_len].fill(0);
    Ok(aligned_len)
}

pub(crate) fn adpcm_packet_capacity(adpcm_len: usize) -> Result<usize, BcError> {
    adpcm_len
        .checked_add(TALK_FRAME_HEADER_LEN + 7)
        .ok_or(BcError::Protocol("talkback frame length overflow"))
}

pub(crate) fn command_header(
    msg_id: u32,
    channel: u8,
    extension_len: usize,
    body_len: usize,
) -> Result<PacketHeader, BcError> {
    let body_len = extension_len
        .checked_add(body_len)
        .ok_or(BcError::Protocol("talkback message length overflow"))?;
    Ok(PacketHeader {
        msg_id,
        body_len: u32::try_from(body_len)
            .map_err(|_| BcError::Protocol("talkback message exceeds protocol limit"))?,
        encryption_offset: u32::from(channel),
        status_class: make_status(BC_CLASS_MODERN_EXT, 0),
        extension: Some(
            u32::try_from(extension_len)
                .map_err(|_| BcError::Protocol("talkback extension exceeds protocol limit"))?,
        ),
    })
}

fn parse_ability(data: &[u8]) -> Result<TalkAbility, BcError> {
    let mut ability = TalkAbility::default();
    let mut current_profile = None;
    let mut error = None;

    xml::visit_xml(data, |event| {
        if error.is_some() {
            return;
        }
        match event {
            XmlVisit::Start("audioConfig") => {
                current_profile = Some(TalkAudioProfile::default());
            }
            XmlVisit::Text { name, text } => {
                let result = if let Some(profile) = current_profile.as_mut() {
                    match name {
                        "audioType" => set_text(&mut profile.audio_type, text),
                        "sampleRate" => set_number(&mut profile.sample_rate, text),
                        "samplePrecision" => set_number(&mut profile.sample_precision, text),
                        "lengthPerEncoder" => set_number(&mut profile.length_per_encoder, text),
                        "soundTrack" => set_text(&mut profile.sound_track, text),
                        _ => Ok(()),
                    }
                } else {
                    match name {
                        "duplex" => push_mode(&mut ability.duplex_modes, text),
                        "audioStreamMode" => push_mode(&mut ability.audio_stream_modes, text),
                        _ => Ok(()),
                    }
                };
                if let Err(parse_error) = result {
                    error = Some(parse_error);
                }
            }
            XmlVisit::End("audioConfig") => {
                if let Some(profile) = current_profile.take()
                    && !profile.audio_type.is_empty()
                    && ability.audio_profiles.try_push(profile).is_err()
                {
                    error = Some(BcError::Protocol("too many camera talkback profiles"));
                }
            }
            _ => {}
        }
    })?;

    error.map_or(Ok(ability), Err)
}

fn preferred_mode(
    modes: &ArrayVec<ArrayString<TALK_TEXT_CAP>, TALK_MODE_CAP>,
    preferred: &str,
) -> Result<ArrayString<TALK_TEXT_CAP>, BcError> {
    modes
        .iter()
        .find(|mode| mode.eq_ignore_ascii_case(preferred))
        .or_else(|| modes.first())
        .cloned()
        .ok_or(BcError::Protocol("camera returned no usable talkback mode"))
}

fn push_mode(
    modes: &mut ArrayVec<ArrayString<TALK_TEXT_CAP>, TALK_MODE_CAP>,
    value: &str,
) -> Result<(), BcError> {
    let mode = bounded_text(value)?;
    if mode.is_empty() || modes.iter().any(|existing| existing == &mode) {
        return Ok(());
    }
    modes
        .try_push(mode)
        .map_err(|_| BcError::Protocol("too many camera talkback modes"))
}

fn set_text<const N: usize>(target: &mut ArrayString<N>, value: &str) -> Result<(), BcError> {
    *target = bounded_text(value)?;
    Ok(())
}

fn bounded_text<const N: usize>(value: &str) -> Result<ArrayString<N>, BcError> {
    ArrayString::try_from(value.trim())
        .map_err(|_| BcError::Protocol("camera talkback value exceeds capacity"))
}

fn set_number(target: &mut u32, value: &str) -> Result<(), BcError> {
    *target = value
        .trim()
        .parse()
        .map_err(|_| BcError::Protocol("camera talkback value is not numeric"))?;
    Ok(())
}

const fn align8(value: usize) -> usize {
    (value + 7) & !7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_adpcm_profile_from_nested_ability() {
        let ability = parse_ability(
            br#"<body><TalkAbility version="1.1"><duplexList><duplex>halfDuplex</duplex><duplex>fullDuplex</duplex></duplexList><audioStreamModeList><audioStreamMode>followVideoStream</audioStreamMode><audioStreamMode>speaker</audioStreamMode></audioStreamModeList><audioConfigList><audioConfig><audioType>g711a</audioType><sampleRate>8000</sampleRate><samplePrecision>16</samplePrecision><lengthPerEncoder>320</lengthPerEncoder><soundTrack>mono</soundTrack></audioConfig><audioConfig><audioType>adpcm</audioType><sampleRate>16000</sampleRate><samplePrecision>16</samplePrecision><lengthPerEncoder>1016</lengthPerEncoder><soundTrack>mono</soundTrack></audioConfig></audioConfigList></TalkAbility></body>"#,
        )
        .unwrap();

        let config = ability.select_adpcm(2).unwrap();
        assert_eq!(config.channel, 2);
        assert_eq!(config.duplex.as_str(), "fullDuplex");
        assert_eq!(config.audio_stream_mode.as_str(), "speaker");
        assert_eq!(config.audio_profile.sample_rate, 16_000);
        assert_eq!(config.audio_profile.length_per_encoder, 1016);
    }

    #[test]
    fn builds_extension_and_config_documents() {
        let config = TalkConfig {
            channel: 0,
            duplex: ArrayString::try_from("fullDuplex").unwrap(),
            audio_stream_mode: ArrayString::try_from("speaker").unwrap(),
            audio_profile: TalkAudioProfile {
                audio_type: ArrayString::try_from("adpcm").unwrap(),
                sample_rate: 8000,
                sample_precision: 16,
                length_per_encoder: 320,
                sound_track: ArrayString::try_from("mono").unwrap(),
            },
        };
        let mut extension = [0_u8; 256];
        let extension_len = build_extension(0, true, &mut extension).unwrap();
        let extension = std::str::from_utf8(&extension[..extension_len]).unwrap();
        assert!(extension.starts_with("<Extension"));
        assert!(extension.contains("<binaryData>1</binaryData>"));

        let mut body = [0_u8; 512];
        let body_len = build_config(&config, &mut body).unwrap();
        let body = std::str::from_utf8(&body[..body_len]).unwrap();
        assert!(body.contains("<TalkConfig"));
        assert!(body.contains("<audioType>adpcm</audioType>"));
        assert!(body.contains("<soundTrack>mono</soundTrack>"));
    }

    #[test]
    fn encodes_zero_pcm_as_an_ima_adpcm_block() {
        let mut encoder = ImaAdpcmEncoder::default();
        let mut output = [0_u8; 8];
        let len = encoder.encode_block(&[0, 0], &mut output).unwrap();
        assert_eq!(len, 5);
        assert_eq!(&output[..len], &[0, 0, 0, 0, 0]);
    }

    #[test]
    fn wraps_adpcm_data_in_a_padded_talk_frame() {
        let adpcm = [0, 0, 0, 0, 0x12];
        let mut output = [0_u8; 32];
        let len = build_adpcm_packet(&adpcm, 0x1234, &mut output).unwrap();
        assert_eq!(len, 24);
        assert_eq!(
            u32::from_le_bytes(output[..4].try_into().unwrap()),
            MEDIA_MAGIC_ADPCM
        );
        assert_eq!(u16::from_le_bytes(output[4..6].try_into().unwrap()), 9);
        assert_eq!(
            u16::from_le_bytes(output[8..10].try_into().unwrap()),
            TALK_ADPCM_MARKER
        );
        assert_eq!(
            u16::from_le_bytes(output[10..12].try_into().unwrap()),
            0x1234
        );
        assert_eq!(&output[12..17], adpcm);
        assert!(output[17..len].iter().all(|byte| *byte == 0));
    }
}
