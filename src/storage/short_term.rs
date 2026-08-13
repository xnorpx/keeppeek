use crate::storage::segment::RecordingFrame;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub struct ShortTermBuffer {
    chunks: VecDeque<RecordingFrame>,
    max_duration: Duration,
    total_bytes: usize,
}

impl ShortTermBuffer {
    pub const fn new(max_duration: Duration) -> Self {
        Self {
            chunks: VecDeque::new(),
            max_duration,
            total_bytes: 0,
        }
    }

    pub fn push(&mut self, frame: RecordingFrame) {
        self.total_bytes += frame.byte_len();
        self.chunks.push_back(frame);
        self.evict_expired();
    }

    fn evict_expired(&mut self) {
        let Some(newest) = self.chunks.back() else {
            return;
        };
        let cutoff = newest.received_at - self.max_duration;
        while let Some(oldest) = self.chunks.front() {
            if oldest.received_at < cutoff {
                self.total_bytes -= oldest.byte_len();
                self.chunks.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn drain_before(&mut self, cutoff: Instant) -> Vec<RecordingFrame> {
        let mut drained = Vec::new();
        while let Some(front) = self.chunks.front() {
            if front.received_at < cutoff {
                let frame = self.chunks.pop_front().unwrap();
                self.total_bytes -= frame.byte_len();
                drained.push(frame);
            } else {
                break;
            }
        }
        drained
    }

    pub fn drain_up_to_last_keyframe_before(&mut self, cutoff: Instant) -> Vec<RecordingFrame> {
        let first_kf = self
            .chunks
            .iter()
            .position(|f| f.is_video_keyframe() && f.received_at < cutoff);

        let Some(start) = first_kf else {
            return Vec::new();
        };

        let last_kf = self
            .chunks
            .iter()
            .enumerate()
            .rev()
            .find(|(_, f)| f.is_video_keyframe() && f.received_at < cutoff)
            .map(|(i, _)| i)
            .unwrap();

        let end = self
            .chunks
            .iter()
            .enumerate()
            .skip(last_kf + 1)
            .find(|(_, f)| f.is_video_keyframe())
            .map(|(i, _)| i)
            .unwrap_or(last_kf + 1);

        for _ in 0..start {
            let frame = self.chunks.pop_front().unwrap();
            self.total_bytes -= frame.byte_len();
        }

        let count = end - start;
        let mut drained = Vec::with_capacity(count);
        for _ in 0..count {
            let frame = self.chunks.pop_front().unwrap();
            self.total_bytes -= frame.byte_len();
            drained.push(frame);
        }
        drained
    }

    pub fn drain_all(&mut self) -> Vec<RecordingFrame> {
        self.total_bytes = 0;
        self.chunks.drain(..).collect()
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub const fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn duration(&self) -> Duration {
        match (self.chunks.front(), self.chunks.back()) {
            (Some(first), Some(last)) => last.received_at.duration_since(first.received_at),
            _ => Duration::ZERO,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &RecordingFrame> {
        self.chunks.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::frame::{MediaFrame, VideoCodec, VideoFrame};

    fn video_frame(keyframe: bool, at: Instant) -> RecordingFrame {
        RecordingFrame {
            received_at: at,
            camera_dts_90k: None,
            frame: MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H264,
                is_keyframe: keyframe,
                width: 1920,
                height: 1080,
                data: vec![0u8; 500].into(),
            }),
        }
    }

    #[test]
    fn evicts_beyond_max_duration() {
        let mut buf = ShortTermBuffer::new(Duration::from_secs(60));
        let now = Instant::now();

        buf.push(video_frame(true, now - Duration::from_secs(120)));
        buf.push(video_frame(false, now - Duration::from_secs(30)));
        buf.push(video_frame(false, now));

        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn drain_up_to_keyframe() {
        let mut buf = ShortTermBuffer::new(Duration::from_secs(300));
        let now = Instant::now();

        buf.push(video_frame(true, now - Duration::from_secs(100)));
        buf.push(video_frame(false, now - Duration::from_secs(80)));
        buf.push(video_frame(true, now - Duration::from_secs(60)));
        buf.push(video_frame(false, now - Duration::from_secs(40)));
        buf.push(video_frame(false, now));

        let cutoff = now - Duration::from_secs(50);
        let drained = buf.drain_up_to_last_keyframe_before(cutoff);
        assert_eq!(drained.len(), 3);
        assert_eq!(buf.len(), 2);
    }
}
