//! Bounded presentation delay. A stream shares its byte and frame budget across all listeners.

use super::{StreamKind, WebRtcAudioCodec};
use crate::{api::proto, cameras::LiveAudioSettings, storage::VideoCodec};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

// One profile can delay either medium by up to 200 ms. An extra 50 ms allows the
// session poller to deliver an eligible frame before latency forces a drop.
pub(super) const MAX_AGE: Duration = Duration::from_millis(250);
const _: () = assert!(MAX_AGE.as_millis() >= crate::cameras::MAX_LIVE_AUDIO_OFFSET_MS as u128 + 50);
// Per camera profile across listeners: bound encoded video bursts by both byte
// count and frame count, and reserve a separate smaller budget for audio units.
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
    settings: RwLock<[StreamPolicy; 2]>,
    audio: [Mutex<Usage>; 2],
    video: [Mutex<Usage>; 2],
}

#[derive(Clone, Copy, Default)]
struct StreamPolicy {
    offset: i32,
    reset_at: Option<Instant>,
    source_reset_at: Option<Instant>,
    generation: u64,
}

#[derive(Default)]
struct Usage {
    payloads: HashMap<PayloadKey, usize>,
    stats: Counters,
    reserved_bytes: usize,
}

#[derive(Default, Clone, Copy)]
struct Counters {
    expired_drops: u64,
    capacity_drops: u64,
    bytes: usize,
    frames: usize,
}

impl StreamPolicy {
    const fn reset(&mut self, now: Instant) {
        self.generation = self.generation.wrapping_add(1);
        self.reset_at = Some(now);
    }
}

impl CameraTiming {
    pub(super) fn configure(&self, settings: LiveAudioSettings) {
        let mut policy = self
            .settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        for (profile, configured) in policy
            .iter_mut()
            .zip([settings.main_offset_ms, settings.sub_offset_ms])
        {
            let offset = configured.or(settings.default_offset_ms).unwrap_or(0);
            if profile.offset != offset {
                profile.offset = offset;
                profile.reset(now);
            }
        }
    }

    pub(super) fn reset(&self) {
        let now = Instant::now();
        for profile in self
            .settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter_mut()
        {
            profile.source_reset_at = Some(now);
            profile.reset(now);
        }
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
        policy[usize::from(stream == StreamKind::Sub)]
    }

    fn record_expired(&self, stream: StreamKind, audio: bool) {
        let mut usage = self
            .budget(stream, audio)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        usage.stats.expired_drops = usage.stats.expired_drops.saturating_add(1);
    }

    pub(super) fn report(
        &self,
        stream: StreamKind,
        preview: Option<i32>,
    ) -> proto::LiveAudioTiming {
        let configured_offset_ms = self.policy(stream).offset;
        let usage = |audio| {
            self.budget(stream, audio)
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .stats
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

/// A capture retains its backing allocation while any queue holds it. The address
/// is only an in-process identity and is never dereferenced or exposed.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PayloadKey {
    address: usize,
    bytes: usize,
    received: Instant,
}

struct Reservation {
    camera: Arc<CameraTiming>,
    stream: StreamKind,
    audio: bool,
    key: PayloadKey,
}

impl Reservation {
    fn acquire<T>(
        camera: &Arc<CameraTiming>,
        stream: StreamKind,
        audio: bool,
        key: PayloadKey,
        frames: &mut VecDeque<Held<T>>,
    ) -> Option<Self> {
        let mut usage = camera
            .budget(stream, audio)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let limit = if audio {
            MAX_AUDIO_BYTES
        } else {
            MAX_VIDEO_BYTES
        };
        let first = !usage.payloads.contains_key(&key);
        let payload_charge = if first { key.bytes } else { 0 };
        let available = limit - usage.reserved_bytes;
        let slots = if key.bytes == 0 || (!audio && first && usage.stats.frames == MAX_FRAMES) {
            None
        } else {
            available
                .checked_sub(payload_charge)
                .and_then(|available| reserve_queue_slot(frames, available))
        };
        let Some(added_slot_bytes) = slots else {
            usage.stats.capacity_drops = usage.stats.capacity_drops.saturating_add(1);
            return None;
        };
        *usage.payloads.entry(key).or_insert(0) += 1;
        if first {
            usage.stats.frames += 1;
            usage.stats.bytes += key.bytes;
        }
        usage.reserved_bytes += payload_charge + added_slot_bytes;
        Some(Self {
            camera: camera.clone(),
            stream,
            audio,
            key,
        })
    }

    fn release_slots<T>(&self, count: usize) {
        let mut usage = self
            .camera
            .budget(self.stream, self.audio)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        usage.reserved_bytes -= count * std::mem::size_of::<Held<T>>();
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut usage = self
            .camera
            .budget(self.stream, self.audio)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let viewers = usage
            .payloads
            .get_mut(&self.key)
            .expect("reservation owns a payload reference");
        *viewers -= 1;
        if *viewers == 0 {
            usage.payloads.remove(&self.key);
            usage.stats.frames -= 1;
            usage.stats.bytes -= self.key.bytes;
            usage.reserved_bytes -= self.key.bytes;
        }
    }
}

/// Grow only when the profile can afford both the current and the replacement allocation.
fn reserve_queue_slot<T>(frames: &mut VecDeque<Held<T>>, available: usize) -> Option<usize> {
    let old = frames.capacity();
    if frames.len() < old {
        return Some(0);
    }
    let slot_bytes = std::mem::size_of::<Held<T>>();
    let affordable = available / slot_bytes;
    let desired = old.saturating_mul(2).max(1).min(affordable);
    if desired <= old {
        return None;
    }
    let mut replacement = VecDeque::new();
    replacement.try_reserve_exact(desired).ok()?;
    let actual = replacement.capacity();
    if actual > affordable {
        return None;
    }
    replacement.append(frames);
    *frames = replacement;
    Some((actual - old) * slot_bytes)
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
        self.discard_frames();
        self.state = None;
        self.last_timestamp = None;
    }

    fn pop_front(&mut self) -> Option<Held<T>> {
        let frame = self.frames.pop_front()?;
        if self.frames.is_empty() {
            let slots = self.frames.capacity();
            self.frames = VecDeque::new();
            frame.reservation.release_slots::<T>(slots);
        }
        Some(frame)
    }

    fn discard_frames(&mut self) {
        while self.pop_front().is_some() {}
    }
}

impl<T> Drop for ProfileQueue<T> {
    fn drop(&mut self) {
        self.discard_frames();
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

    pub(super) fn set_preview(&mut self, preview: Option<i32>) -> bool {
        if self.preview == preview {
            return false;
        }
        self.clear();
        self.preview = preview;
        true
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
                self.profiles[index].pop_front();
                continue;
            }
            if head.due > now {
                break;
            }
            let head = self.profiles[index].pop_front().expect("head was present");
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
            payload_address,
        } = packet;
        let policy = self.camera.policy(stream);
        if policy.source_reset_at.is_some_and(|reset| received < reset) {
            return None;
        }
        let profile = &mut self.profiles[usize::from(stream == StreamKind::Sub)];
        let state = (format, policy.generation);
        if profile.state != Some(state)
            || matches!(format, Format::Audio(..))
                && timestamp
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
        if policy.reset_at.is_some_and(|reset| received < reset) {
            return None;
        }
        if age > MAX_AGE {
            self.camera.record_expired(stream, audio);
            return None;
        }
        let due = received + Duration::from_millis(u64::from(ms.unsigned_abs()));
        if due <= now && profile.frames.is_empty() {
            return Some((item, due));
        }
        let reservation = Reservation::acquire::<T>(
            &self.camera,
            stream,
            audio,
            PayloadKey {
                address: payload_address,
                bytes,
                received,
            },
            &mut profile.frames,
        )?;
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
    pub(super) payload_address: usize,
}

/// Maps a sender reset onto the current listener timeline. The original capture timestamp
/// remains on the ingested packet and in the recording.
#[derive(Default)]
pub(super) struct AudioClock {
    previous: Option<(Instant, Option<Duration>, Duration)>,
    previous_unit_duration: Option<Duration>,
}

impl AudioClock {
    pub(super) const fn reset_source(&mut self) {
        if let Some((_, previous_source, _)) = &mut self.previous {
            *previous_source = None;
        }
    }

    pub(super) fn map(
        &mut self,
        received: Instant,
        source: Option<Duration>,
        rate: u32,
        unit_duration: Option<Duration>,
    ) -> Duration {
        let mapped = self.previous.map_or_else(
            || source.unwrap_or_default(),
            |(old_wall, old_source, old_mapped)| {
                let wall_step = received.saturating_duration_since(old_wall);
                let step = old_source
                    .zip(source)
                    .and_then(|(old, next)| next.checked_sub(old))
                    .filter(|step| !step.is_zero() && step.abs_diff(wall_step) <= MAX_AGE)
                    .unwrap_or_else(|| {
                        if wall_step > MAX_AGE {
                            wall_step
                        } else {
                            self.previous_unit_duration.unwrap_or(wall_step)
                        }
                    });
                // At least one sample tick when a gateway repeats both timestamps.
                old_mapped.saturating_add(
                    step.max(self.previous_unit_duration.unwrap_or_default())
                        .max(Duration::from_nanos(
                            1_000_000_000_u64.div_ceil(u64::from(rate.max(1))),
                        )),
                )
            },
        );
        self.previous_unit_duration = unit_duration;
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
            payload_address: bytes,
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
    fn successive_listeners_release_actual_queue_capacity_without_unsubscribing() {
        let camera = Arc::new(CameraTiming::default());
        let now = Instant::now();
        let due = now + Duration::from_millis(120);
        let mut listeners = (0..12)
            .map(|_| Queue::new(camera.clone()))
            .collect::<Vec<_>>();
        for index in 0..listeners.len() {
            let listener = &mut listeners[index];
            listener.set_preview(Some(120));
            // The last entry remains pending, so earlier pops must keep the ring charged.
            for i in 0..96_u32 {
                let arrival = if i == 95 {
                    now + Duration::from_millis(1)
                } else {
                    now
                };
                listener.admit(i, packet(StreamKind::Main, AUDIO, arrival, 16), arrival);
            }
            assert!(listener.profiles[0].frames.capacity() >= 96);
            while listener.pop_due(due).is_some() {}
            assert_eq!(listener.profiles[0].frames.len(), 1);
            let slots = listener.profiles[0].frames.capacity();
            let usage = camera.budget(StreamKind::Main, true).lock().unwrap();
            assert_eq!(
                usage.reserved_bytes,
                16 + slots * std::mem::size_of::<Held<u32>>()
            );
            drop(usage);
            if index % 2 == 0 {
                assert_eq!(
                    listener.pop_due(due + Duration::from_millis(1)).unwrap().0,
                    95
                );
            } else {
                listener.clear();
            }
            assert!(
                listeners
                    .iter()
                    .all(|l| l.profiles[0].frames.capacity() == 0)
            );
            assert_eq!(
                camera
                    .budget(StreamKind::Main, true)
                    .lock()
                    .unwrap()
                    .reserved_bytes,
                0
            );
        }
    }

    #[test]
    fn multiple_viewers_share_per_stream_caps_and_return_reservations_on_drop() {
        let camera = Arc::new(CameraTiming::default());
        for (format, offset, bytes) in [
            (
                AUDIO,
                200,
                MAX_AUDIO_BYTES - std::mem::size_of::<Held<i32>>(),
            ),
            (
                VIDEO,
                -200,
                MAX_VIDEO_BYTES - std::mem::size_of::<Held<i32>>(),
            ),
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
            let mut frame = packet(StreamKind::Main, VIDEO, t, 1);
            frame.payload_address = n;
            video.admit(n, frame, t);
        }
        assert_eq!(video.profiles[0].frames.len(), MAX_FRAMES);
        assert_eq!(video.pop_due(t + MAX_AGE + Duration::from_millis(1)), None);
        assert_eq!(
            camera
                .budget(StreamKind::Main, false)
                .lock()
                .unwrap()
                .stats
                .bytes,
            0
        );
    }

    #[test]
    fn fresh_frames_after_the_poll_snapshot_are_scheduled_and_shared_across_viewers() {
        let camera = Arc::new(CameraTiming::default());
        let mut first = Queue::new(camera.clone());
        let mut second = Queue::new(camera);
        first.set_preview(Some(-120));
        second.set_preview(Some(-120));
        let poll = Instant::now();
        let arrival = poll + Duration::from_millis(1);
        let bytes = MAX_VIDEO_BYTES * 3 / 4;
        first.admit(1, packet(StreamKind::Main, VIDEO, arrival, bytes), poll);
        second.admit(2, packet(StreamKind::Main, VIDEO, arrival, bytes), poll);
        let stats = first.report(StreamKind::Main);
        assert_eq!(
            (
                stats.held_video_frames,
                stats.held_video_bytes,
                stats.expired_drops,
                stats.capacity_drops
            ),
            (1, bytes as u32, 0, 0)
        );
        let due = arrival + Duration::from_millis(120);
        assert_eq!(first.pop_due(due), Some((1, due)));
        assert_eq!(second.report(StreamKind::Main).held_video_frames, 1);
        assert_eq!(second.pop_due(due), Some((2, due)));
        assert_eq!(second.report(StreamKind::Main).held_video_bytes, 0);
    }

    #[test]
    fn audio_only_save_preserves_inflight_video_until_the_source_itself_resets() {
        let camera = Arc::new(CameraTiming::default());
        let mut queue = Queue::new(camera.clone());
        let earlier = Instant::now() - Duration::from_millis(10);
        camera.configure(LiveAudioSettings {
            main_offset_ms: Some(80),
            ..Default::default()
        });
        let now = Instant::now();
        assert_eq!(
            queue.admit(1, packet(StreamKind::Main, VIDEO, earlier, 1), now),
            Some((1, earlier))
        );
        assert_eq!(
            queue.admit(2, packet(StreamKind::Main, AUDIO, earlier, 1), now),
            None
        );
        camera.reset();
        assert_eq!(
            queue.admit(3, packet(StreamKind::Main, VIDEO, earlier, 1), now),
            None
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
    fn delayed_video_preserves_decode_order_across_b_frames_and_other_profile_changes() {
        let camera = Arc::new(CameraTiming::default());
        camera.configure(LiveAudioSettings {
            main_offset_ms: Some(-180),
            ..Default::default()
        });
        let mut queue = Queue::new(camera.clone());
        let start = Instant::now();
        for (i, pts) in [0, 120, 40, 80].into_iter().enumerate() {
            let received = start + Duration::from_millis(i as u64 * 10);
            let mut frame = packet(StreamKind::Main, VIDEO, received, 100);
            frame.timestamp = Some(Duration::from_millis(pts));
            assert!(queue.admit(i, frame, received).is_none());
        }
        let generation = camera.policy(StreamKind::Main).generation;
        camera.configure(LiveAudioSettings {
            main_offset_ms: Some(-180),
            sub_offset_ms: Some(30),
            ..Default::default()
        });
        assert_eq!(camera.policy(StreamKind::Main).generation, generation);
        for i in 0..4 {
            let due = start + Duration::from_millis(180 + i * 10);
            assert_eq!(queue.pop_due(due), Some((i as usize, due)));
        }
        let sub_generation = camera.policy(StreamKind::Sub).generation;
        camera.reset();
        assert_ne!(camera.policy(StreamKind::Main).generation, generation);
        assert_ne!(camera.policy(StreamKind::Sub).generation, sub_generation);
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
    fn burst_audio_without_a_usable_clock_advances_by_the_previous_encoded_block() {
        use crate::webrtc::audio::AudioFrame;
        let t = Instant::now();
        let mut clock = AudioClock::default();
        for (bytes, source, expected) in [
            (160, None, 0),
            (80, None, 160),
            (240, Some(Duration::ZERO), 240),
            (160, Some(Duration::ZERO), 480),
        ] {
            let frame = AudioFrame {
                codec: WebRtcAudioCodec::G711Ulaw,
                sample_rate_hz: 8000,
                channel_count: 1,
                timestamp: source,
                duration: None,
                received_at: t,
                data: bytes::Bytes::from(vec![0; bytes]),
            };
            let mapped = clock.map(t, source, 8000, frame.encoded_duration());
            assert_eq!(super::super::duration_to_ticks(mapped, 8000), expected);
        }
        let mut aac = AudioClock::default();
        for i in 0..3 {
            let time = aac.map(t, None, 48_000, Some(Duration::from_millis(20)));
            assert_eq!(time, Duration::from_millis(i * 20));
        }
    }

    #[test]
    fn source_reset_and_missing_audio_timestamps_keep_the_listener_clock_advancing() {
        let t = Instant::now();
        let mut clock = AudioClock::default();
        let mut last = clock.map(t, Some(Duration::from_secs(90)), 8000, None);
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
            let next = clock.map(t + Duration::from_millis(ms), pts, 8000, None);
            assert!(next > last);
            assert!(next - last <= Duration::from_millis(30));
            last = next;
        }
    }
}
