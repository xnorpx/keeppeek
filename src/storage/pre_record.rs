use super::{frame::MediaFrame, segment::RecordingFrame};
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

pub(super) struct BufferedFrame {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryReason {
    Ready,
    ReplayPending,
    Startup,
    MissingKeyframe,
    DurationEviction,
    StreamPressure,
    GlobalPressure,
    InvalidOrder,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct HistorySnapshot {
    pub requested: Duration,
    pub available: Duration,
    pub retained_bytes: usize,
    pub pending_bytes: usize,
    pub reason: HistoryReason,
}

struct StreamHistory {
    duration: Duration,
    budget: Arc<Budget>,
    gops: VecDeque<Gop>,
    awaiting_keyframe: bool,
    discontinuous: bool,
    last_received_at: Option<Instant>,
    reason: HistoryReason,
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
            self.reason = if now.saturating_duration_since(frame.received_at) > self.duration {
                HistoryReason::DurationEviction
            } else {
                HistoryReason::InvalidOrder
            };
            self.discard_open();
            return false;
        }
        self.last_received_at = Some(frame.received_at);
        let keyframe = frame.is_video_keyframe();
        if self.awaiting_keyframe && !keyframe {
            if self.reason == HistoryReason::Startup {
                self.reason = HistoryReason::MissingKeyframe;
            }
            return false;
        }
        let bytes = frame.byte_len();
        if keyframe {
            self.awaiting_keyframe = true;
        }
        if bytes > self.budget.max_bytes || bytes > global_limit {
            self.reason = if bytes > self.budget.max_bytes {
                HistoryReason::StreamPressure
            } else {
                HistoryReason::GlobalPressure
            };
            self.discard_open();
            return false;
        }
        if keyframe && self.discontinuous {
            // Older GOPs cannot establish continuous coverage across a discarded GOP.
            self.gops.clear();
            self.discontinuous = false;
        }
        while !self.budget.fits(bytes) {
            self.reason = HistoryReason::StreamPressure;
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
            self.reason = HistoryReason::DurationEviction;
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
        self.discontinuous = true;
    }
}

pub(super) struct PreRecordBuffers {
    streams: BTreeMap<String, BTreeMap<String, StreamHistory>>,
    stream_count: usize,
    stream_bytes: usize,
    global_budget: Arc<Budget>,
}

impl PreRecordBuffers {
    pub(super) fn new(stream_bytes: usize, global_bytes: usize) -> Self {
        Self {
            streams: BTreeMap::new(),
            stream_count: 0,
            stream_bytes,
            global_budget: Arc::new(Budget::new(global_bytes, MAX_GLOBAL_FRAMES)),
        }
    }

    pub(super) fn configure(&mut self, source: &str, stream: &str, duration: Duration) -> bool {
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
            history.discontinuous = false;
            history.last_received_at = None;
            history.duration = duration;
            history.reason = HistoryReason::Startup;
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
                discontinuous: false,
                last_received_at: None,
                reason: HistoryReason::Startup,
            },
        );
        self.stream_count += 1;
        true
    }

    fn stream_mut(&mut self, source: &str, stream: &str) -> Option<&mut StreamHistory> {
        self.streams.get_mut(source)?.get_mut(stream)
    }

    pub(super) fn push(&mut self, source: &str, stream: &str, frame: RecordingFrame, now: Instant) {
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
            self.stream_mut(source, stream)
                .expect("configured stream remains registered")
                .reason = HistoryReason::GlobalPressure;
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
            if matches!(
                history.reason,
                HistoryReason::MissingKeyframe | HistoryReason::InvalidOrder
            ) {
                history.reason = HistoryReason::Startup;
            }
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
            let evicted = self
                .stream_mut(&oldest_source, &oldest_stream)
                .expect("selected registered stream");
            evicted.reason = HistoryReason::GlobalPressure;
            evicted.evict_oldest();
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

    pub(super) fn take(&mut self, source: &str, stream: &str, now: Instant) -> Vec<BufferedFrame> {
        let Some(history) = self.stream_mut(source, stream) else {
            return Vec::new();
        };
        history.expire(now);
        history.awaiting_keyframe = true;
        let mut frames: Vec<_> = history.gops.drain(..).flat_map(|gop| gop.frames).collect();
        if !frames.is_empty() {
            history.reason = HistoryReason::Startup;
        }
        trim_audio_to_video(&mut frames);
        frames
    }

    pub(super) fn bytes(&self) -> usize {
        self.global_budget.bytes.load(Ordering::Relaxed)
    }

    pub(super) fn snapshot(
        &mut self,
        source: &str,
        stream: &str,
        now: Instant,
    ) -> Option<HistorySnapshot> {
        let history = self.stream_mut(source, stream)?;
        history.expire(now);
        let start = history.gops.front().map(|gop| gop.start);
        let end = history
            .gops
            .back()
            .and_then(|gop| {
                gop.frames
                    .iter()
                    .rev()
                    .find(|frame| frame.frame.frame.is_video())
            })
            .map(|frame| frame.frame.received_at);
        let retained_bytes = history
            .gops
            .iter()
            .flat_map(|gop| &gop.frames)
            .map(|frame| frame.frame.byte_len())
            .sum();
        let reserved_bytes = history.budget.bytes.load(Ordering::Relaxed);
        let available = start.zip(end).map_or(Duration::ZERO, |(start, end)| {
            end.saturating_duration_since(start)
        });
        let pending_bytes = reserved_bytes.saturating_sub(retained_bytes);
        let reason = if available >= history.duration {
            HistoryReason::Ready
        } else if history.gops.is_empty() && pending_bytes > 0 {
            HistoryReason::ReplayPending
        } else {
            history.reason
        };
        Some(HistorySnapshot {
            requested: history.duration,
            available,
            retained_bytes,
            pending_bytes,
            reason,
        })
    }
}

impl BufferedFrame {
    pub(super) fn into_frame(self) -> RecordingFrame {
        let mut frame = self.frame;
        let data = match &mut frame.frame {
            MediaFrame::Video(video) => &mut video.data,
            MediaFrame::Audio(audio) => &mut audio.data,
        };
        *data = bytes::Bytes::from_owner(ReservedPayload {
            data: std::mem::take(data),
            _reservation: self._reservation,
        });
        frame
    }
}

struct ReservedPayload {
    data: bytes::Bytes,
    _reservation: Reservation,
}

impl AsRef<[u8]> for ReservedPayload {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

fn trim_audio_to_video(frames: &mut Vec<BufferedFrame>) {
    let mut video = frames.iter().filter(|frame| frame.frame.frame.is_video());
    let Some(first) = video.next() else {
        frames.clear();
        return;
    };
    let last = video.next_back().unwrap_or(first);
    let wall_start = first.frame.received_at;
    let wall_end = last.frame.received_at;
    let mut audio_end = wall_start;
    frames.retain(|retained| {
        let MediaFrame::Audio(audio) = &retained.frame.frame else {
            return true;
        };
        // Encoded packets cannot be trimmed without decoding. Drop crossing packets.
        let start = retained.frame.received_at;
        let Some(end) = start.checked_add(audio.duration) else {
            return false;
        };
        if start < audio_end || end > wall_end || audio.duration.is_zero() {
            return false;
        }
        audio_end = end;
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::frame::{VideoCodec, VideoFrame};
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

    fn audio(now: Instant, timestamp: Option<Duration>, duration_ms: u64) -> RecordingFrame {
        RecordingFrame {
            received_at: now,
            timestamp,
            frame: MediaFrame::Audio(crate::storage::frame::AudioFrame {
                codec: crate::storage::frame::AudioCodec::Aac,
                sample_rate: 16000,
                duration: Duration::from_millis(duration_ms),
                data: bytes::Bytes::from_static(&[1; 3]),
            }),
        }
    }

    #[test]
    fn replay_drops_audio_packets_crossing_video_end() {
        let start = Instant::now();
        let end = start + Duration::from_millis(100);
        let mut buffers = PreRecordBuffers::new(100, 100);
        buffers.configure("camera", "main", Duration::from_secs(2));
        buffers.push("camera", "main", video(start, true, 3), start);
        let inside = start + Duration::from_millis(20);
        buffers.push("camera", "main", audio(inside, None, 20), inside);
        let crossing = start + Duration::from_millis(90);
        buffers.push("camera", "main", audio(crossing, None, 20), crossing);
        buffers.push("camera", "main", video(end, false, 3), end);
        let replay = buffers.take("camera", "main", end);
        assert_eq!(replay.len(), 3);
        assert_eq!(replay[1].frame.received_at, inside);
        assert_eq!(buffers.bytes(), 9);
    }

    #[test]
    fn replay_uses_receive_clock_when_track_timestamp_origins_differ() {
        let start = Instant::now();
        let end = start + Duration::from_millis(100);
        let mut buffers = PreRecordBuffers::new(100, 100);
        buffers.configure("camera", "main", Duration::from_secs(2));
        let mut first = video(start, true, 3);
        first.timestamp = Some(Duration::from_secs(10));
        buffers.push("camera", "main", first, start);
        for timestamp_ms in [20, 30, 90] {
            let received = start + Duration::from_millis(timestamp_ms);
            buffers.push(
                "camera",
                "main",
                audio(received, Some(Duration::from_millis(timestamp_ms)), 20),
                received,
            );
        }
        let mut last = video(end, false, 3);
        last.timestamp = Some(Duration::from_millis(10100));
        buffers.push("camera", "main", last, end);
        let replay = buffers.take("camera", "main", end);
        assert_eq!(replay.len(), 3);
        assert_eq!(replay[0].frame.timestamp, Some(Duration::from_secs(10)));
        assert_eq!(
            replay[1].frame.received_at,
            start + Duration::from_millis(20)
        );
        assert_eq!(buffers.bytes(), 9);
    }

    #[test]
    fn replay_reservation_follows_video_payload_into_writer_samples() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(8, 8);
        buffers.configure("camera", "main", Duration::from_secs(2));
        buffers.push("camera", "main", video(now, true, 3), now);
        let replay = buffers.take("camera", "main", now);
        let frame = replay.into_iter().next().unwrap().into_frame();
        let MediaFrame::Video(video) = frame.frame else {
            unreachable!()
        };
        let sample = video.data.slice(1..);
        drop(video);
        assert_eq!(buffers.bytes(), 3);
        drop(sample);
        assert_eq!(buffers.bytes(), 0);
    }

    #[test]
    fn snapshots_distinguish_history_from_pending_replay_and_expire_silent_streams() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let mut buffers = PreRecordBuffers::new(10, 20);
        buffers.configure("camera", "main", Duration::from_secs(2));
        assert_eq!(
            buffers.snapshot("camera", "main", now).unwrap().reason,
            HistoryReason::Startup
        );
        buffers.push("camera", "main", video(now, false, 3), now);
        assert_eq!(
            buffers.snapshot("camera", "main", now).unwrap().reason,
            HistoryReason::MissingKeyframe
        );
        buffers.push("camera", "main", video(now, true, 3), now);
        buffers.push("camera", "main", video(later, false, 3), later);
        let before = buffers.snapshot("camera", "main", later).unwrap();
        assert_eq!(before.available, Duration::from_secs(1));
        assert_eq!((before.retained_bytes, before.pending_bytes), (6, 0));
        let replay = buffers.take("camera", "main", later);
        let pending = buffers.snapshot("camera", "main", later).unwrap();
        assert_eq!(pending.available, Duration::ZERO);
        assert_eq!((pending.retained_bytes, pending.pending_bytes), (0, 6));
        drop(replay);
        buffers.push("camera", "main", video(later, true, 3), later);
        let expired = buffers
            .snapshot("camera", "main", later + Duration::from_secs(3))
            .unwrap();
        assert_eq!(expired.reason, HistoryReason::DurationEviction);
        assert_eq!(expired.retained_bytes, 0);
        assert_eq!(expired.available, Duration::ZERO);
    }

    #[test]
    fn snapshots_report_local_and_global_pressure_separately() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let mut buffers = PreRecordBuffers::new(6, 8);
        buffers.configure("a", "main", Duration::from_secs(2));
        buffers.configure("b", "main", Duration::from_secs(2));
        buffers.push("a", "main", video(now, true, 4), now);
        buffers.push("a", "main", video(now, false, 4), now);
        assert_eq!(
            buffers.snapshot("a", "main", now).unwrap().reason,
            HistoryReason::StreamPressure
        );
        assert_eq!(buffers.bytes(), 0);
        buffers.push("a", "main", video(now, true, 4), now);
        buffers.push("b", "main", video(later, true, 6), later);
        assert_eq!(
            buffers.snapshot("a", "main", later).unwrap().reason,
            HistoryReason::GlobalPressure
        );
        assert_eq!(buffers.bytes(), 6);
    }

    #[test]
    fn snapshot_reasons_recover_when_history_refills() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(10, 10);
        buffers.configure("camera", "main", Duration::from_secs(2));
        buffers.push("camera", "main", video(now, false, 3), now);
        buffers.push("camera", "main", video(now, true, 3), now);
        assert_eq!(
            buffers.snapshot("camera", "main", now).unwrap().reason,
            HistoryReason::Startup
        );
        let later = now + Duration::from_secs(2);
        buffers.push("camera", "main", video(later, false, 3), later);
        assert_eq!(
            buffers.snapshot("camera", "main", later).unwrap().reason,
            HistoryReason::Ready
        );
        let replay = buffers.take("camera", "main", later);
        assert_eq!(
            buffers.snapshot("camera", "main", later).unwrap().reason,
            HistoryReason::ReplayPending
        );
        drop(replay);
        assert_eq!(
            buffers.snapshot("camera", "main", later).unwrap().reason,
            HistoryReason::Startup
        );
    }

    #[test]
    fn recovery_does_not_report_coverage_across_a_discarded_gop() {
        let now = Instant::now();
        let mut buffers = PreRecordBuffers::new(100, 100);
        buffers.configure("camera", "main", Duration::from_secs(2));
        buffers.push("camera", "main", video(now, true, 3), now);
        let second = now + Duration::from_secs(1);
        buffers.push("camera", "main", video(second, true, 3), second);
        buffers.push("camera", "main", video(now, false, 3), second);
        let recovered = now + Duration::from_secs(2);
        buffers.push("camera", "main", video(recovered, true, 3), recovered);
        let snapshot = buffers.snapshot("camera", "main", recovered).unwrap();
        assert_eq!(snapshot.available, Duration::ZERO);
        assert_eq!(buffers.take("camera", "main", recovered).len(), 1);
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
