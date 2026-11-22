use crate::storage::frame::MediaFrame;
use std::time::{Duration, Instant};

pub struct RecordingFrame {
    pub received_at: Instant,
    /// Zero-based monotonic media timestamp from the camera protocol.
    pub timestamp: Option<Duration>,
    /// Original packet evidence, measured independently of the normalized media timeline.
    pub source_rtp: Option<mp4::SourceRtpPacket>,
    pub frame: MediaFrame,
}

impl RecordingFrame {
    pub const fn byte_len(&self) -> usize {
        self.frame.byte_len()
    }

    pub const fn is_video_keyframe(&self) -> bool {
        self.frame.is_video_keyframe()
    }
}
