use bytes::Bytes;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const MAX_AUDIO_QUEUE_BYTES: usize = 256 * 1024;
const MAX_AUDIO_QUEUE_AGE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioCodec {
    Aac,
    G711Alaw,
    G711Ulaw,
}

#[derive(Debug, Clone)]
pub(crate) struct AudioFrame {
    pub(crate) codec: AudioCodec,
    pub(crate) sample_rate_hz: u32,
    pub(crate) channel_count: u8,
    pub(crate) timestamp: Option<Duration>,
    pub(crate) received_at: Instant,
    pub(crate) data: Bytes,
}

#[derive(Debug, Default)]
pub(crate) struct AudioQueue {
    frames: VecDeque<AudioFrame>,
    bytes: usize,
    dropped_frames: u64,
}

impl AudioQueue {
    pub(crate) fn push(&mut self, frame: AudioFrame, now: Instant) {
        self.expire(now);
        self.bytes = self.bytes.saturating_add(frame.data.len());
        self.frames.push_back(frame);
        while self.bytes > MAX_AUDIO_QUEUE_BYTES {
            self.drop_oldest();
        }
    }

    pub(crate) fn pop(&mut self, now: Instant) -> Option<AudioFrame> {
        self.expire(now);
        let frame = self.frames.pop_front()?;
        self.bytes = self.bytes.saturating_sub(frame.data.len());
        Some(frame)
    }

    pub(crate) const fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn expire(&mut self, now: Instant) {
        while self.frames.front().is_some_and(|frame| {
            now.saturating_duration_since(frame.received_at) > MAX_AUDIO_QUEUE_AGE
        }) {
            self.drop_oldest();
        }
    }

    fn drop_oldest(&mut self) {
        if let Some(frame) = self.frames.pop_front() {
            self.bytes = self.bytes.saturating_sub(frame.data.len());
            self.dropped_frames = self.dropped_frames.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioCodec, AudioFrame, AudioQueue};
    use bytes::Bytes;
    use std::time::{Duration, Instant};

    fn frame(received_at: Instant, len: usize) -> AudioFrame {
        AudioFrame {
            codec: AudioCodec::G711Alaw,
            sample_rate_hz: 8_000,
            channel_count: 1,
            timestamp: None,
            received_at,
            data: Bytes::from(vec![0; len]),
        }
    }

    #[test]
    fn queue_drops_oldest_audio_when_byte_bound_is_reached() {
        let start = Instant::now();
        let mut queue = AudioQueue::default();
        queue.push(frame(start, 200 * 1024), start);
        queue.push(frame(start, 100 * 1024), start);

        assert_eq!(queue.dropped_frames(), 1);
        assert_eq!(queue.pop(start).unwrap().data.len(), 100 * 1024);
    }

    #[test]
    fn queue_expires_audio_before_it_can_accumulate_latency() {
        let start = Instant::now();
        let mut queue = AudioQueue::default();
        queue.push(frame(start, 1), start);

        assert!(queue.pop(start + Duration::from_millis(251)).is_none());
        assert_eq!(queue.dropped_frames(), 1);
        assert!(queue.is_empty());
    }
}
