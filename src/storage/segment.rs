use crate::storage::frame::MediaFrame;
use std::time::Instant;

pub struct RecordingFrame {
    pub received_at: Instant,
    /// Monotonic decode timestamp from the camera in 90 kHz units.
    /// When present, the MP4 writer uses this instead of `received_at`
    /// for sample timing, avoiding jitter from bursty TCP delivery.
    pub camera_dts_90k: Option<u64>,
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
