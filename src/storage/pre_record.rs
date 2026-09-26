use super::segment::RecordingFrame;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_STREAMS: usize = 254;
const MAX_STREAM_FRAMES: usize = 16_384;
const MAX_GLOBAL_FRAMES: usize = 131_072;
const MAX_ID_BYTES: usize = 256;

struct Budget {
    bytes: AtomicUsize,
    frames: AtomicUsize,
    max_bytes: usize,
    max_frames: usize,
}

impl Budget {
    fn new(max_bytes: usize, max_frames: usize) -> Self {
        Self {
            bytes: AtomicUsize::new(0),
            frames: AtomicUsize::new(0),
            max_bytes,
            max_frames,
        }
    }

    fn fits(&self, bytes: usize) -> bool {
        self.bytes
            .load(Ordering::Relaxed)
            .checked_add(bytes)
            .is_some_and(|total| total <= self.max_bytes)
            && self.frames.load(Ordering::Relaxed) < self.max_frames
    }

    fn acquire(&self, bytes: usize) -> bool {
        if !self.fits(bytes) {
            return false;
        }
        // Only the registry acquires reservations. Replay owners can only release them.
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.frames.fetch_add(1, Ordering::Relaxed);
        true
    }

    fn release(&self, bytes: usize) {
        self.bytes.fetch_sub(bytes, Ordering::Relaxed);
        self.frames.fetch_sub(1, Ordering::Relaxed);
    }
}

struct BufferedFrame {
    // Field order releases the payload before its memory reservation.
    frame: RecordingFrame,
    _reservation: Reservation,
}

struct Reservation {
    bytes: usize,
    stream_budget: Arc<Budget>,
    global_budget: Arc<Budget>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.stream_budget.release(self.bytes);
        self.global_budget.release(self.bytes);
    }
}

struct Gop {
    start: Instant,
    frames: Vec<BufferedFrame>,
}

struct StreamHistory {
    duration: Duration,
    budget: Arc<Budget>,
    gops: VecDeque<Gop>,
    awaiting_keyframe: bool,
    last_received_at: Option<Instant>,
}

impl StreamHistory {
    fn admit(&mut self, frame: &RecordingFrame, now: Instant, global_limit: usize) -> bool {
        self.expire(now);
        if frame.received_at > now
            || now.saturating_duration_since(frame.received_at) > self.duration
            || self
                .last_received_at
                .is_some_and(|last| frame.received_at < last)
        {
            self.discard_open();
            return false;
        }
        self.last_received_at = Some(frame.received_at);
        let keyframe = frame.is_video_keyframe();
        if self.awaiting_keyframe && !keyframe {
            return false;
        }
        let bytes = frame.byte_len();
        if keyframe {
            self.awaiting_keyframe = true;
        }
        if bytes > self.budget.max_bytes || bytes > global_limit {
            self.discard_open();
            return false;
        }
        while !self.budget.fits(bytes) {
            if !self.evict_oldest() {
                return false;
            }
        }
        if self.awaiting_keyframe && !keyframe {
            return false;
        }

        true
    }

    fn expire(&mut self, now: Instant) {
        while self
            .gops
            .front()
            .is_some_and(|gop| now.saturating_duration_since(gop.start) > self.duration)
        {
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) -> bool {
        let evicted = self.gops.pop_front().is_some();
        if self.gops.is_empty() {
            self.awaiting_keyframe = true;
        }
        evicted
    }

    fn discard_open(&mut self) {
        if !self.awaiting_keyframe {
            self.gops.pop_back();
        }
        self.awaiting_keyframe = true;
    }
}

struct PreRecordBuffers {
    streams: BTreeMap<String, BTreeMap<String, StreamHistory>>,
    stream_count: usize,
    stream_bytes: usize,
    global_budget: Arc<Budget>,
}

impl PreRecordBuffers {
    fn new(stream_bytes: usize, global_bytes: usize) -> Self {
        Self {
            streams: BTreeMap::new(),
            stream_count: 0,
            stream_bytes,
            global_budget: Arc::new(Budget::new(global_bytes, MAX_GLOBAL_FRAMES)),
        }
    }

    fn configure(&mut self, source: &str, stream: &str, duration: Duration) -> bool {
        if source.is_empty()
            || source.len() > MAX_ID_BYTES
            || !matches!(stream, "main" | "sub")
            || duration.is_zero()
            || duration > Duration::from_secs(30)
            || self.stream_bytes == 0
            || self.global_budget.max_bytes == 0
        {
            return false;
        }
        if let Some(history) = self.stream_mut(source, stream) {
            history.gops.clear();
            history.awaiting_keyframe = true;
            history.last_received_at = None;
            history.duration = duration;
            return true;
        }
        if self.stream_count == MAX_STREAMS {
            return false;
        }
        self.streams.entry(source.to_owned()).or_default().insert(
            stream.to_owned(),
            StreamHistory {
                duration,
                budget: Arc::new(Budget::new(self.stream_bytes, MAX_STREAM_FRAMES)),
                gops: VecDeque::new(),
                awaiting_keyframe: true,
                last_received_at: None,
            },
        );
        self.stream_count += 1;
        true
    }

    fn stream_mut(&mut self, source: &str, stream: &str) -> Option<&mut StreamHistory> {
        self.streams.get_mut(source)?.get_mut(stream)
    }

    fn push(&mut self, source: &str, stream: &str, frame: RecordingFrame, now: Instant) {
        let global_limit = self.global_budget.max_bytes;
        let Some(history) = self.stream_mut(source, stream) else {
            return;
        };
        if !history.admit(&frame, now, global_limit) {
            return;
        }
        let bytes = frame.byte_len();
        let keyframe = frame.is_video_keyframe();
        if !self.make_global_room(source, stream, &frame) {
            return;
        }
        let global_budget = Arc::clone(&self.global_budget);
        let history = self
            .stream_mut(source, stream)
            .expect("configured stream remains registered");
        if history.awaiting_keyframe && !keyframe {
            return;
        }
        if !history.budget.acquire(bytes) {
            return;
        }
        if !global_budget.acquire(bytes) {
            history.budget.release(bytes);
            history.discard_open();
            return;
        }
        let received_at = frame.received_at;
        let retained = BufferedFrame {
            frame,
            _reservation: Reservation {
                bytes,
                stream_budget: Arc::clone(&history.budget),
                global_budget,
            },
        };
        if keyframe {
            history.gops.push_back(Gop {
                start: received_at,
                frames: vec![retained],
            });
            history.awaiting_keyframe = false;
        } else if let Some(gop) = history.gops.back_mut() {
            gop.frames.push(retained);
        }
    }

    fn make_global_room(&mut self, source: &str, stream: &str, frame: &RecordingFrame) -> bool {
        let bytes = frame.byte_len();
        let keyframe = frame.is_video_keyframe();
        while !self.global_budget.fits(bytes) {
            // ponytail: Scan at most 254 configured streams only under global pressure.
            let oldest = self
                .streams
                .iter()
                .flat_map(|(source, streams)| {
                    streams.iter().filter_map(move |(stream, history)| {
                        history
                            .gops
                            .front()
                            .map(|gop| (gop.start, source.as_str(), stream.as_str()))
                    })
                })
                .min();
            let Some((oldest_time, oldest_source, oldest_stream)) = oldest else {
                return false;
            };
            if keyframe
                && (frame.received_at, source, stream) < (oldest_time, oldest_source, oldest_stream)
            {
                return false;
            }
            let oldest_source = oldest_source.to_owned();
            let oldest_stream = oldest_stream.to_owned();
            self.stream_mut(&oldest_source, &oldest_stream)
                .expect("selected registered stream")
                .evict_oldest();
            if !keyframe
                && self
                    .stream_mut(source, stream)
                    .is_some_and(|history| history.awaiting_keyframe)
            {
                return false;
            }
        }
        true
    }

    fn take(&mut self, source: &str, stream: &str, now: Instant) -> Vec<BufferedFrame> {
        let Some(history) = self.stream_mut(source, stream) else {
            return Vec::new();
        };
        history.expire(now);
        history.awaiting_keyframe = true;
        history.gops.drain(..).flat_map(|gop| gop.frames).collect()
    }

    fn bytes(&self) -> usize {
        self.global_budget.bytes.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::frame::{MediaFrame, VideoCodec, VideoFrame};
    use bytes::Bytes;

    fn video(now: Instant, keyframe: bool, bytes: usize) -> RecordingFrame {
        RecordingFrame {
            received_at: now,
            timestamp: None,
            frame: MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H264,
                is_keyframe: keyframe,
                width: 1920,
                height: 1080,
                data: Bytes::from(vec![1; bytes]),
            }),
        }
    }

    #[test]
    fn pressure_evicts_whole_gops_and_replay_keeps_its_budget() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 16);
        buffers.configure("camera", "main", Duration::from_secs(30));
        buffers.push("camera", "main", video(now, true, 3), now);
        buffers.push("camera", "main", video(now, false, 3), now);
        let later = now + Duration::from_secs(1);
        buffers.push("camera", "main", video(later, true, 3), later);
        let replay = buffers.take("camera", "main", later);
        assert_eq!(replay.len(), 1);
        assert_eq!(buffers.bytes(), 3);
        drop(replay);
        assert_eq!(buffers.bytes(), 0);
    }

    #[test]
    fn global_pressure_uses_oldest_gop_and_stable_identity_tiebreak() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 8);
        buffers.configure("b", "main", Duration::from_secs(30));
        buffers.configure("a", "main", Duration::from_secs(30));
        buffers.push("b", "main", video(now, true, 4), now);
        buffers.push("a", "main", video(now, true, 4), now);
        let later = now + Duration::from_secs(1);
        buffers.push("b", "main", video(later, true, 4), later);
        assert!(buffers.take("a", "main", later).is_empty());
        assert_eq!(buffers.take("b", "main", later).len(), 2);
    }

    #[test]
    fn silent_stream_history_expires_before_replay() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 16);
        buffers.configure("camera", "main", Duration::from_secs(2));
        buffers.push("camera", "main", video(now, true, 3), now);
        assert_eq!(buffers.bytes(), 3);
        assert!(
            buffers
                .take("camera", "main", now + Duration::from_secs(3))
                .is_empty()
        );
        assert_eq!(buffers.bytes(), 0);
    }

    #[test]
    fn delayed_arrival_does_not_enter_expired_history() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 16);
        buffers.configure("camera", "main", Duration::from_secs(2));
        buffers.push(
            "camera",
            "main",
            video(now, true, 3),
            now + Duration::from_secs(3),
        );
        assert_eq!(buffers.bytes(), 0);
    }

    #[test]
    fn replay_prevents_refilling_the_same_stream_over_budget() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 16);
        buffers.configure("camera", "main", Duration::from_secs(30));
        buffers.push("camera", "main", video(now, true, 6), now);
        let replay = buffers.take("camera", "main", now);
        buffers.push("camera", "main", video(now, true, 3), now);
        assert_eq!(buffers.bytes(), 6);
        assert!(buffers.take("camera", "main", now).is_empty());
        drop(replay);
        buffers.push("camera", "main", video(now, true, 3), now);
        assert_eq!(buffers.take("camera", "main", now).len(), 1);
    }

    #[test]
    fn oversized_gop_discards_its_prefix_and_waits_for_a_new_keyframe() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 16);
        buffers.configure("camera", "main", Duration::from_secs(30));
        buffers.push("camera", "main", video(now, true, 6), now);
        buffers.push("camera", "main", video(now, false, 3), now);
        buffers.push("camera", "main", video(now, false, 1), now);
        assert_eq!(buffers.bytes(), 0);
        buffers.push("camera", "main", video(now, true, 3), now);
        assert_eq!(buffers.take("camera", "main", now).len(), 1);
    }

    #[test]
    fn out_of_order_frame_invalidates_dependent_frames() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let mut buffers = PreRecordBuffers::new(32, 32);
        buffers.configure("camera", "main", Duration::from_secs(30));
        buffers.push("camera", "main", video(now, true, 3), now);
        buffers.push("camera", "main", video(later, false, 3), later);
        buffers.push("camera", "main", video(now, false, 3), later);
        buffers.push("camera", "main", video(later, false, 3), later);
        assert!(buffers.take("camera", "main", later).is_empty());
        buffers.push("camera", "main", video(later, true, 3), later);
        assert_eq!(buffers.take("camera", "main", later).len(), 1);
    }

    #[test]
    fn metadata_is_bounded_even_when_payloads_are_empty() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 16);
        buffers.configure("camera", "main", Duration::from_secs(30));
        for _ in 0..MAX_STREAM_FRAMES + 10 {
            buffers.push("camera", "main", video(now, true, 0), now);
        }
        assert_eq!(buffers.take("camera", "main", now).len(), MAX_STREAM_FRAMES);
    }

    #[test]
    fn stream_registration_and_duration_are_bounded() {
        let mut buffers = PreRecordBuffers::new(8, 16);
        assert!(!buffers.configure("camera", "main", Duration::ZERO));
        assert!(!buffers.configure("camera", "main", Duration::from_secs(31)));
        for index in 0..MAX_STREAMS {
            assert!(buffers.configure(&format!("camera-{index}"), "main", Duration::from_secs(30)));
        }
        assert!(!buffers.configure("overflow", "main", Duration::from_secs(30)));
    }

    #[test]
    fn pending_replay_keeps_global_budget_across_other_streams_and_reconfiguration() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 8);
        buffers.configure("a", "main", Duration::from_secs(30));
        buffers.configure("b", "main", Duration::from_secs(30));
        buffers.push("a", "main", video(now, true, 6), now);
        let replay = buffers.take("a", "main", now);
        buffers.configure("a", "main", Duration::from_secs(2));
        buffers.push("b", "main", video(now, true, 3), now);
        assert!(buffers.take("b", "main", now).is_empty());
        assert_eq!(buffers.bytes(), 6);
        drop(replay);
        buffers.push("b", "main", video(now, true, 3), now);
        assert_eq!(buffers.take("b", "main", now).len(), 1);
    }

    #[test]
    fn reservation_outlives_payload_and_registry() {
        struct Payload {
            data: [u8; 3],
            budget: Arc<Budget>,
        }
        impl AsRef<[u8]> for Payload {
            fn as_ref(&self) -> &[u8] {
                &self.data
            }
        }
        impl Drop for Payload {
            fn drop(&mut self) {
                assert_eq!(self.budget.bytes.load(Ordering::Relaxed), 3);
            }
        }
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 8);
        buffers.configure("camera", "main", Duration::from_secs(30));
        let budget = Arc::clone(&buffers.global_budget);
        let mut frame = video(now, true, 3);
        let MediaFrame::Video(video) = &mut frame.frame else {
            unreachable!()
        };
        video.data = Bytes::from_owner(Payload {
            data: [0; 3],
            budget: Arc::clone(&budget),
        });
        buffers.push("camera", "main", frame, now);
        let replay = buffers.take("camera", "main", now);
        assert_eq!(replay[0].frame.byte_len(), 3);
        drop(buffers);
        assert_eq!(budget.bytes.load(Ordering::Relaxed), 3);
        drop(replay);
        assert_eq!(budget.bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn dropping_the_incoming_gop_does_not_evict_unrelated_history() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let mut buffers = PreRecordBuffers::new(20, 10);
        buffers.configure("a", "main", Duration::from_secs(30));
        buffers.configure("b", "main", Duration::from_secs(30));
        buffers.push("a", "main", video(now, true, 6), now);
        buffers.push("b", "main", video(later, true, 4), later);
        buffers.push("a", "main", video(later, false, 8), later);
        assert_eq!(buffers.bytes(), 4);
        assert!(buffers.take("a", "main", later).is_empty());
        assert_eq!(buffers.take("b", "main", later).len(), 1);
    }

    #[test]
    fn global_frame_count_includes_detached_replay() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 16);
        let mut held = Vec::new();
        for index in 0..MAX_GLOBAL_FRAMES / MAX_STREAM_FRAMES {
            let source = format!("camera-{index}");
            buffers.configure(&source, "main", Duration::from_secs(30));
            for _ in 0..MAX_STREAM_FRAMES {
                buffers.push(&source, "main", video(now, true, 0), now);
            }
            held.extend(buffers.take(&source, "main", now));
        }
        buffers.configure("overflow", "main", Duration::from_secs(30));
        buffers.push("overflow", "main", video(now, true, 0), now);
        assert!(buffers.take("overflow", "main", now).is_empty());
        assert_eq!(
            buffers.global_budget.frames.load(Ordering::Relaxed),
            MAX_GLOBAL_FRAMES
        );
        drop(held);
        buffers.push("overflow", "main", video(now, true, 0), now);
        assert_eq!(buffers.take("overflow", "main", now).len(), 1);
    }
}
