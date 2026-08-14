use bytes::Bytes;
use std::{fmt, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    H265,
}

impl fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::H264 => f.write_str("h264"),
            Self::H265 => f.write_str("h265"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioCodec {
    Aac,
    G711Alaw,
    G711Ulaw,
    Adpcm,
}

impl AudioCodec {
    pub const fn default_sample_rate(self) -> u32 {
        match self {
            Self::Aac => 16000,
            Self::G711Alaw | Self::G711Ulaw => 8000,
            Self::Adpcm => 8000,
        }
    }
}

impl fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aac => f.write_str("aac"),
            Self::G711Alaw => f.write_str("g711a"),
            Self::G711Ulaw => f.write_str("g711u"),
            Self::Adpcm => f.write_str("adpcm"),
        }
    }
}

pub struct VideoFrame {
    pub codec: VideoCodec,
    pub is_keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub data: Bytes,
}

pub struct AudioFrame {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub duration: Duration,
    pub data: Vec<u8>,
}

pub enum MediaFrame {
    Video(VideoFrame),
    Audio(AudioFrame),
}

impl MediaFrame {
    pub const fn byte_len(&self) -> usize {
        match self {
            Self::Video(v) => v.data.len(),
            Self::Audio(a) => a.data.len(),
        }
    }

    pub const fn is_video_keyframe(&self) -> bool {
        matches!(self, Self::Video(v) if v.is_keyframe)
    }

    pub const fn is_video(&self) -> bool {
        matches!(self, Self::Video(_))
    }

    pub const fn is_audio(&self) -> bool {
        matches!(self, Self::Audio(_))
    }
}

impl fmt::Debug for MediaFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Video(v) => f
                .debug_struct("Video")
                .field("codec", &v.codec)
                .field("keyframe", &v.is_keyframe)
                .field("resolution", &format_args!("{}x{}", v.width, v.height))
                .field("bytes", &v.data.len())
                .finish(),
            Self::Audio(a) => f
                .debug_struct("Audio")
                .field("codec", &a.codec)
                .field("sample_rate", &a.sample_rate)
                .field("bytes", &a.data.len())
                .finish(),
        }
    }
}
