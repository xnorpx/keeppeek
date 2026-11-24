use crate::storage::frame::MediaFrame;
use std::{
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, Instant},
};

/// A measured packet shares the immutable source identity with other frames of its epoch.
pub struct RecordingRtpTime {
    pub epoch: Arc<RecordingRtpEpoch>,
    pub clock_rate: NonZeroU32,
    pub timestamp: i64,
}

pub struct RecordingRtpEpoch {
    pub session_id: [u8; 16],
    pub epoch_id: [u8; 16],
}

impl RecordingRtpTime {
    pub(crate) fn packet(&self) -> mp4::SourceRtpPacket {
        mp4::SourceRtpPacket {
            session_id: self.epoch.session_id,
            epoch_id: self.epoch.epoch_id,
            clock_rate: self.clock_rate,
            timestamp: self.timestamp,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_packet(packet: mp4::SourceRtpPacket) -> Self {
        Self {
            epoch: Arc::new(RecordingRtpEpoch {
                session_id: packet.session_id,
                epoch_id: packet.epoch_id,
            }),
            clock_rate: packet.clock_rate,
            timestamp: packet.timestamp,
        }
    }
}

pub struct RecordingFrame {
    pub received_at: Instant,
    /// Zero-based monotonic media timestamp from the camera protocol.
    pub timestamp: Option<Duration>,
    /// Original packet evidence, measured independently of the normalized media timeline.
    pub source_rtp: Option<RecordingRtpTime>,
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
