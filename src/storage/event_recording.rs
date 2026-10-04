//! Selects bounded event history and live frames for the existing recording writer.

use super::{
    identity::RecordingStreamIdentity,
    pre_record::{BufferedFrame, HistoryReason, PreRecordBuffers},
    segment::RecordingFrame,
};
use crate::cameras::{CameraRecordingMode, EventRecordingStream};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_CAMERAS: usize = 127;

#[cfg(test)]
mod media_tests;

#[derive(Clone, Copy, Debug)]
pub(super) struct EventSettings {
    pub mode: CameraRecordingMode,
    pub stream: EventRecordingStream,
    pub pre: Duration,
    pub post: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreRecordReason {
    Disabled,
    Startup,
    MissingKeyframe,
    DurationEviction,
    PerStreamPressure,
    GlobalPressure,
    Ready,
    PendingReplay,
    MalformedOrder,
    Discontinuity,
    Privacy,
    StoragePause,
    WriterFailure,
}

#[derive(Clone, Debug)]
pub struct PreRecordStatus {
    pub enabled: bool,
    pub active: bool,
    pub selected_stream: EventRecordingStream,
    pub requested_ms: u64,
    pub available_ms: u64,
    pub retained_bytes: u64,
    pub reason: PreRecordReason,
}

struct QueueLease {
    bytes: usize,
    bytes_counter: Arc<AtomicUsize>,
    frames_counter: Arc<AtomicUsize>,
}

impl Drop for QueueLease {
    fn drop(&mut self) {
        self.bytes_counter.fetch_sub(self.bytes, Ordering::Relaxed);
        self.frames_counter.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) struct QueuedEventFrame {
    pub frame: RecordingFrame,
    lease: QueueLease,
}

impl QueuedEventFrame {
    pub(super) fn try_new(
        frame: RecordingFrame,
        bytes_counter: Arc<AtomicUsize>,
        frames_counter: Arc<AtomicUsize>,
        max_bytes: usize,
        max_frames: usize,
    ) -> Result<Self, RecordingFrame> {
        if frames_counter
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_add(1).filter(|count| *count <= max_frames)
            })
            .is_err()
        {
            return Err(frame);
        }
        let bytes = frame.byte_len();
        if bytes_counter
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_add(bytes).filter(|count| *count <= max_bytes)
            })
            .is_err()
        {
            frames_counter.fetch_sub(1, Ordering::Relaxed);
            return Err(frame);
        }
        Ok(Self {
            frame,
            lease: QueueLease {
                bytes,
                bytes_counter,
                frames_counter,
            },
        })
    }
}

enum PendingFrame {
    History(BufferedFrame),
    Live(QueuedEventFrame),
    Finish(Instant),
}

impl PendingFrame {
    const fn frame(&self) -> Option<&RecordingFrame> {
        match self {
            Self::History(frame) => Some(frame.frame()),
            Self::Live(frame) => Some(&frame.frame),
            Self::Finish(_) => None,
        }
    }

    fn into_frame(self) -> RecordingFrame {
        match self {
            Self::History(frame) => frame.into_frame(),
            Self::Live(frame) => frame.frame,
            Self::Finish(_) => unreachable!("finish markers are handled before frame extraction"),
        }
    }
}

pub(super) enum EventOutput {
    Frame {
        identity: RecordingStreamIdentity,
        frame: RecordingFrame,
    },
    Finish {
        identity: RecordingStreamIdentity,
        end: Instant,
    },
}

struct CameraWindow {
    settings: EventSettings,
    identity: Option<RecordingStreamIdentity>,
    deadline: Option<Instant>,
    pending: VecDeque<PendingFrame>,
    main_active: bool,
    waiting_keyframe: bool,
    window_started: bool,
    last_video: Option<Instant>,
    first_video: Option<Instant>,
    last_audio_end: Option<Instant>,
    watermark: Option<Instant>,
    last_input: [Option<Instant>; 2],
    decoder: [Option<[u8; 32]>; 2],
    queued: Option<Arc<AtomicUsize>>,
    status: PreRecordStatus,
}

pub(super) struct EventRecordings {
    buffers: PreRecordBuffers,
    cameras: BTreeMap<String, CameraWindow>,
    ready: VecDeque<String>,
}

impl EventRecordings {
    #[cfg(feature = "event-preroll-benchmark-current")]
    pub(super) fn retained_bytes(&self) -> usize {
        self.buffers.bytes()
    }

    #[cfg(feature = "event-preroll-benchmark-current")]
    pub(super) fn max_stream_bytes(&self) -> usize {
        self.buffers.max_stream_bytes()
    }

    pub(super) fn drop_main_history(&mut self, source: &str) {
        self.invalidate_main_history(source, PreRecordReason::GlobalPressure);
    }

    fn invalidate_main_history(&mut self, source: &str, reason: PreRecordReason) {
        self.buffers.clear_stream(source, "main");
        if let Some(camera) = self.cameras.get_mut(source) {
            camera.decoder[0] = None;
            camera.last_input[0] = None;
            camera.status.available_ms = 0;
            camera.status.reason = reason;
            if camera.main_active {
                camera.waiting_keyframe = true;
            }
        }
    }

    fn reject_input(&mut self, identity: &RecordingStreamIdentity, reason: PreRecordReason) {
        if identity.stream_id == "main"
            && self
                .cameras
                .get(&identity.source_id)
                .is_some_and(|camera| camera.settings.mode == CameraRecordingMode::EventBoost)
        {
            self.invalidate_main_history(&identity.source_id, reason);
        } else {
            self.fail(&identity.source_id, reason);
        }
    }

    pub(super) fn new(stream_bytes: usize, global_bytes: usize) -> Self {
        Self {
            buffers: PreRecordBuffers::new(stream_bytes, global_bytes),
            cameras: BTreeMap::new(),
            ready: VecDeque::new(),
        }
    }

    pub(super) fn begin_shutdown(&mut self, now: Instant) {
        self.release_continuous();
        for (source, camera) in &mut self.cameras {
            if camera.settings.mode == CameraRecordingMode::EventBoost && !camera.main_active {
                for frame in self.buffers.take(source, "sub", now) {
                    camera.select(PendingFrame::History(frame));
                }
            }
            if let Some(deadline) = camera.deadline {
                camera.deadline = Some(deadline.min(now));
            }
            if camera.settings.mode == CameraRecordingMode::EventBoost {
                camera.deadline = Some(now);
            }
        }
    }

    pub(super) fn configure(&mut self, source: &str, settings: EventSettings) -> bool {
        if !matches!(
            settings.mode,
            CameraRecordingMode::EventOnly | CameraRecordingMode::EventBoost
        ) || (settings.mode == CameraRecordingMode::EventBoost && settings.pre.is_zero())
        {
            self.cameras.remove(source);
            self.ready.retain(|camera| camera != source);
            self.buffers.remove_source(source);
            return true;
        }
        if source.is_empty()
            || source.len() > 256
            || settings.pre > Duration::from_secs(30)
            || settings.post.is_zero()
            || settings.post > Duration::from_secs(3600)
        {
            return false;
        }
        if !self.cameras.contains_key(source) && self.cameras.len() >= MAX_CAMERAS {
            return false;
        }
        if !matches!(
            settings.mode,
            CameraRecordingMode::EventOnly | CameraRecordingMode::EventBoost
        ) {
            return false;
        }
        self.buffers.clear_source(source);
        let stream = selected_stream(settings);
        if !settings.pre.is_zero() {
            if !self.buffers.configure(source, stream, settings.pre) {
                return false;
            }
            if settings.mode == CameraRecordingMode::EventBoost {
                if !self.buffers.configure(source, "sub", settings.pre) {
                    return false;
                }
                self.buffers.set_continuous(source, "sub", true);
            }
        }
        if !self.cameras.contains_key(source) {
            self.ready.push_back(source.to_owned());
        }
        self.cameras
            .insert(source.to_owned(), CameraWindow::new(settings));
        true
    }

    pub(super) fn reset(&mut self, source: &str) {
        self.buffers.clear_source(source);
        if let Some(camera) = self.cameras.get_mut(source) {
            let settings = camera.settings;
            *camera = CameraWindow::new(settings);
            camera.status.reason = PreRecordReason::Discontinuity;
            self.buffers.set_continuous(
                source,
                "sub",
                settings.mode == CameraRecordingMode::EventBoost,
            );
        }
    }

    pub(super) fn fail(&mut self, source: &str, reason: PreRecordReason) {
        self.reset(source);
        if let Some(camera) = self.cameras.get_mut(source) {
            camera.status.reason = reason;
        }
    }

    pub(super) fn note_event(&mut self, source: &str, now: Instant) {
        self.release_continuous();
        let Some(camera) = self.cameras.get_mut(source) else {
            return;
        };
        let overlaps = camera.deadline.is_some_and(|deadline| now < deadline);
        if !overlaps && camera.settings.mode == CameraRecordingMode::EventOnly {
            if let Some(end) = camera.deadline.take()
                && camera.window_started
            {
                camera.close_pending_window(end);
            }
            camera.waiting_keyframe = true;
            camera.first_video = None;
            camera.last_audio_end = None;
        }
        camera.deadline = now.checked_add(camera.settings.post);
        camera.status.active = true;
        if overlaps || camera.main_active {
            return;
        }
        self.select_history(source, now);
    }

    fn select_history(&mut self, source: &str, now: Instant) {
        let Some(camera) = self.cameras.get_mut(source) else {
            return;
        };
        let stream = selected_stream(camera.settings);
        let snapshot = self.buffers.snapshot(source, stream, now);
        let mut replay = self.buffers.take(source, stream, now);
        let first = replay.iter().position(|frame| {
            frame.frame().is_video_keyframe()
                && camera
                    .watermark
                    .is_none_or(|watermark| frame.frame().received_at > watermark)
        });
        let Some(first) = first else {
            return;
        };
        replay.drain(..first);
        let start = replay[0].frame().received_at;
        if camera.settings.mode == CameraRecordingMode::EventBoost {
            camera.trim_pending_audio(start);
            self.buffers.set_continuous(source, "sub", false);
            for frame in self.buffers.take(source, "sub", now) {
                if frame_ends_before(frame.frame(), start) {
                    camera.select(PendingFrame::History(frame));
                }
            }
            camera.main_active = true;
        }
        camera.waiting_keyframe = false;
        let covered_end = replay
            .iter()
            .rev()
            .find(|frame| frame.frame().frame.is_video())
            .map_or(start, |frame| frame.frame().received_at);
        camera.status.available_ms = milliseconds(covered_end.saturating_duration_since(start));
        camera.status.reason = snapshot.map_or(PreRecordReason::Startup, |snapshot| {
            history_reason(snapshot.reason)
        });
        for frame in replay {
            camera.select(PendingFrame::History(frame));
        }
    }

    pub(super) fn ingest(
        &mut self,
        identity: RecordingStreamIdentity,
        input: QueuedEventFrame,
        now: Instant,
    ) {
        let source = identity.source_id.clone();
        if self.cameras.get(&source).is_some_and(|camera| {
            camera.settings.mode == CameraRecordingMode::EventOnly
                && identity.stream_id != selected_stream(camera.settings)
        }) {
            return;
        }
        if !self.validate(&identity, &input.frame, now) {
            return;
        }
        let camera = self
            .cameras
            .get_mut(&source)
            .expect("validated source is configured");
        camera.queued = Some(Arc::clone(&input.lease.frames_counter));
        camera.identity = Some(if camera.settings.mode == CameraRecordingMode::EventBoost {
            identity.clone().with_recording_stream("sub")
        } else {
            identity.clone()
        });
        let only = camera.settings.mode == CameraRecordingMode::EventOnly;
        let at = input.frame.received_at;
        if only && camera.deadline.is_some_and(|deadline| at < deadline) {
            camera.select(PendingFrame::Live(input));
            return;
        }
        if !only && camera.main_active {
            let switches = identity.stream_id == "sub"
                && input.frame.is_video_keyframe()
                && camera.deadline.is_none_or(|deadline| at >= deadline)
                && camera.watermark.is_none_or(|watermark| at > watermark);
            if switches {
                camera.trim_pending_audio(at);
                camera.main_active = false;
                camera.deadline = None;
                camera.status.active = false;
                self.buffers.set_continuous(&source, "sub", true);
            } else {
                if identity.stream_id == "main" {
                    camera.select(PendingFrame::Live(input));
                }
                return;
            }
        }
        if camera.settings.pre.is_zero() {
            return;
        }
        self.buffer_input(identity, input, now, only);
    }

    fn buffer_input(
        &mut self,
        identity: RecordingStreamIdentity,
        input: QueuedEventFrame,
        now: Instant,
        only: bool,
    ) {
        let source = identity.source_id.clone();
        let at = input.frame.received_at;
        let QueuedEventFrame { frame, lease } = input;
        let rejected = self
            .buffers
            .push_owned(&source, &identity.stream_id, frame, now)
            .err();
        self.release_continuous();
        let camera = self
            .cameras
            .get_mut(&source)
            .expect("configured camera remains registered");
        if let Some(frame) = rejected
            && !only
            && identity.stream_id == "sub"
        {
            camera.select(PendingFrame::Live(QueuedEventFrame { frame, lease }));
        }
        if !only
            && !camera.main_active
            && identity.stream_id == "main"
            && camera.deadline.is_some_and(|deadline| at < deadline)
        {
            self.select_history(&source, now);
        }
    }

    fn validate(
        &mut self,
        identity: &RecordingStreamIdentity,
        frame: &RecordingFrame,
        now: Instant,
    ) -> bool {
        let Some(camera) = self.cameras.get_mut(&identity.source_id) else {
            return false;
        };
        let index = match identity.stream_id.as_str() {
            "main" => 0,
            "sub" => 1,
            _ => return false,
        };
        if frame.received_at > now
            || camera.last_input[index].is_some_and(|last| frame.received_at < last)
        {
            self.reject_input(identity, PreRecordReason::MalformedOrder);
            return false;
        }
        camera.last_input[index] = Some(frame.received_at);
        if frame.frame.is_audio() && camera.decoder[index].is_none() {
            return false;
        }
        if let super::frame::MediaFrame::Video(video) = &frame.frame {
            if video.is_keyframe {
                let Ok(config) = super::medium_term::required_video_media_config(video) else {
                    self.reject_input(identity, PreRecordReason::MissingKeyframe);
                    return false;
                };
                let fingerprint = decoder_fingerprint(config);
                if camera.decoder[index].is_some_and(|previous| previous != fingerprint) {
                    self.buffers
                        .clear_stream(&identity.source_id, &identity.stream_id);
                    camera.status.reason = PreRecordReason::Discontinuity;
                }
                camera.decoder[index] = Some(fingerprint);
                if !camera.settings.pre.is_zero()
                    && !matches!(
                        camera.status.reason,
                        PreRecordReason::Ready
                            | PreRecordReason::PendingReplay
                            | PreRecordReason::DurationEviction
                            | PreRecordReason::PerStreamPressure
                            | PreRecordReason::GlobalPressure
                    )
                {
                    camera.status.reason = PreRecordReason::Startup;
                }
            } else if camera.decoder[index].is_none() {
                if identity.stream_id == selected_stream(camera.settings) {
                    camera.status.reason = PreRecordReason::MissingKeyframe;
                }
                return false;
            }
        }
        true
    }

    fn release_continuous(&mut self) {
        while let Some((source, _, frame)) = self.buffers.next_released() {
            if let Some(camera) = self.cameras.get_mut(&source)
                && !camera.main_active
            {
                camera.select(PendingFrame::History(frame));
            }
        }
    }

    pub(super) fn next_output(&mut self, now: Instant) -> Option<EventOutput> {
        self.buffers.expire(now);
        self.release_continuous();
        for _ in 0..self.ready.len() {
            let source = self
                .ready
                .pop_front()
                .expect("round robin queue has a camera");
            let output = self
                .cameras
                .get_mut(&source)
                .and_then(|camera| camera.next_output(now));
            self.ready.push_back(source);
            if output.is_some() {
                return output;
            }
        }
        None
    }

    pub(super) fn status(&mut self, source: &str, now: Instant) -> Option<PreRecordStatus> {
        let camera = self.cameras.get_mut(source)?;
        let mut status = camera.status.clone();
        if let Some(snapshot) = self
            .buffers
            .snapshot(source, selected_stream(camera.settings), now)
        {
            status.retained_bytes =
                u64::try_from(snapshot.retained_bytes + snapshot.pending_bytes).unwrap_or(u64::MAX);
            if !status.active
                && (snapshot.reason != HistoryReason::Startup
                    || status.reason == PreRecordReason::Startup)
                && matches!(
                    status.reason,
                    PreRecordReason::Startup
                        | PreRecordReason::Ready
                        | PreRecordReason::PendingReplay
                        | PreRecordReason::DurationEviction
                        | PreRecordReason::PerStreamPressure
                        | PreRecordReason::GlobalPressure
                )
            {
                status.available_ms = milliseconds(snapshot.available);
                status.reason = history_reason(snapshot.reason);
            }
            if snapshot.pending_bytes > 0 {
                status.reason = PreRecordReason::PendingReplay;
            }
        }
        Some(status)
    }
}

fn decoder_fingerprint(config: mp4::MediaConfig) -> [u8; 32] {
    // Parameter sets can exceed the history budget; retain only their epoch identity.
    match config {
        mp4::MediaConfig::AvcConfig(config) => hash_decoder_parameters(
            1,
            config.width,
            config.height,
            &[&config.seq_param_set, &config.pic_param_set],
        ),
        mp4::MediaConfig::HevcConfig(config) => hash_decoder_parameters(
            2,
            config.width,
            config.height,
            &[
                &config.vps,
                &config.sps,
                &config.pps,
                &config.decoder_config,
            ],
        ),
        _ => unreachable!("the video parser returns only AVC or HEVC configurations"),
    }
}

fn hash_decoder_parameters(codec: u8, width: u16, height: u16, parameters: &[&[u8]]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update([codec]);
    hash.update(width.to_be_bytes());
    hash.update(height.to_be_bytes());
    for parameter in parameters {
        hash.update(
            u64::try_from(parameter.len())
                .expect("parameter length fits u64")
                .to_be_bytes(),
        );
        hash.update(parameter);
    }
    hash.finalize().into()
}

fn selected_stream(settings: EventSettings) -> &'static str {
    if settings.mode == CameraRecordingMode::EventBoost
        || settings.stream == EventRecordingStream::Main
    {
        "main"
    } else {
        "sub"
    }
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl CameraWindow {
    fn close_pending_window(&mut self, end: Instant) {
        if let Some(video_end) = self.last_video {
            self.trim_pending_audio(video_end);
        }
        self.pending.push_back(PendingFrame::Finish(end));
        self.window_started = false;
    }

    fn trim_pending_audio(&mut self, boundary: Instant) {
        self.pending.retain(|pending| {
            let Some(frame) = pending.frame() else {
                return true;
            };
            match &frame.frame {
                super::frame::MediaFrame::Audio(audio) => frame
                    .received_at
                    .checked_add(audio.duration)
                    .is_some_and(|end| end <= boundary),
                _ => true,
            }
        });
        self.last_audio_end = self.last_audio_end.filter(|end| *end <= boundary);
    }

    fn select(&mut self, pending: PendingFrame) {
        let Some(frame) = pending.frame() else {
            return;
        };
        if frame.frame.is_video() {
            if self.waiting_keyframe && !frame.is_video_keyframe() {
                return;
            }
            if self
                .last_video
                .is_some_and(|last| frame.received_at <= last)
            {
                return;
            }
            self.waiting_keyframe = false;
            self.window_started = true;
            self.first_video.get_or_insert(frame.received_at);
            self.last_video = Some(frame.received_at);
            self.watermark = Some(
                self.watermark
                    .map_or(frame.received_at, |last| last.max(frame.received_at)),
            );
        } else if let super::frame::MediaFrame::Audio(audio) = &frame.frame {
            let Some(end) = frame.received_at.checked_add(audio.duration) else {
                return;
            };
            if audio.duration.is_zero()
                || self
                    .first_video
                    .is_none_or(|start| frame.received_at < start)
                || self
                    .last_audio_end
                    .is_some_and(|last| frame.received_at < last)
                || (self.settings.mode == CameraRecordingMode::EventOnly
                    && self.deadline.is_some_and(|deadline| end > deadline))
            {
                return;
            }
            self.last_audio_end = Some(end);
            // Pending audio cannot extend coverage beyond the selected video.
        }
        self.pending.push_back(pending);
    }

    fn next_output(&mut self, now: Instant) -> Option<EventOutput> {
        if self.identity.is_none() && self.deadline.is_some_and(|end| now >= end) {
            self.deadline = None;
            self.status.active = false;
        }
        let identity = self.identity.clone()?;
        while let Some(front) = self.pending.front() {
            if let PendingFrame::Finish(end) = front {
                let end = *end;
                self.pending.pop_front();
                return Some(EventOutput::Finish { identity, end });
            }
            let frame = front.frame().expect("media entry has a frame");
            if let super::frame::MediaFrame::Audio(audio) = &frame.frame {
                let end = frame.received_at.checked_add(audio.duration)?;
                if self.last_video.is_none_or(|video| end > video) {
                    if self.deadline.is_some_and(|deadline| now >= deadline) {
                        self.pending.pop_front();
                        continue;
                    }
                    return None;
                }
            }
            let frame = self
                .pending
                .pop_front()
                .expect("pending frame exists")
                .into_frame();
            return Some(EventOutput::Frame { identity, frame });
        }
        self.finish_if_due(identity, now)
    }

    fn finish_if_due(
        &mut self,
        identity: RecordingStreamIdentity,
        now: Instant,
    ) -> Option<EventOutput> {
        let end = self.deadline?;
        let index = usize::from(selected_stream(self.settings) == "sub");
        let caught_up = self
            .queued
            .as_ref()
            .is_none_or(|queued| queued.load(Ordering::Relaxed) == 0)
            || self.last_input[index].is_some_and(|last| last >= end);
        if self.settings.mode == CameraRecordingMode::EventOnly && now >= end && caught_up {
            self.deadline = None;
            self.status.active = false;
            self.waiting_keyframe = true;
            if std::mem::take(&mut self.window_started) {
                return Some(EventOutput::Finish { identity, end });
            }
        }
        if self.settings.mode == CameraRecordingMode::EventBoost
            && !self.main_active
            && now >= end
            && caught_up
        {
            self.deadline = None;
            self.status.active = false;
        }
        None
    }

    fn new(settings: EventSettings) -> Self {
        Self {
            settings,
            identity: None,
            deadline: None,
            pending: VecDeque::new(),
            main_active: false,
            waiting_keyframe: true,
            window_started: false,
            last_video: None,
            first_video: None,
            last_audio_end: None,
            watermark: None,
            last_input: [None; 2],
            decoder: [None, None],
            queued: None,
            status: PreRecordStatus {
                enabled: !settings.pre.is_zero(),
                active: false,
                selected_stream: if selected_stream(settings) == "main" {
                    EventRecordingStream::Main
                } else {
                    EventRecordingStream::Sub
                },
                requested_ms: milliseconds(settings.pre),
                available_ms: 0,
                retained_bytes: 0,
                reason: if settings.pre.is_zero() {
                    PreRecordReason::Disabled
                } else {
                    PreRecordReason::Startup
                },
            },
        }
    }
}

fn frame_ends_before(frame: &RecordingFrame, boundary: Instant) -> bool {
    match &frame.frame {
        super::frame::MediaFrame::Audio(audio) => frame
            .received_at
            .checked_add(audio.duration)
            .is_some_and(|end| end <= boundary),
        super::frame::MediaFrame::Video(_) => frame.received_at < boundary,
    }
}

const fn history_reason(reason: HistoryReason) -> PreRecordReason {
    match reason {
        HistoryReason::Ready => PreRecordReason::Ready,
        HistoryReason::ReplayPending => PreRecordReason::PendingReplay,
        HistoryReason::Startup => PreRecordReason::Startup,
        HistoryReason::MissingKeyframe => PreRecordReason::MissingKeyframe,
        HistoryReason::DurationEviction => PreRecordReason::DurationEviction,
        HistoryReason::StreamPressure => PreRecordReason::PerStreamPressure,
        HistoryReason::GlobalPressure => PreRecordReason::GlobalPressure,
        HistoryReason::InvalidOrder => PreRecordReason::MalformedOrder,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::frame::{MediaFrame, VideoCodec, VideoFrame};
    use bytes::Bytes;

    fn video(at: Instant) -> RecordingFrame {
        RecordingFrame {
            received_at: at,
            timestamp: None,
            frame: MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H264,
                is_keyframe: true,
                width: 320,
                height: 240,
                data: Bytes::from_static(&[
                    0, 0, 0, 8, 0x67, 0x42, 0, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68,
                    0xce, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
                ]),
            }),
        }
    }

    fn configured(mode: CameraRecordingMode, pre: u64) -> EventRecordings {
        let mut runtime = EventRecordings::new(4096, 8192);
        assert!(runtime.configure(
            "camera",
            EventSettings {
                mode,
                stream: EventRecordingStream::Main,
                pre: Duration::from_secs(pre),
                post: Duration::from_secs(2)
            }
        ));
        runtime
    }

    #[test]
    fn oversized_decoder_parameters_cannot_remain_as_unbudgeted_history() {
        let mut runtime = configured(CameraRecordingMode::EventOnly, 2);
        let now = Instant::now();
        let mut frame = video(now);
        let MediaFrame::Video(video) = &mut frame.frame else {
            unreachable!()
        };
        let mut data = 65_536_u32.to_be_bytes().to_vec();
        data.extend_from_slice(&video.data[4..12]);
        data.resize(4 + 65_536, 0);
        data.extend_from_slice(&video.data[12..]);
        video.data = data.into();
        let queued = QueuedEventFrame::try_new(
            frame,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            1024 * 1024,
            1024,
        )
        .unwrap_or_else(|_| panic!("fixture fits ingress"));
        runtime.ingest(
            RecordingStreamIdentity::new("camera", "main", "camera"),
            queued,
            now,
        );
        assert_eq!(runtime.buffers.bytes(), 0);
        let epochs = &runtime.cameras["camera"].decoder;
        assert!(epochs[0].is_some());
        assert_eq!(
            std::mem::size_of_val(epochs),
            2 * std::mem::size_of::<Option<[u8; 32]>>(),
            "decoder epochs must retain only fixed-size fingerprints"
        );
    }

    #[test]
    fn decoder_epoch_preserves_identical_parameters_and_discards_changed_history() {
        let mut runtime = configured(CameraRecordingMode::EventOnly, 3);
        let now = Instant::now();
        input(&mut runtime, "main", now);
        let first = runtime.cameras["camera"].decoder[0];
        let bytes = runtime.buffers.bytes();
        input(&mut runtime, "main", now + Duration::from_secs(1));
        assert_eq!(runtime.cameras["camera"].decoder[0], first);
        assert_eq!(runtime.buffers.bytes(), 2 * bytes);
        let at = now + Duration::from_secs(2);
        let mut frame = video(at);
        let MediaFrame::Video(video) = &mut frame.frame else {
            unreachable!()
        };
        let mut data = video.data.to_vec();
        data[19] ^= 1;
        video.data = data.into();
        let queued = QueuedEventFrame::try_new(
            frame,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            4096,
            1024,
        )
        .unwrap_or_else(|_| panic!("fixture fits ingress"));
        runtime.ingest(
            RecordingStreamIdentity::new("camera", "main", "camera"),
            queued,
            at,
        );
        assert_ne!(runtime.cameras["camera"].decoder[0], first);
        assert_eq!(runtime.buffers.bytes(), bytes);
    }

    fn input(runtime: &mut EventRecordings, stream: &str, at: Instant) {
        let queued = QueuedEventFrame::try_new(
            video(at),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            4096,
            1024,
        )
        .unwrap_or_else(|_| panic!("fixture fits"));
        runtime.ingest(
            RecordingStreamIdentity::new("camera", stream, "camera"),
            queued,
            at,
        );
    }

    fn outputs(runtime: &mut EventRecordings, now: Instant) -> Vec<(String, Instant)> {
        let mut frames = Vec::new();
        for _ in 0..1024 {
            match runtime.next_output(now) {
                Some(EventOutput::Frame { identity, frame }) => {
                    frames.push((identity.stream_id, frame.received_at));
                }
                Some(EventOutput::Finish { .. }) => {}
                None => return frames,
            }
        }
        panic!("output must be bounded");
    }

    #[test]
    fn event_only_idle_selects_nothing_and_repeated_events_extend_once() {
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventOnly, 2);
        input(&mut runtime, "main", start);
        input(&mut runtime, "main", start + Duration::from_secs(1));
        assert!(outputs(&mut runtime, start + Duration::from_secs(1)).is_empty());
        runtime.note_event("camera", start + Duration::from_secs(1));
        runtime.note_event("camera", start + Duration::from_secs(2));
        input(&mut runtime, "main", start + Duration::from_secs(3));
        input(&mut runtime, "main", start + Duration::from_secs(4));
        let frames = outputs(&mut runtime, start + Duration::from_secs(4));
        assert_eq!(
            frames.iter().map(|(_, at)| *at).collect::<Vec<_>>(),
            vec![
                start,
                start + Duration::from_secs(1),
                start + Duration::from_secs(3)
            ]
        );
        assert!(
            !runtime
                .status("camera", start + Duration::from_secs(4))
                .unwrap()
                .active
        );
    }

    #[test]
    fn event_only_zero_pre_waits_for_live_keyframe_and_closes_silent_source() {
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventOnly, 0);
        input(&mut runtime, "main", start);
        runtime.note_event("camera", start);
        assert!(runtime.next_output(start).is_none());
        input(&mut runtime, "main", start + Duration::from_millis(10));
        assert!(matches!(
            runtime.next_output(start + Duration::from_secs(2)),
            Some(EventOutput::Frame { .. })
        ));
        assert!(
            matches!(runtime.next_output(start + Duration::from_secs(2)), Some(EventOutput::Finish { end, .. }) if end == start + Duration::from_secs(2))
        );
    }

    #[test]
    fn boost_replays_main_under_continuous_sub_identity_without_overlap() {
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventBoost, 2);
        input(&mut runtime, "sub", start);
        input(&mut runtime, "main", start + Duration::from_millis(100));
        input(&mut runtime, "sub", start + Duration::from_secs(1));
        input(&mut runtime, "main", start + Duration::from_millis(1100));
        runtime.note_event("camera", start + Duration::from_millis(1200));
        let frames = outputs(&mut runtime, start + Duration::from_millis(1200));
        assert_eq!(
            frames,
            vec![
                ("sub".into(), start),
                ("sub".into(), start + Duration::from_millis(100)),
                ("sub".into(), start + Duration::from_millis(1100))
            ]
        );
    }

    #[test]
    fn continuous_sub_survives_pressure_and_shutdown() {
        let start = Instant::now();
        let bytes = video(start).byte_len();
        let mut runtime = EventRecordings::new(bytes * 2, bytes * 4);
        assert!(runtime.configure(
            "camera",
            EventSettings {
                mode: CameraRecordingMode::EventBoost,
                stream: EventRecordingStream::Main,
                pre: Duration::from_secs(2),
                post: Duration::from_secs(2)
            }
        ));
        let mut actual = Vec::new();
        for offset in 0..10 {
            let at = start + Duration::from_millis(offset * 100);
            input(&mut runtime, "sub", at);
            actual.extend(outputs(&mut runtime, at));
            assert!(runtime.buffers.bytes() <= bytes * 4);
        }
        runtime.begin_shutdown(start + Duration::from_secs(1));
        actual.extend(outputs(&mut runtime, start + Duration::from_secs(1)));
        assert_eq!(
            actual.iter().map(|(_, at)| *at).collect::<Vec<_>>(),
            (0..10)
                .map(|offset| start + Duration::from_millis(offset * 100))
                .collect::<Vec<_>>()
        );
        assert_eq!(runtime.buffers.bytes(), 0);
    }

    #[test]
    fn events_without_media_cannot_accumulate_finish_markers() {
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventOnly, 0);
        for offset in 0..10_000 {
            let now = start + Duration::from_secs(offset * 3);
            runtime.note_event("camera", now);
            assert!(runtime.next_output(now).is_none());
        }
        assert!(runtime.cameras["camera"].pending.is_empty());
        assert_eq!(runtime.buffers.bytes(), 0);
        let end = start + Duration::from_secs(30_000);
        assert!(runtime.next_output(end).is_none());
        assert!(!runtime.status("camera", end).unwrap().active);
    }

    #[test]
    fn live_audio_packets_fit_video_coverage_and_exclusive_deadline() {
        use crate::storage::frame::{AudioCodec, AudioFrame};
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventOnly, 0);
        runtime.note_event("camera", start);
        input(&mut runtime, "main", start);
        for offset in [10, 1990] {
            let at = start + Duration::from_millis(offset);
            let frame = RecordingFrame {
                received_at: at,
                timestamp: None,
                frame: MediaFrame::Audio(AudioFrame {
                    codec: AudioCodec::Aac,
                    sample_rate: 16000,
                    duration: Duration::from_millis(20),
                    data: Bytes::from_static(&[0xaa]),
                }),
            };
            let queued = QueuedEventFrame::try_new(
                frame,
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
                4096,
                1024,
            )
            .unwrap_or_else(|_| panic!("fixture fits"));
            runtime.ingest(
                RecordingStreamIdentity::new("camera", "main", "camera"),
                queued,
                at,
            );
            if offset == 10 {
                input(&mut runtime, "main", start + Duration::from_millis(40));
            }
        }
        let frames = outputs(&mut runtime, start + Duration::from_secs(2));
        assert_eq!(
            frames.iter().map(|(_, at)| *at).collect::<Vec<_>>(),
            vec![
                start,
                start + Duration::from_millis(10),
                start + Duration::from_millis(40)
            ]
        );
    }

    #[test]
    fn oversized_sub_frame_is_forwarded_after_earlier_history() {
        let start = Instant::now();
        let bytes = video(start).byte_len();
        let mut runtime = EventRecordings::new(bytes, bytes);
        assert!(runtime.configure(
            "camera",
            EventSettings {
                mode: CameraRecordingMode::EventBoost,
                stream: EventRecordingStream::Main,
                pre: Duration::from_secs(2),
                post: Duration::from_secs(2)
            }
        ));
        input(&mut runtime, "sub", start);
        let at = start + Duration::from_millis(100);
        let mut large = video(at);
        if let MediaFrame::Video(video) = &mut large.frame {
            let mut payload = video.data.to_vec();
            payload.extend_from_slice(&[0, 0, 0, 1, 0x65]);
            video.data = payload.into();
        }
        let input = QueuedEventFrame::try_new(
            large,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            4096,
            1024,
        )
        .unwrap_or_else(|_| panic!("fixture fits"));
        runtime.ingest(
            RecordingStreamIdentity::new("camera", "sub", "camera"),
            input,
            at,
        );
        assert_eq!(
            outputs(&mut runtime, at),
            vec![("sub".into(), start), ("sub".into(), at)]
        );
    }

    #[test]
    fn reset_drops_replay_and_releases_all_budget() {
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventOnly, 2);
        input(&mut runtime, "main", start);
        runtime.note_event("camera", start);
        assert!(runtime.buffers.bytes() > 0);
        runtime.fail("camera", PreRecordReason::Privacy);
        assert_eq!(runtime.buffers.bytes(), 0);
        assert!(runtime.next_output(start).is_none());
        assert_eq!(
            runtime.status("camera", start).unwrap().reason,
            PreRecordReason::Privacy
        );
    }

    #[test]
    fn later_window_cannot_validate_audio_outside_an_earlier_video_interval() {
        use crate::storage::frame::{AudioCodec, AudioFrame};
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventOnly, 0);
        runtime.note_event("camera", start);
        input(&mut runtime, "main", start);
        let at = start + Duration::from_millis(100);
        let frame = RecordingFrame {
            received_at: at,
            timestamp: None,
            frame: MediaFrame::Audio(AudioFrame {
                codec: AudioCodec::Aac,
                sample_rate: 16000,
                duration: Duration::from_millis(20),
                data: Bytes::from_static(&[0xaa]),
            }),
        };
        let queued = QueuedEventFrame::try_new(
            frame,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            4096,
            1024,
        )
        .unwrap_or_else(|_| panic!("fixture fits"));
        runtime.ingest(
            RecordingStreamIdentity::new("camera", "main", "camera"),
            queued,
            at,
        );
        let next = start + Duration::from_secs(3);
        runtime.note_event("camera", next);
        input(&mut runtime, "main", next);
        assert_eq!(
            outputs(&mut runtime, next),
            vec![("main".into(), start), ("main".into(), next)]
        );
    }

    #[test]
    fn shutdown_cannot_extend_an_expired_event_deadline() {
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventOnly, 0);
        runtime.note_event("camera", start);
        input(&mut runtime, "main", start);
        let shutdown = start + Duration::from_secs(10);
        runtime.begin_shutdown(shutdown);
        assert!(matches!(
            runtime.next_output(shutdown),
            Some(EventOutput::Frame { .. })
        ));
        assert!(
            matches!(runtime.next_output(shutdown), Some(EventOutput::Finish { end, .. }) if end == start + Duration::from_secs(2))
        );
    }

    #[test]
    fn boost_without_a_main_keyframe_stops_reporting_an_expired_event() {
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventBoost, 2);
        input(&mut runtime, "sub", start);
        runtime.note_event("camera", start);
        let end = start + Duration::from_secs(3);
        assert_eq!(outputs(&mut runtime, end), vec![("sub".into(), start)]);
        assert!(!runtime.status("camera", end).unwrap().active);
    }

    #[test]
    fn malformed_optional_main_preserves_continuous_sub_history() {
        let start = Instant::now();
        let mut runtime = configured(CameraRecordingMode::EventBoost, 2);
        input(&mut runtime, "sub", start);
        input(&mut runtime, "main", start + Duration::from_secs(1));
        input(&mut runtime, "main", start);
        let end = start + Duration::from_secs(3);
        assert_eq!(outputs(&mut runtime, end), vec![("sub".into(), start)]);
        assert_eq!(
            runtime.status("camera", end).unwrap().reason,
            PreRecordReason::MalformedOrder
        );
    }
}
