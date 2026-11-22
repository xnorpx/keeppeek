//! Bounded presentation delay. A stream shares its byte and frame budget across all listeners.

use super::{StreamKind, WebRtcAudioCodec};
use crate::{api::proto, cameras::LiveAudioSettings, storage::VideoCodec};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

pub(super) const MAX_AGE: Duration = Duration::from_millis(250);
const MAX_VIDEO_BYTES: usize = 2 * 1024 * 1024;
const MAX_AUDIO_BYTES: usize = 256 * 1024;
const MAX_FRAMES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Format {
    Audio(WebRtcAudioCodec, u32, u8),
    Video(VideoCodec),
}

#[derive(Default)]
pub(super) struct CameraTiming {
    settings: RwLock<Policy>,
    audio: [Mutex<Usage>; 2],
    video: [Mutex<Usage>; 2],
}

#[derive(Default)]
struct Policy {
    settings: LiveAudioSettings,
    reset_at: Option<Instant>,
    generation: u64,
}

#[derive(Clone, Copy)]
struct StreamPolicy {
    offset: i32,
    reset_at: Option<Instant>,
    generation: u64,
}

#[derive(Default, Clone, Copy)]
struct Usage {
    expired_drops: u64,
    capacity_drops: u64,
    bytes: usize,
    reserved_bytes: usize,
    frames: usize,
}

impl CameraTiming {
    pub(super) fn configure(&self, settings: LiveAudioSettings) {
        let mut policy = self
            .settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if policy.settings != settings {
            policy.settings = settings;
            policy.generation = policy.generation.wrapping_add(1);
            policy.reset_at = Some(Instant::now());
        }
    }

    pub(super) fn reset(&self) {
        let mut policy = self
            .settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        policy.reset_at = Some(Instant::now());
        policy.generation = policy.generation.wrapping_add(1);
    }

    fn budget(&self, stream: StreamKind, audio: bool) -> &Mutex<Usage> {
        let index = usize::from(stream == StreamKind::Sub);
        if audio {
            &self.audio[index]
        } else {
            &self.video[index]
        }
    }

    fn policy(&self, stream: StreamKind) -> StreamPolicy {
        let policy = self
            .settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let value = match stream {
            StreamKind::Main => policy.settings.main_offset_ms,
            StreamKind::Sub => policy.settings.sub_offset_ms,
        };
        StreamPolicy {
            offset: value.or(policy.settings.default_offset_ms).unwrap_or(0),
            reset_at: policy.reset_at,
            generation: policy.generation,
        }
    }

    fn record_expired(&self, stream: StreamKind, audio: bool) {
        let mut usage = self
            .budget(stream, audio)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        usage.expired_drops = usage.expired_drops.saturating_add(1);
    }

    pub(super) fn report(
        &self,
        stream: StreamKind,
        preview: Option<i32>,
    ) -> proto::LiveAudioTiming {
        let configured_offset_ms = self.policy(stream).offset;
        let usage = |audio| {
            *self
                .budget(stream, audio)
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        let (audio, video) = (usage(true), usage(false));
        proto::LiveAudioTiming {
            held_audio_bytes: audio.bytes as u32,
            held_video_bytes: video.bytes as u32,
            held_video_frames: video.frames as u32,
            capacity_drops: audio.capacity_drops.saturating_add(video.capacity_drops),
            expired_drops: audio.expired_drops.saturating_add(video.expired_drops),
            configured_offset_ms,
            applied_offset_ms: preview.unwrap_or(configured_offset_ms),
            preview: preview.is_some(),
        }
    }
}

struct Reservation {
    camera: Arc<CameraTiming>,
    stream: StreamKind,
    audio: bool,
    bytes: usize,
    charge: usize,
}

impl Reservation {
    fn acquire<T>(
        camera: &Arc<CameraTiming>,
        stream: StreamKind,
        audio: bool,
        bytes: usize,
    ) -> Option<Self> {
        let mut used = camera
            .budget(stream, audio)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let limit = if audio {
            MAX_AUDIO_BYTES
        } else {
            MAX_VIDEO_BYTES
        };
        // A one-byte audio packet still occupies a queue entry. Charge its
        // bookkeeping to the audio budget so variable packetization stays bounded.
        let charge = if audio {
            bytes.saturating_add(size_of::<Held<T>>())
        } else {
            bytes
        };
        if bytes == 0
            || charge > limit - used.reserved_bytes
            || (!audio && used.frames == MAX_FRAMES)
        {
            used.capacity_drops = used.capacity_drops.saturating_add(1);
            return None;
        }
        used.frames += 1;
        used.bytes += bytes;
        used.reserved_bytes += charge;
        Some(Self {
            camera: camera.clone(),
            stream,
            audio,
            bytes,
            charge,
        })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut used = self
            .camera
            .budget(self.stream, self.audio)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        used.frames -= 1;
        used.bytes -= self.bytes;
        used.reserved_bytes -= self.charge;
    }
}

struct Held<T> {
    item: T,
    received: Instant,
    due: Instant,
    reservation: Reservation,
}

/// Survives serialization so a backed-up data channel cannot hand a stale
/// corrected frame to the listener after a reset, unsubscribe, or expiration.
#[derive(Clone)]
pub(super) struct DeliveryPermit(Arc<DeferredDelivery>);

struct DeferredDelivery {
    camera: Arc<CameraTiming>,
    stream: StreamKind,
    audio: bool,
    received: Instant,
    generation: std::sync::Weak<()>,
    policy_generation: u64,
    counted_expired: std::sync::atomic::AtomicBool,
}

impl DeliveryPermit {
    pub(super) fn current(&self, now: Instant) -> bool {
        let delivery = &self.0;
        if delivery.generation.upgrade().is_none()
            || delivery.camera.policy(delivery.stream).generation != delivery.policy_generation
        {
            return false;
        }
        if now.saturating_duration_since(delivery.received) <= MAX_AGE {
            return true;
        }
        if !delivery
            .counted_expired
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            delivery
                .camera
                .record_expired(delivery.stream, delivery.audio);
        }
        false
    }
}

// Active and pending profiles can overlap during an adaptive keyframe handover.
struct ProfileQueue<T> {
    generation: Arc<()>,
    state: Option<(Format, u64)>,
    last_timestamp: Option<Duration>,
    frames: VecDeque<Held<T>>,
}

impl<T> Default for ProfileQueue<T> {
    fn default() -> Self {
        Self {
            generation: Arc::new(()),
            state: None,
            last_timestamp: None,
            frames: VecDeque::new(),
        }
    }
}

impl<T> ProfileQueue<T> {
    fn clear(&mut self) {
        if Arc::weak_count(&self.generation) > 0 {
            self.generation = Arc::new(());
        }
        self.frames.clear();
        self.state = None;
        self.last_timestamp = None;
    }
}

pub(super) struct Queue<T> {
    camera: Arc<CameraTiming>,
    preview: Option<i32>,
    profiles: [ProfileQueue<T>; 2],
}

impl<T> Queue<T> {
    pub(super) fn new(camera: Arc<CameraTiming>) -> Self {
        Self {
            camera,
            preview: None,
            profiles: Default::default(),
        }
    }

    pub(super) fn set_preview(&mut self, preview: Option<i32>) {
        if self.preview != preview {
            self.clear();
            self.preview = preview;
        }
    }

    pub(super) fn offsets(&self, stream: StreamKind) -> (i32, i32) {
        let configured = self.camera.policy(stream).offset;
        (configured, self.preview.unwrap_or(configured))
    }

    pub(super) fn report(&self, stream: StreamKind) -> proto::LiveAudioTiming {
        self.camera.report(stream, self.preview)
    }

    pub(super) fn delivery_permit(
        &self,
        stream: StreamKind,
        received: Instant,
        due: Instant,
    ) -> Option<DeliveryPermit> {
        if due <= received {
            return None;
        }
        let profile = &self.profiles[usize::from(stream == StreamKind::Sub)];
        let (format, policy_generation) = profile.state?;
        Some(DeliveryPermit(Arc::new(DeferredDelivery {
            camera: self.camera.clone(),
            stream,
            audio: matches!(format, Format::Audio(..)),
            received,
            policy_generation,
            generation: Arc::downgrade(&profile.generation),
            counted_expired: false.into(),
        })))
    }

    pub(super) fn clear(&mut self) {
        self.profiles.iter_mut().for_each(ProfileQueue::clear);
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.profiles
            .iter()
            .filter_map(|profile| profile.frames.front().map(|frame| frame.due))
            .min()
    }

    pub(super) fn pop_due(&mut self, now: Instant) -> Option<(T, Instant)> {
        while let Some((index, head)) = self
            .profiles
            .iter()
            .enumerate()
            .filter_map(|(index, profile)| Some((index, profile.frames.front()?)))
            .min_by_key(|(_, head)| head.due)
        {
            let stream = head.reservation.stream;
            let policy = self.camera.policy(stream);
            let expired = now.saturating_duration_since(head.received) > MAX_AGE;
            if expired {
                self.camera.record_expired(stream, head.reservation.audio);
            }
            if self.profiles[index]
                .state
                .is_none_or(|(_, generation)| generation != policy.generation)
                || expired
            {
                self.profiles[index].frames.pop_front();
                continue;
            }
            if head.due > now {
                break;
            }
            let head = self.profiles[index]
                .frames
                .pop_front()
                .expect("head was present");
            return Some((head.item, head.due));
        }
        None
    }

    pub(super) fn admit(&mut self, item: T, packet: Packet, now: Instant) -> Option<(T, Instant)> {
        let Packet {
            stream,
            format,
            received,
            timestamp,
            bytes,
        } = packet;
        let policy = self.camera.policy(stream);
        if policy.reset_at.is_some_and(|reset| received < reset) {
            return None;
        }
        let profile = &mut self.profiles[usize::from(stream == StreamKind::Sub)];
        let state = (format, policy.generation);
        if profile.state != Some(state)
            || timestamp
                .zip(profile.last_timestamp)
                .is_some_and(|(next, prev)| next < prev)
        {
            profile.clear();
            profile.state = Some(state);
        }
        profile.last_timestamp = timestamp;
        let offset = self.preview.unwrap_or(policy.offset);
        let audio = matches!(format, Format::Audio(..));
        let ms = if audio {
            offset.max(0)
        } else {
            (-offset).max(0)
        };
        if ms == 0 {
            return Some((item, received));
        }
        let age = now.saturating_duration_since(received);
        if received > now || age > MAX_AGE {
            self.camera.record_expired(stream, audio);
            return None;
        }
        let due = received + Duration::from_millis(u64::from(ms.unsigned_abs()));
        if due <= now && profile.frames.is_empty() {
            return Some((item, due));
        }
        let reservation = Reservation::acquire::<T>(&self.camera, stream, audio, bytes)?;
        profile.frames.push_back(Held {
            item,
            received,
            due,
            reservation,
        });
        None
    }
}

pub(super) struct Packet {
    pub(super) stream: StreamKind,
    pub(super) format: Format,
    pub(super) received: Instant,
    pub(super) timestamp: Option<Duration>,
    pub(super) bytes: usize,
}

/// Maps a sender reset onto the current listener timeline. The original capture timestamp
/// remains on the ingested packet and in the recording.
#[derive(Default)]
pub(super) struct AudioClock {
    previous: Option<(Instant, Option<Duration>, Duration)>,
}

impl AudioClock {
    pub(super) fn map(
        &mut self,
        received: Instant,
        source: Option<Duration>,
        rate: u32,
    ) -> Duration {
        let mapped = self.previous.map_or_else(
            || source.unwrap_or_default(),
            |(old_wall, old_source, old_mapped)| {
                let wall_step = received.saturating_duration_since(old_wall);
                let step = old_source
                    .zip(source)
                    .and_then(|(old, next)| next.checked_sub(old))
                    .filter(|step| !step.is_zero() && step.abs_diff(wall_step) <= MAX_AGE)
                    .unwrap_or(wall_step);
                // At least one sample tick when a gateway repeats both timestamps.
                old_mapped.saturating_add(step.max(Duration::from_nanos(
                    1_000_000_000_u64.div_ceil(u64::from(rate.max(1))),
                )))
            },
        );
        self.previous = Some((received, source, mapped));
        mapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(stream: StreamKind, format: Format, received: Instant, bytes: usize) -> Packet {
        Packet {
            stream,
            format,
            received,
            timestamp: Some(Duration::ZERO),
            bytes,
        }
    }

    const AUDIO: Format = Format::Audio(WebRtcAudioCodec::Aac, 48_000, 1);
    const VIDEO: Format = Format::Video(VideoCodec::H264);

    #[test]
    fn signed_preview_is_local_and_only_the_chosen_medium_waits() {
        let camera = Arc::new(CameraTiming::default());
        camera.configure(LiveAudioSettings {
            default_offset_ms: Some(200),
            sub_offset_ms: Some(0),
            ..Default::default()
        });
        let t = Instant::now();
        let mut queue = Queue::new(camera.clone());
        let mut other = Queue::new(camera);
        assert_eq!(queue.report(StreamKind::Sub).applied_offset_ms, 0);
        assert_eq!(
            queue.admit(1, packet(StreamKind::Main, AUDIO, t, 1024), t),
            None
        );
        assert_eq!(queue.pop_due(t + Duration::from_millis(199)), None);
        assert_eq!(
            queue.pop_due(t + Duration::from_millis(200)),
            Some((1, t + Duration::from_millis(200)))
        );
        assert_eq!(
            queue.admit(2, packet(StreamKind::Main, VIDEO, t, 1024), t),
            Some((2, t))
        );
        queue.set_preview(Some(-200));
        assert_eq!(queue.report(StreamKind::Main).configured_offset_ms, 200);
        assert!(queue.report(StreamKind::Main).preview);
        assert_eq!(
            queue.admit(3, packet(StreamKind::Main, AUDIO, t, 1024), t),
            Some((3, t))
        );
        assert_eq!(
            queue.admit(4, packet(StreamKind::Main, VIDEO, t, 1024), t),
            None
        );
        assert_eq!(
            other.admit(4, packet(StreamKind::Main, VIDEO, t, 1024), t),
            Some((4, t))
        );
        queue.set_preview(None);
        assert!(queue.deadline().is_none());
        assert!(!queue.report(StreamKind::Main).preview);
    }

    #[test]
    fn multiple_viewers_share_per_stream_caps_and_return_reservations_on_drop() {
        let camera = Arc::new(CameraTiming::default());
        for (format, offset, bytes) in [
            (AUDIO, 200, MAX_AUDIO_BYTES - size_of::<Held<i32>>()),
            (VIDEO, -200, MAX_VIDEO_BYTES),
        ] {
            let t = Instant::now();
            let mut first = Queue::new(camera.clone());
            let mut second = Queue::new(camera.clone());
            first.set_preview(Some(offset));
            second.set_preview(Some(offset));
            first.admit(1, packet(StreamKind::Main, format, t, bytes), t);
            second.admit(2, packet(StreamKind::Main, format, t, 1), t);
            assert!(second.deadline().is_none());
            second.admit(3, packet(StreamKind::Sub, format, t, bytes), t);
            assert_eq!(second.pop_due(t + Duration::from_millis(200)).unwrap().0, 3);
            drop(first);
            second.admit(4, packet(StreamKind::Main, format, t, bytes), t);
            assert_eq!(second.pop_due(t + Duration::from_millis(200)).unwrap().0, 4);
        }
        let mut video = Queue::new(camera.clone());
        video.set_preview(Some(-200));
        let t = Instant::now();
        for n in 0..MAX_FRAMES + 6 {
            video.admit(n, packet(StreamKind::Main, VIDEO, t, 1), t);
        }
        assert_eq!(video.profiles[0].frames.len(), MAX_FRAMES);
        assert_eq!(video.pop_due(t + MAX_AGE + Duration::from_millis(1)), None);
        assert_eq!(
            camera.budget(StreamKind::Main, false).lock().unwrap().bytes,
            0
        );
    }

    #[test]
    fn pending_profile_cannot_flush_active_frames_during_an_adaptive_switch() {
        let camera = Arc::new(CameraTiming::default());
        camera.configure(LiveAudioSettings {
            main_offset_ms: Some(-180),
            sub_offset_ms: Some(-60),
            ..Default::default()
        });
        let now = Instant::now();
        let mut queue = Queue::new(camera);
        queue.admit(1, packet(StreamKind::Main, VIDEO, now, 1), now);
        queue.admit(2, packet(StreamKind::Sub, VIDEO, now, 1), now);
        assert_eq!(
            queue.pop_due(now + Duration::from_millis(60)),
            Some((2, now + Duration::from_millis(60)))
        );
        assert_eq!(
            queue.pop_due(now + Duration::from_millis(180)),
            Some((1, now + Duration::from_millis(180)))
        );
    }

    #[test]
    fn policy_epoch_codec_or_backwards_source_time_discards_held_media() {
        let camera = Arc::new(CameraTiming::default());
        let mut q = Queue::new(camera.clone());
        q.set_preview(Some(100));
        let t = Instant::now();
        q.admit(1, packet(StreamKind::Main, AUDIO, t, 1), t);
        camera.reset();
        assert!(q.pop_due(t + Duration::from_millis(121)).is_none());
        let t = Instant::now();
        let mut old = packet(StreamKind::Main, AUDIO, t, 1);
        old.timestamp = Some(Duration::from_secs(90));
        q.admit(2, old, t);
        q.admit(3, packet(StreamKind::Main, AUDIO, t, 1), t);
        assert_eq!(q.profiles[0].frames.len(), 1);
        q.admit(
            4,
            packet(
                StreamKind::Main,
                Format::Audio(WebRtcAudioCodec::G711Ulaw, 8_000, 1),
                t,
                1,
            ),
            t,
        );
        assert_eq!(q.pop_due(t + Duration::from_millis(100)).unwrap().0, 4);
        assert!(q.deadline().is_none());
    }

    #[test]
    fn paired_source_events_with_known_skew_reach_the_same_listener_deadline() {
        let mut audio = Queue::new(Arc::new(CameraTiming::default()));
        let mut video = Queue::new(audio.camera.clone());
        let base = Instant::now();
        for offset in [120_i32, -120] {
            audio.set_preview(Some(offset));
            video.set_preview(Some(offset));
            for (index, elapsed) in [0, 40, 330, 350, 1_000].into_iter().enumerate() {
                let first = base + Duration::from_millis(elapsed);
                let last = first + Duration::from_millis(120);
                let (audio_at, video_at) = if offset > 0 {
                    (first, last)
                } else {
                    (last, first)
                };
                let audio_due = audio
                    .admit(
                        index,
                        packet(StreamKind::Main, AUDIO, audio_at, 256 + index * 33),
                        audio_at,
                    )
                    .or_else(|| audio.pop_due(last));
                let video_due = video
                    .admit(
                        index,
                        packet(StreamKind::Main, VIDEO, video_at, 16_384 + index * 507),
                        video_at,
                    )
                    .or_else(|| video.pop_due(last));
                assert_eq!(audio_due.unwrap(), (index, last));
                assert_eq!(video_due.unwrap(), (index, last));
            }
        }
    }

    #[test]
    #[ignore = "run explicitly for paired server-side live timing measurements"]
    fn measure_live_listener_timing() {
        let base = Instant::now();
        for run in 0..=30 {
            let offsets = if run % 2 == 0 {
                [0_i32, 120, -120]
            } else {
                [-120_i32, 120, 0]
            };
            for offset in offsets {
                let camera = Arc::new(CameraTiming::default());
                let mut a = Queue::new(camera.clone());
                let mut v = Queue::new(camera);
                a.set_preview(Some(offset));
                v.set_preview(Some(offset));
                let mut peak_audio = 0;
                let mut peak_video = 0;
                let mut delivered = 0;
                let start = Instant::now();
                for step in 0..5120 {
                    let at = base + Duration::from_millis(step * 20);
                    while a.pop_due(at).is_some() {
                        delivered += 1;
                    }
                    while v.pop_due(at).is_some() {
                        delivered += 1;
                    }
                    if step < 5000 {
                        delivered += usize::from(
                            a.admit(step, packet(StreamKind::Main, AUDIO, at, 512), at)
                                .is_some(),
                        );
                        if step % 2 == 0 {
                            delivered += usize::from(
                                v.admit(step, packet(StreamKind::Main, VIDEO, at, 32_768), at)
                                    .is_some(),
                            );
                        }
                    }
                    if step < 10 {
                        let stats = a.report(StreamKind::Main);
                        peak_audio = peak_audio.max(stats.held_audio_bytes);
                        peak_video = peak_video.max(stats.held_video_bytes);
                    }
                }
                let elapsed_ns = start.elapsed().as_nanos();
                assert_eq!(delivered, 7500);
                if run > 0 {
                    println!("live-timing,{run},{offset},{elapsed_ns},{peak_audio},{peak_video}");
                }
            }
        }
    }

    #[test]
    fn source_reset_and_missing_audio_timestamps_keep_the_listener_clock_advancing() {
        let t = Instant::now();
        let mut clock = AudioClock::default();
        let mut last = clock.map(t, Some(Duration::from_secs(90)), 8000);
        for (ms, pts) in [
            (10, Some(Duration::ZERO)),
            (10, Some(Duration::ZERO)),
            (30, None),
            (40, None),
            (50, Some(Duration::from_millis(20))),
            (60, Some(Duration::from_secs(9_000))),
            (
                70,
                Some(Duration::from_secs(9_000) + Duration::from_millis(10)),
            ),
        ] {
            let next = clock.map(t + Duration::from_millis(ms), pts, 8000);
            assert!(next > last);
            assert!(next - last <= Duration::from_millis(30));
            last = next;
        }
    }
}
