//! Media frame parsing for binary Stream payloads.
//!
//! Baichuan Stream messages (msg_id 3) carry binary payloads containing
//! one or more media frames. Each frame is identified by a 4-byte magic
//! and followed by frame-type-specific header fields and payload data.
//! All frames are 8-byte aligned within the payload.

use crate::error::BcError;

/// Stream info V1 header magic (`"1001"` as LE u32).
pub const MEDIA_MAGIC_INFO_V1: u32 = 0x31303031;

/// Stream info V2 header magic (`"1002"` as LE u32).
pub const MEDIA_MAGIC_INFO_V2: u32 = 0x32303031;

/// I-frame magic base (channel 0). Add channel index for channels 1-9.
pub const MEDIA_MAGIC_IFRAME_BASE: u32 = 0x63643030;

/// P-frame magic base (channel 0). Add channel index for channels 1-9.
pub const MEDIA_MAGIC_PFRAME_BASE: u32 = 0x63643130;

/// AAC audio frame magic (`"05wb"` as LE u32).
pub const MEDIA_MAGIC_AAC: u32 = 0x62773530;

/// Alternate AAC audio frame magic (`"15wb"` as LE u32).
pub const MEDIA_MAGIC_AAC_V2: u32 = 0x62773531;

/// ADPCM audio frame magic (`"01wb"` as LE u32).
pub const MEDIA_MAGIC_ADPCM: u32 = 0x62773130;

/// Offsets to try first when scanning for the next valid frame after corruption.
const RECOVERY_OFFSETS: [usize; 3] = [528, 1056, 1584];

/// Video codec identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    H265,
}

/// Audio codec identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Adpcm,
    Pcm,
    G711Alaw,
    G711Ulaw,
}

/// Baichuan embedded timestamp (wall-clock, year-offset encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BcTimestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Identified media frame magic type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaMagic {
    InfoV1,
    InfoV2,
    IFrame(u8),
    PFrame(u8),
    AacAudio,
    AdpcmAudio,
}

impl MediaMagic {
    /// Identify a media frame magic from a u32 value (read as LE from wire).
    pub fn from_u32(magic: u32) -> Option<Self> {
        match magic {
            MEDIA_MAGIC_INFO_V1 => Some(Self::InfoV1),
            MEDIA_MAGIC_INFO_V2 => Some(Self::InfoV2),
            MEDIA_MAGIC_AAC | MEDIA_MAGIC_AAC_V2 => Some(Self::AacAudio),
            MEDIA_MAGIC_ADPCM => Some(Self::AdpcmAudio),
            m if (MEDIA_MAGIC_IFRAME_BASE..=MEDIA_MAGIC_IFRAME_BASE + 9).contains(&m) => {
                Some(Self::IFrame((m - MEDIA_MAGIC_IFRAME_BASE) as u8))
            }
            m if (MEDIA_MAGIC_PFRAME_BASE..=MEDIA_MAGIC_PFRAME_BASE + 9).contains(&m) => {
                Some(Self::PFrame((m - MEDIA_MAGIC_PFRAME_BASE) as u8))
            }
            _ => None,
        }
    }

    /// Convert this magic back to its u32 representation.
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::InfoV1 => MEDIA_MAGIC_INFO_V1,
            Self::InfoV2 => MEDIA_MAGIC_INFO_V2,
            Self::IFrame(ch) => MEDIA_MAGIC_IFRAME_BASE + ch as u32,
            Self::PFrame(ch) => MEDIA_MAGIC_PFRAME_BASE + ch as u32,
            Self::AacAudio => MEDIA_MAGIC_AAC,
            Self::AdpcmAudio => MEDIA_MAGIC_ADPCM,
        }
    }
}

/// Parsed stream info (from InfoV1 or InfoV2 headers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamMetadata {
    pub width: u32,
    pub height: u32,
    pub fps: u8,
    pub start_time: Option<BcTimestamp>,
    pub end_time: Option<BcTimestamp>,
}

/// Video frame reference into the raw payload buffer.
#[derive(Debug, Clone, Copy)]
pub struct VideoFrameRef<'a> {
    /// Channel index (0-9).
    pub channel: u8,
    /// Whether this is a keyframe (I-frame).
    pub is_keyframe: bool,
    /// Video codec.
    pub codec: VideoCodec,
    /// Raw video bitstream data (Annex B or other format).
    pub data: &'a [u8],
    /// Sub-second timestamp in microseconds.
    pub microseconds: u32,
}

/// Audio frame reference into the raw payload buffer.
#[derive(Debug, Clone, Copy)]
pub struct AudioFrameRef<'a> {
    /// Audio codec.
    pub codec: AudioCodec,
    /// Raw audio frame data.
    pub data: &'a [u8],
}

/// Parsed media frame. Payload data borrows the source buffer.
#[derive(Debug, Clone, Copy)]
pub enum MediaFrame<'a> {
    Info(StreamMetadata),
    Video(VideoFrameRef<'a>),
    Audio(AudioFrameRef<'a>),
}

/// Round up to the next 8-byte boundary.
pub(crate) const fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// Read a LE u32 from a byte slice at the given offset.
const fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read a LE u16 from a byte slice at the given offset.
const fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Parse stream metadata. Returns the metadata and total bytes consumed.
pub(crate) const fn parse_stream_metadata(data: &[u8]) -> Result<(StreamMetadata, usize), BcError> {
    if data.len() < 8 {
        return Err(BcError::Incomplete);
    }

    let header_size = read_u32_le(data, 4) as usize;
    if header_size < 8 {
        return Err(BcError::Protocol("stream info header size too small"));
    }
    if data.len() < header_size {
        return Err(BcError::Incomplete);
    }

    let width = if header_size >= 12 {
        read_u32_le(data, 8)
    } else {
        0
    };
    let height = if header_size >= 16 {
        read_u32_le(data, 12)
    } else {
        0
    };
    // Offset 16 is reserved/undocumented, offset 17 is FPS.
    let fps = if header_size >= 18 { data[17] } else { 0 };

    let start_time = if header_size >= 24 {
        Some(BcTimestamp {
            year: 2000 + data[18] as u16,
            month: data[19],
            day: data[20],
            hour: data[21],
            minute: data[22],
            second: data[23],
        })
    } else {
        None
    };

    let end_time = if header_size >= 30 {
        Some(BcTimestamp {
            year: 2000 + data[24] as u16,
            month: data[25],
            day: data[26],
            hour: data[27],
            minute: data[28],
            second: data[29],
        })
    } else {
        None
    };

    Ok((
        StreamMetadata {
            width,
            height,
            fps,
            start_time,
            end_time,
        },
        header_size,
    ))
}

/// Parse a video frame header.
/// Returns (codec, data_len, microseconds, total_header_len, stream_handle_hint).
pub(crate) fn parse_video_header(
    data: &[u8],
) -> Result<(VideoCodec, u32, u32, usize, u32), BcError> {
    // magic(4) + video_type(4) + data_len(4) + ah_size(4) + us(4) + unknown(4) = 24
    if data.len() < 24 {
        return Err(BcError::Incomplete);
    }

    let codec = match &data[4..8] {
        b"H264" => VideoCodec::H264,
        b"H265" => VideoCodec::H265,
        _ => return Err(BcError::Protocol("unknown video codec in frame header")),
    };

    let data_len = read_u32_le(data, 8);
    let additional_header_size = read_u32_le(data, 12);
    let microseconds = read_u32_le(data, 16);
    let stream_handle_hint = read_u32_le(data, 20);

    let header_total = 24 + additional_header_size as usize;
    Ok((
        codec,
        data_len,
        microseconds,
        header_total,
        stream_handle_hint,
    ))
}

/// Parse an AAC audio frame header. Returns (data_len, header_len).
pub(crate) const fn parse_aac_header(data: &[u8]) -> Result<(usize, usize), BcError> {
    // magic(4) + data_len(2) + data_len_verify(2) = 8
    if data.len() < 8 {
        return Err(BcError::Incomplete);
    }
    let data_len = read_u16_le(data, 4) as usize;
    Ok((data_len, 8))
}

/// Parse an ADPCM audio frame header. Returns (block_len, header_len).
pub(crate) const fn parse_adpcm_header(data: &[u8]) -> Result<(usize, usize), BcError> {
    // magic(4) + size1(2) + size2(2) + magic_data(2) + half_block(2) = 12
    if data.len() < 12 {
        return Err(BcError::Incomplete);
    }
    let payload_len = read_u16_le(data, 4) as usize;
    if payload_len < 4 {
        return Err(BcError::Protocol(
            "ADPCM payload is shorter than its subheader",
        ));
    }
    if read_u16_le(data, 8) != 0x0100 {
        return Err(BcError::Protocol("invalid ADPCM frame marker"));
    }
    Ok((payload_len - 4, 12))
}

/// Zero-allocation iterator over media frames in a binary payload.
///
/// Yields `Result<MediaFrame<'a>, BcError>` for each frame. Frames
/// that reference video/audio data borrow directly from the source
/// buffer with no heap copy.
pub struct MediaFrameIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> MediaFrameIter<'a> {
    /// Create an iterator over media frames in the given binary payload.
    pub const fn new(data: &'a [u8]) -> Self {
        MediaFrameIter { data, pos: 0 }
    }

    /// Remaining unparsed bytes.
    pub const fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Try to parse the next frame at the current position.
    fn parse_next(&mut self) -> Result<Option<MediaFrame<'a>>, BcError> {
        let remaining = &self.data[self.pos..];
        if remaining.len() < 4 {
            return Ok(None);
        }

        let magic_u32 = read_u32_le(remaining, 0);
        let magic = match MediaMagic::from_u32(magic_u32) {
            Some(m) => m,
            None => {
                // Unknown magic: attempt corruption recovery
                if let Some(offset) = self.scan_for_magic() {
                    self.pos += offset;
                    return self.parse_next();
                }
                self.pos = self.data.len();
                return Ok(None);
            }
        };

        match magic {
            MediaMagic::InfoV1 | MediaMagic::InfoV2 => {
                let (info, consumed) = parse_stream_metadata(remaining)?;
                self.pos += align8(consumed);
                Ok(Some(MediaFrame::Info(info)))
            }
            MediaMagic::IFrame(channel) | MediaMagic::PFrame(channel) => {
                let is_keyframe = matches!(magic, MediaMagic::IFrame(_));
                let (codec, data_len, microseconds, header_total, _stream_handle_hint) =
                    parse_video_header(remaining)?;
                let frame_end = header_total + data_len as usize;
                if remaining.len() < frame_end {
                    return Err(BcError::Incomplete);
                }
                let video_data = &remaining[header_total..frame_end];
                self.pos += frame_end + padding_len(data_len as usize);
                Ok(Some(MediaFrame::Video(VideoFrameRef {
                    channel,
                    is_keyframe,
                    codec,
                    data: video_data,
                    microseconds,
                })))
            }
            MediaMagic::AacAudio => {
                let (data_len, header_len) = parse_aac_header(remaining)?;
                let frame_end = header_len + data_len;
                if remaining.len() < frame_end {
                    return Err(BcError::Incomplete);
                }
                let audio_data = &remaining[header_len..frame_end];
                self.pos += align8(frame_end);
                Ok(Some(MediaFrame::Audio(AudioFrameRef {
                    codec: AudioCodec::Aac,
                    data: audio_data,
                })))
            }
            MediaMagic::AdpcmAudio => {
                let (data_len, header_len) = parse_adpcm_header(remaining)?;
                let frame_end = header_len + data_len;
                if remaining.len() < frame_end {
                    return Err(BcError::Incomplete);
                }
                let audio_data = &remaining[header_len..frame_end];
                self.pos += align8(frame_end);
                Ok(Some(MediaFrame::Audio(AudioFrameRef {
                    codec: AudioCodec::Adpcm,
                    data: audio_data,
                })))
            }
        }
    }

    /// Scan forward for a known media magic value.
    ///
    /// First checks fast-path offsets (528, 1056, 1584) before falling
    /// back to a linear byte-by-byte scan.
    fn scan_for_magic(&self) -> Option<usize> {
        let remaining = &self.data[self.pos..];

        // Fast-path offsets
        for &offset in &RECOVERY_OFFSETS {
            if offset + 4 <= remaining.len() {
                let m = read_u32_le(remaining, offset);
                if MediaMagic::from_u32(m).is_some() {
                    return Some(offset);
                }
            }
        }

        // Linear scan (skip offset 0 -- that's the unknown magic)
        for i in 1..remaining.len().saturating_sub(3) {
            let m = read_u32_le(remaining, i);
            if MediaMagic::from_u32(m).is_some() {
                return Some(i);
            }
        }

        None
    }
}

const fn padding_len(payload_len: usize) -> usize {
    (8 - payload_len % 8) % 8
}

impl<'a> Iterator for MediaFrameIter<'a> {
    type Item = Result<MediaFrame<'a>, BcError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }
        match self.parse_next() {
            Ok(Some(frame)) => Some(Ok(frame)),
            Ok(None) => None,
            Err(e) => {
                self.pos = self.data.len();
                Some(Err(e))
            }
        }
    }
}

/// Create a media frame iterator over a binary payload.
pub const fn parse_media_frames(data: &[u8]) -> MediaFrameIter<'_> {
    MediaFrameIter::new(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stream_metadata_bytes(magic: u32, width: u32, height: u32, fps: u8) -> Vec<u8> {
        let header_size: u32 = 30;
        let mut buf = Vec::new();
        buf.extend_from_slice(&magic.to_le_bytes());
        buf.extend_from_slice(&header_size.to_le_bytes());
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        buf.push(0); // reserved byte at offset 16
        buf.push(fps); // FPS at offset 17
        // Start: year_offset=25 (2025), month=2, day=15, hour=10, min=30, sec=0
        buf.extend_from_slice(&[25, 2, 15, 10, 30, 0]);
        // End: year_offset=25, month=2, day=15, hour=11, min=30, sec=0
        buf.extend_from_slice(&[25, 2, 15, 11, 30, 0]);
        while buf.len() % 8 != 0 {
            buf.push(0);
        }
        buf
    }

    fn make_video_frame_bytes(
        magic: u32,
        codec: &[u8; 4],
        data: &[u8],
        microseconds: u32,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&magic.to_le_bytes());
        buf.extend_from_slice(codec);
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // additional header = 0
        buf.extend_from_slice(&microseconds.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // unknown
        buf.extend_from_slice(data);
        while buf.len() % 8 != 0 {
            buf.push(0);
        }
        buf
    }

    fn make_aac_frame_bytes(data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MEDIA_MAGIC_AAC.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(data.len() as u16).to_le_bytes()); // verify field
        buf.extend_from_slice(data);
        while buf.len() % 8 != 0 {
            buf.push(0);
        }
        buf
    }

    fn make_adpcm_frame_bytes(data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let payload_len = data.len() + 4;
        buf.extend_from_slice(&MEDIA_MAGIC_ADPCM.to_le_bytes());
        buf.extend_from_slice(&(payload_len as u16).to_le_bytes());
        buf.extend_from_slice(&(payload_len as u16).to_le_bytes());
        buf.extend_from_slice(&0x0100u16.to_le_bytes()); // magic_data
        buf.extend_from_slice(&((data.len() / 2) as u16).to_le_bytes());
        buf.extend_from_slice(data);
        while buf.len() % 8 != 0 {
            buf.push(0);
        }
        buf
    }

    #[test]
    fn magic_info_v1() {
        assert_eq!(MediaMagic::from_u32(0x31303031), Some(MediaMagic::InfoV1));
    }

    #[test]
    fn magic_info_v2() {
        assert_eq!(MediaMagic::from_u32(0x32303031), Some(MediaMagic::InfoV2));
    }

    #[test]
    fn magic_iframe_channels() {
        for ch in 0..=9u8 {
            let m = MEDIA_MAGIC_IFRAME_BASE + ch as u32;
            assert_eq!(MediaMagic::from_u32(m), Some(MediaMagic::IFrame(ch)));
        }
    }

    #[test]
    fn magic_pframe_channels() {
        for ch in 0..=9u8 {
            let m = MEDIA_MAGIC_PFRAME_BASE + ch as u32;
            assert_eq!(MediaMagic::from_u32(m), Some(MediaMagic::PFrame(ch)));
        }
    }

    #[test]
    fn magic_aac() {
        assert_eq!(
            MediaMagic::from_u32(MEDIA_MAGIC_AAC),
            Some(MediaMagic::AacAudio)
        );
    }

    #[test]
    fn magic_aac_v2() {
        assert_eq!(
            MediaMagic::from_u32(MEDIA_MAGIC_AAC_V2),
            Some(MediaMagic::AacAudio)
        );
    }

    #[test]
    fn magic_adpcm() {
        assert_eq!(
            MediaMagic::from_u32(MEDIA_MAGIC_ADPCM),
            Some(MediaMagic::AdpcmAudio)
        );
    }

    #[test]
    fn magic_unknown() {
        assert_eq!(MediaMagic::from_u32(0xDEADBEEF), None);
    }

    #[test]
    fn magic_roundtrip() {
        let variants = [
            MediaMagic::InfoV1,
            MediaMagic::InfoV2,
            MediaMagic::IFrame(0),
            MediaMagic::IFrame(5),
            MediaMagic::IFrame(9),
            MediaMagic::PFrame(0),
            MediaMagic::PFrame(9),
            MediaMagic::AacAudio,
            MediaMagic::AdpcmAudio,
        ];
        for m in variants {
            assert_eq!(MediaMagic::from_u32(m.to_u32()), Some(m));
        }
    }

    #[test]
    fn parse_stream_info_v1() {
        let data = make_stream_metadata_bytes(MEDIA_MAGIC_INFO_V1, 2560, 1440, 15);
        let mut iter = MediaFrameIter::new(&data);
        match iter.next().unwrap().unwrap() {
            MediaFrame::Info(info) => {
                assert_eq!(info.width, 2560);
                assert_eq!(info.height, 1440);
                assert_eq!(info.fps, 15);
                assert_eq!(
                    info.start_time,
                    Some(BcTimestamp {
                        year: 2025,
                        month: 2,
                        day: 15,
                        hour: 10,
                        minute: 30,
                        second: 0,
                    })
                );
                assert_eq!(
                    info.end_time,
                    Some(BcTimestamp {
                        year: 2025,
                        month: 2,
                        day: 15,
                        hour: 11,
                        minute: 30,
                        second: 0,
                    })
                );
            }
            other => panic!("expected Info, got {other:?}"),
        }
        assert!(iter.next().is_none());
    }

    #[test]
    fn parse_stream_info_v2() {
        let data = make_stream_metadata_bytes(MEDIA_MAGIC_INFO_V2, 1920, 1080, 30);
        let mut iter = MediaFrameIter::new(&data);
        match iter.next().unwrap().unwrap() {
            MediaFrame::Info(info) => {
                assert_eq!(info.width, 1920);
                assert_eq!(info.height, 1080);
                assert_eq!(info.fps, 30);
            }
            other => panic!("expected Info, got {other:?}"),
        }
    }

    #[test]
    fn parse_iframe_h264() {
        let payload = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB];
        let data = make_video_frame_bytes(MEDIA_MAGIC_IFRAME_BASE, b"H264", &payload, 12345);
        let mut iter = MediaFrameIter::new(&data);
        match iter.next().unwrap().unwrap() {
            MediaFrame::Video(v) => {
                assert_eq!(v.channel, 0);
                assert!(v.is_keyframe);
                assert_eq!(v.codec, VideoCodec::H264);
                assert_eq!(v.data, &payload);
                assert_eq!(v.microseconds, 12345);
            }
            other => panic!("expected Video, got {other:?}"),
        }
    }

    #[test]
    fn parse_pframe_h265() {
        let payload = vec![0x00, 0x00, 0x00, 0x01, 0x02, 0x01];
        let magic = MEDIA_MAGIC_PFRAME_BASE + 2; // channel 2
        let data = make_video_frame_bytes(magic, b"H265", &payload, 99999);
        let mut iter = MediaFrameIter::new(&data);
        match iter.next().unwrap().unwrap() {
            MediaFrame::Video(v) => {
                assert_eq!(v.channel, 2);
                assert!(!v.is_keyframe);
                assert_eq!(v.codec, VideoCodec::H265);
                assert_eq!(v.data, &payload);
                assert_eq!(v.microseconds, 99999);
            }
            other => panic!("expected Video, got {other:?}"),
        }
    }

    #[test]
    fn parse_video_with_additional_header() {
        let video_data = vec![0xDD; 16];
        let additional_header = vec![0xEE; 8];
        let mut buf = Vec::new();
        buf.extend_from_slice(&MEDIA_MAGIC_IFRAME_BASE.to_le_bytes());
        buf.extend_from_slice(b"H264");
        buf.extend_from_slice(&(video_data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(additional_header.len() as u32).to_le_bytes());
        buf.extend_from_slice(&42u32.to_le_bytes()); // microseconds
        buf.extend_from_slice(&0u32.to_le_bytes()); // unknown
        buf.extend_from_slice(&additional_header);
        buf.extend_from_slice(&video_data);
        while buf.len() % 8 != 0 {
            buf.push(0);
        }

        let mut iter = MediaFrameIter::new(&buf);
        match iter.next().unwrap().unwrap() {
            MediaFrame::Video(v) => {
                assert_eq!(v.data, &video_data);
                assert_eq!(v.microseconds, 42);
            }
            other => panic!("expected Video, got {other:?}"),
        }
    }

    #[test]
    fn parse_aac_audio() {
        let audio_data = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x00];
        let data = make_aac_frame_bytes(&audio_data);
        let mut iter = MediaFrameIter::new(&data);
        match iter.next().unwrap().unwrap() {
            MediaFrame::Audio(a) => {
                assert_eq!(a.codec, AudioCodec::Aac);
                assert_eq!(a.data, &audio_data);
            }
            other => panic!("expected Audio, got {other:?}"),
        }
    }

    #[test]
    fn parse_aac_v2_audio() {
        let audio_data = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x00];
        let mut data = make_aac_frame_bytes(&audio_data);
        data[..4].copy_from_slice(&MEDIA_MAGIC_AAC_V2.to_le_bytes());
        let mut iter = MediaFrameIter::new(&data);
        match iter.next().unwrap().unwrap() {
            MediaFrame::Audio(audio) => {
                assert_eq!(audio.codec, AudioCodec::Aac);
                assert_eq!(audio.data, audio_data);
            }
            other => panic!("expected Audio, got {other:?}"),
        }
    }

    #[test]
    fn parse_adpcm_audio() {
        let audio_data = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let data = make_adpcm_frame_bytes(&audio_data);
        let mut iter = MediaFrameIter::new(&data);
        match iter.next().unwrap().unwrap() {
            MediaFrame::Audio(a) => {
                assert_eq!(a.codec, AudioCodec::Adpcm);
                assert_eq!(a.data, &audio_data);
            }
            other => panic!("expected Audio, got {other:?}"),
        }
    }

    #[test]
    fn reject_adpcm_audio_with_an_invalid_marker() {
        let mut data = make_adpcm_frame_bytes(&[0x01, 0x02, 0x03, 0x04]);
        data[8..10].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let mut iter = MediaFrameIter::new(&data);
        assert!(matches!(
            iter.next(),
            Some(Err(BcError::Protocol("invalid ADPCM frame marker")))
        ));
    }

    #[test]
    fn parse_multi_frame_aligned() {
        let mut payload = Vec::new();
        payload.extend(make_stream_metadata_bytes(
            MEDIA_MAGIC_INFO_V1,
            1920,
            1080,
            25,
        ));
        let video_data = vec![0xAA; 100];
        payload.extend(make_video_frame_bytes(
            MEDIA_MAGIC_IFRAME_BASE,
            b"H264",
            &video_data,
            1000,
        ));
        let pframe_data = vec![0xBB; 50];
        payload.extend(make_video_frame_bytes(
            MEDIA_MAGIC_PFRAME_BASE,
            b"H264",
            &pframe_data,
            2000,
        ));
        let audio_data = vec![0xCC; 20];
        payload.extend(make_aac_frame_bytes(&audio_data));

        let frames: Vec<_> = MediaFrameIter::new(&payload)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(frames.len(), 4);
        assert!(matches!(frames[0], MediaFrame::Info(_)));
        assert!(matches!(
            frames[1],
            MediaFrame::Video(ref v) if v.is_keyframe && v.data.len() == 100
        ));
        assert!(matches!(
            frames[2],
            MediaFrame::Video(ref v) if !v.is_keyframe && v.data.len() == 50
        ));
        assert!(matches!(
            frames[3],
            MediaFrame::Audio(ref a) if a.codec == AudioCodec::Aac && a.data.len() == 20
        ));
    }

    #[test]
    fn corruption_recovery_linear_scan() {
        let mut payload = Vec::new();
        // Garbage before a valid frame
        payload.extend_from_slice(&[0xFF; 20]);
        let video_data = vec![0xAA; 8];
        payload.extend(make_video_frame_bytes(
            MEDIA_MAGIC_IFRAME_BASE,
            b"H264",
            &video_data,
            5000,
        ));

        let frames: Vec<_> = MediaFrameIter::new(&payload)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            MediaFrame::Video(v) => {
                assert!(v.is_keyframe);
                assert_eq!(v.data, &[0xAA; 8]);
            }
            other => panic!("expected Video, got {other:?}"),
        }
    }

    #[test]
    fn corruption_recovery_fast_path() {
        let mut payload = Vec::new();
        // Garbage exactly 528 bytes, then a valid frame
        payload.extend_from_slice(&[0xFF; 528]);
        let video_data = vec![0xBB; 16];
        payload.extend(make_video_frame_bytes(
            MEDIA_MAGIC_IFRAME_BASE,
            b"H265",
            &video_data,
            3000,
        ));

        let frames: Vec<_> = MediaFrameIter::new(&payload)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            MediaFrame::Video(v) => {
                assert_eq!(v.codec, VideoCodec::H265);
            }
            other => panic!("expected Video, got {other:?}"),
        }
    }

    #[test]
    fn only_garbage_yields_nothing() {
        let payload = [0xFF; 100];
        let frames: Vec<_> = MediaFrameIter::new(&payload)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(frames.is_empty());
    }

    #[test]
    fn align8_values() {
        assert_eq!(align8(0), 0);
        assert_eq!(align8(1), 8);
        assert_eq!(align8(7), 8);
        assert_eq!(align8(8), 8);
        assert_eq!(align8(9), 16);
        assert_eq!(align8(16), 16);
        assert_eq!(align8(24), 24);
        assert_eq!(align8(25), 32);
    }

    #[test]
    fn empty_payload() {
        let mut iter = MediaFrameIter::new(&[]);
        assert!(iter.next().is_none());
    }

    #[test]
    fn too_short_for_magic() {
        let mut iter = MediaFrameIter::new(&[0x30, 0x30]);
        assert!(iter.next().is_none());
    }

    #[test]
    fn remaining_tracks_position() {
        let data = make_stream_metadata_bytes(MEDIA_MAGIC_INFO_V1, 640, 480, 15);
        let total = data.len();
        let mut iter = MediaFrameIter::new(&data);
        assert_eq!(iter.remaining(), total);
        iter.next().unwrap().unwrap();
        assert_eq!(iter.remaining(), 0);
    }

    #[test]
    fn parse_media_frames_convenience() {
        let data = make_stream_metadata_bytes(MEDIA_MAGIC_INFO_V1, 640, 480, 10);
        let count = parse_media_frames(&data).count();
        assert_eq!(count, 1);
    }
}
