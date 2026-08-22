//! Provides thread-based WebRTC delivery for encoded camera frames.

use crate::{
    keeppeek::StreamKind,
    media_time::duration_to_ticks,
    storage::{RecordingDemand, RecordingDemandGuard, VideoCodec, nal},
};
use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use hap_video::{
    IceCandidate as HapIceCandidate, OfferDescription as HapOfferDescription,
    SessionId as HapSessionId, Str0mSession, VideoCodec as HapVideoCodec,
};
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use polling::{Event as PollEvent, Events, Poller};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use str0m::{
    Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig,
    bwe::{Bitrate, BweKind},
    change::{SdpAnswer, SdpOffer},
    format::Codec,
    media::{MediaKind, MediaTime, Mid},
    net::{Protocol, Receive},
};

const FRAME_QUEUE_CAPACITY: usize = 1_000;
const UDP_EVENT_KEY: usize = 1;
const UDP_PACKET_CAPACITY: usize = 2_048;
const DEFAULT_FRAME_TICKS: u64 = 3_000;
const INITIAL_EGRESS_BITRATE: Bitrate = Bitrate::mbps(2);
const DEFAULT_MAIN_BITRATE: Bitrate = Bitrate::mbps(8);
const DEFAULT_SUB_BITRATE: Bitrate = Bitrate::mbps(1);
const MAX_DESIRED_BITRATE: Bitrate = Bitrate::mbps(20);
const SOURCE_BITRATE_WINDOW: Duration = Duration::from_secs(1);
const TARGET_FRAME_DELIVERY: Duration = Duration::from_millis(100);
const DESIRED_BITRATE_REFRESH: Duration = Duration::from_secs(1);
const UPGRADE_HOLD: Duration = Duration::from_secs(3);
const DOWNGRADE_HOLD: Duration = Duration::from_secs(1);
/// Bounds an HTTP close request while the poller wakes and the session thread releases resources.
const SESSION_CLOSE_WAIT: Duration = Duration::from_secs(1);
/// Limits one browser session so a malformed offer cannot monopolize the session thread.
const MAX_TRACKS: usize = 32;
/// Limits client-generated track identifiers kept in server session state.
const MAX_TRACK_ID_BYTES: usize = 64;
/// Matches str0m's fixed-width SDP media identifier capacity.
const MAX_MID_BYTES: usize = 16;

fn redacted_sdp(sdp: &str) -> String {
    let mut redacted = String::with_capacity(sdp.len());
    for line in sdp.lines() {
        if line.starts_with("a=ice-pwd:") {
            redacted.push_str("a=ice-pwd:<redacted>");
        } else {
            redacted.push_str(line.trim_end_matches('\r'));
        }
        redacted.push('\n');
    }
    redacted
}

fn webrtc_datagram_kind(packet: &[u8]) -> &'static str {
    if packet.len() >= 8 && packet[0] & 0b1100_0000 == 0 && packet[4..8] == [0x21, 0x12, 0xa4, 0x42]
    {
        "stun"
    } else if packet.first().is_some_and(|byte| (20..=63).contains(byte)) {
        "dtls"
    } else if packet
        .first()
        .is_some_and(|byte| byte & 0b1100_0000 == 0b1000_0000)
    {
        "rtp-or-rtcp"
    } else {
        "unknown"
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StreamQuality {
    #[default]
    Auto,
    High,
    Low,
}

impl StreamQuality {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::High => 1,
            Self::Low => 2,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::High,
            2 => Self::Low,
            _ => Self::Auto,
        }
    }
}

impl std::fmt::Display for StreamQuality {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => formatter.write_str("auto"),
            Self::High => formatter.write_str("high"),
            Self::Low => formatter.write_str("low"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub(crate) struct SessionId(u64);

impl SessionId {
    pub(crate) const fn from_u64(value: u64) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub(crate) struct TrackId(String);

impl TrackId {
    pub(crate) fn parse(value: String) -> anyhow::Result<Self> {
        if value.is_empty()
            || value.len() > MAX_TRACK_ID_BYTES
            || !value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            anyhow::bail!("live track ID must contain 1 to 64 visible ASCII characters");
        }
        Ok(Self(value))
    }
}

impl std::fmt::Display for TrackId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub(crate) struct Session {
    pub(crate) id: SessionId,
    pub(crate) answer: SdpAnswer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct SessionStatus {
    pub(crate) requested_quality: StreamQuality,
    pub(crate) active_stream: StreamKind,
    pub(crate) estimated_bitrate_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TrackStatus {
    pub(crate) track_id: TrackId,
    pub(crate) requested_quality: StreamQuality,
    pub(crate) active_stream: StreamKind,
    pub(crate) estimated_bitrate_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MultiTrackSessionStatus {
    pub(crate) estimated_bitrate_bps: Option<u64>,
    pub(crate) tracks: Vec<TrackStatus>,
}

#[derive(Debug, Clone)]
pub(crate) struct TrackPlan {
    pub(crate) track_id: TrackId,
    pub(crate) mid: String,
    pub(crate) camera_ip: IpAddr,
    pub(crate) has_sub_stream: bool,
    pub(crate) recording_label: String,
    pub(crate) quality: StreamQuality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Source {
    pub camera_ip: IpAddr,
    pub stream: StreamKind,
}

#[derive(Debug, Clone)]
struct MediaFrame {
    codec: VideoCodec,
    is_keyframe: bool,
    received_at: Instant,
    timestamp: Option<Duration>,
    data: Arc<MediaFrameData>,
}

#[derive(Debug)]
struct MediaFrameData {
    avcc: Bytes,
    annexb: OnceLock<Arc<[u8]>>,
    h264_profile_level_id: OnceLock<Option<u32>>,
}

impl MediaFrameData {
    const fn new(avcc: Bytes) -> Self {
        Self {
            avcc,
            annexb: OnceLock::new(),
            h264_profile_level_id: OnceLock::new(),
        }
    }

    fn annexb(&self) -> Arc<[u8]> {
        self.annexb
            .get_or_init(|| Arc::from(nal::avcc_to_annexb(&self.avcc)))
            .clone()
    }

    fn h264_profile_level_id(&self) -> Option<u32> {
        *self.h264_profile_level_id.get_or_init(|| {
            let (sps, _) = nal::extract_h264_sps_pps(&self.avcc);
            let sps = sps?;
            let [_, profile_idc, profile_iop, level_idc, ..] = sps.as_slice() else {
                return None;
            };
            Some(u32::from_be_bytes([
                0,
                *profile_idc,
                *profile_iop,
                *level_idc,
            ]))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum H264Profile {
    ConstrainedBaseline,
    Baseline,
    Main,
    Extended,
    High,
    ConstrainedHigh,
    High10,
    High422,
    High444Predictive,
    High10Intra,
    High422Intra,
    High444Intra,
    Cavlc444Intra,
}

const fn h264_profile(profile_level_id: u32) -> Option<H264Profile> {
    let [_, profile_idc, profile_iop, _] = profile_level_id.to_be_bytes();
    match (profile_idc, profile_iop) {
        (0x42, profile_iop) if profile_iop & 0x4f == 0x40 => Some(H264Profile::ConstrainedBaseline),
        (0x4d, profile_iop) if profile_iop & 0x8f == 0x80 => Some(H264Profile::ConstrainedBaseline),
        (0x58, profile_iop) if profile_iop & 0xcf == 0xc0 => Some(H264Profile::ConstrainedBaseline),
        (0x42, profile_iop) if profile_iop & 0x4f == 0 => Some(H264Profile::Baseline),
        (0x58, profile_iop) if profile_iop & 0xcf == 0x80 => Some(H264Profile::Baseline),
        (0x4d, profile_iop) if profile_iop & 0xaf == 0 => Some(H264Profile::Main),
        (0x58, profile_iop) if profile_iop & 0xcf == 0 => Some(H264Profile::Extended),
        (0x64, 0) => Some(H264Profile::High),
        (0x64, 0x0c) => Some(H264Profile::ConstrainedHigh),
        (0x6e, 0) => Some(H264Profile::High10),
        (0x7a, 0) => Some(H264Profile::High422),
        (0xf4, 0) => Some(H264Profile::High444Predictive),
        (0x6e, 0x10) => Some(H264Profile::High10Intra),
        (0x7a, 0x10) => Some(H264Profile::High422Intra),
        (0xf4, 0x10) => Some(H264Profile::High444Intra),
        (0x2c, 0x10) => Some(H264Profile::Cavlc444Intra),
        _ => None,
    }
}

fn h264_profiles_match(source: u32, payload: u32) -> bool {
    h264_profile(source).is_some_and(|source_profile| h264_profile(payload) == Some(source_profile))
}

#[derive(Debug)]
enum SessionCommand {
    Frame {
        sequence: u64,
        source: Source,
        frame: MediaFrame,
    },
}

enum HomeKitCommand {
    ApplyAnswer {
        sdp: String,
        candidates: Vec<HapIceCandidate>,
        response: Sender<Result<(), String>>,
    },
    AcceptReoffer {
        sdp: String,
        response: Sender<Result<String, String>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeKitTransportState {
    AwaitingAnswer,
    Connecting,
    Connected,
    Closed,
}

impl HomeKitTransportState {
    const fn as_u8(self) -> u8 {
        match self {
            Self::AwaitingAnswer => 0,
            Self::Connecting => 1,
            Self::Connected => 2,
            Self::Closed => 3,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Connecting,
            2 => Self::Connected,
            3 => Self::Closed,
            _ => Self::AwaitingAnswer,
        }
    }
}

struct HomeKitSessionControl {
    session_id: HapSessionId,
    internal_id: SessionId,
    source: Source,
    commands: Sender<HomeKitCommand>,
    poller: Arc<Poller>,
    shutdown: Arc<AtomicBool>,
    state: AtomicU8,
    completion: SessionCompletion,
    udp_packets_sent: AtomicU64,
    udp_bytes_sent: AtomicU64,
    udp_packets_received: AtomicU64,
    udp_bytes_received: AtomicU64,
    video_frames_written: AtomicU64,
    video_bytes_written: AtomicU64,
}

impl HomeKitSessionControl {
    fn state(&self) -> HomeKitTransportState {
        HomeKitTransportState::from_u8(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, state: HomeKitTransportState) {
        let previous =
            HomeKitTransportState::from_u8(self.state.swap(state.as_u8(), Ordering::AcqRel));
        if previous != state {
            tracing::info!(
                session_id = ?self.session_id,
                internal_id = %self.internal_id,
                source = ?self.source,
                previous = ?previous,
                current = ?state,
                "HomeKit WebRTC transport state changed"
            );
        }
    }

    fn close(&self) {
        self.shutdown.store(true, Ordering::Release);
        if let Err(error) = self.poller.notify() {
            tracing::debug!(%error, "unable to wake HomeKit WebRTC session for shutdown");
        }
    }

    fn finish(&self) {
        self.completion.finish();
    }

    fn wait_for_finish(&self) -> bool {
        self.completion.wait_for_finish()
    }

    fn record_udp_sent(&self, bytes: usize) {
        self.udp_packets_sent.fetch_add(1, Ordering::Relaxed);
        self.udp_bytes_sent
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn record_udp_received(&self, bytes: usize) {
        self.udp_packets_received.fetch_add(1, Ordering::Relaxed);
        self.udp_bytes_received
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn record_video_frame(&self, frame: &MediaFrame, origin: FrameOrigin, sequence: Option<u64>) {
        let frame_number = self.video_frames_written.fetch_add(1, Ordering::Relaxed) + 1;
        self.video_bytes_written
            .fetch_add(frame.data.avcc.len() as u64, Ordering::Relaxed);
        tracing::trace!(
            session_id = ?self.session_id,
            internal_id = %self.internal_id,
            source = ?self.source,
            frame_number,
            ?sequence,
            ?origin,
            codec = ?frame.codec,
            keyframe = frame.is_keyframe,
            bytes = frame.data.avcc.len(),
            media_timestamp = ?frame.timestamp,
            queue_age = ?frame.received_at.elapsed(),
            "HomeKit WebRTC video frame written to str0m"
        );
        if frame_number == 1 {
            tracing::info!(
                session_id = ?self.session_id,
                internal_id = %self.internal_id,
                source = ?self.source,
                codec = ?frame.codec,
                keyframe = frame.is_keyframe,
                bytes = frame.data.avcc.len(),
                ?origin,
                "first HomeKit WebRTC video frame written"
            );
        } else if frame.is_keyframe {
            tracing::debug!(
                session_id = ?self.session_id,
                internal_id = %self.internal_id,
                frame_number,
                bytes = frame.data.avcc.len(),
                ?origin,
                "HomeKit WebRTC video keyframe written"
            );
        }
    }

    fn log_summary(&self) {
        tracing::info!(
            session_id = ?self.session_id,
            internal_id = %self.internal_id,
            source = ?self.source,
            state = ?self.state(),
            udp_packets_sent = self.udp_packets_sent.load(Ordering::Relaxed),
            udp_bytes_sent = self.udp_bytes_sent.load(Ordering::Relaxed),
            udp_packets_received = self.udp_packets_received.load(Ordering::Relaxed),
            udp_bytes_received = self.udp_bytes_received.load(Ordering::Relaxed),
            video_frames_written = self.video_frames_written.load(Ordering::Relaxed),
            video_bytes_written = self.video_bytes_written.load(Ordering::Relaxed),
            "HomeKit WebRTC session transport summary"
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameOrigin {
    Cached,
    Live,
}

#[derive(Debug)]
struct KeyframeGate {
    waiting: bool,
    cached_preview_written: bool,
}

impl KeyframeGate {
    const fn new() -> Self {
        Self {
            waiting: true,
            cached_preview_written: false,
        }
    }

    const fn allows(&self, origin: FrameOrigin, is_keyframe: bool) -> bool {
        match origin {
            FrameOrigin::Cached => self.waiting && !self.cached_preview_written,
            FrameOrigin::Live => !self.waiting || is_keyframe,
        }
    }

    const fn mark_written(&mut self, origin: FrameOrigin) {
        match origin {
            FrameOrigin::Cached => self.cached_preview_written = true,
            FrameOrigin::Live => self.waiting = false,
        }
    }

    const fn reset(&mut self) {
        self.waiting = true;
        self.cached_preview_written = false;
    }

    const fn wait_for_live_keyframe(&mut self) {
        self.waiting = true;
        self.cached_preview_written = true;
    }

    fn observe_sequence(&mut self, last_sequence: &mut Option<u64>, sequence: u64) -> bool {
        let contiguous = last_sequence.is_none_or(|last| sequence == last.wrapping_add(1));
        *last_sequence = Some(sequence);
        if !contiguous {
            self.wait_for_live_keyframe();
        }
        contiguous
    }
}

#[derive(Default)]
struct SessionQueueStats {
    high_water: AtomicUsize,
    written_frames: AtomicU64,
    full_drops: AtomicU64,
    discarded_frames: AtomicU64,
    recovery_drops: AtomicU64,
}

#[derive(Clone)]
struct SessionSender {
    id: SessionId,
    track_id: Option<TrackId>,
    tx: Sender<SessionCommand>,
    queue_stats: Arc<SessionQueueStats>,
    queue_high_water: Arc<AtomicUsize>,
    latest_keyframe: Arc<Mutex<Option<MediaFrame>>>,
    poller: Arc<Poller>,
    shutdown: Arc<AtomicBool>,
}

impl SessionSender {
    fn is_same_subscriber(&self, other: &Self) -> bool {
        self.id == other.id && self.track_id == other.track_id
    }
}

struct SessionControl {
    requested_quality: AtomicU8,
    active_stream: AtomicU8,
    estimated_bitrate_bps: AtomicU64,
    poller: Arc<Poller>,
}

impl SessionControl {
    const fn new(quality: StreamQuality, active_stream: StreamKind, poller: Arc<Poller>) -> Self {
        Self {
            requested_quality: AtomicU8::new(quality.as_u8()),
            active_stream: AtomicU8::new(stream_as_u8(active_stream)),
            estimated_bitrate_bps: AtomicU64::new(0),
            poller,
        }
    }

    fn status(&self) -> SessionStatus {
        let estimated_bitrate_bps = self.estimated_bitrate_bps.load(Ordering::Acquire);
        SessionStatus {
            requested_quality: StreamQuality::from_u8(
                self.requested_quality.load(Ordering::Acquire),
            ),
            active_stream: stream_from_u8(self.active_stream.load(Ordering::Acquire)),
            estimated_bitrate_bps: (estimated_bitrate_bps > 0).then_some(estimated_bitrate_bps),
        }
    }
}

struct MultiTrackControl {
    tracks: BTreeMap<TrackId, Arc<SessionControl>>,
    estimated_bitrate_bps: AtomicU64,
    poller: Arc<Poller>,
    shutdown: Arc<AtomicBool>,
    completion: SessionCompletion,
}

#[derive(Default)]
struct SessionCompletion {
    finished: Mutex<bool>,
    wake: Condvar,
}

impl SessionCompletion {
    fn finish(&self) {
        *self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.wake.notify_all();
    }

    fn wait_for_finish(&self) -> bool {
        let finished = self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *finished {
            return true;
        }
        let (finished, _) = self
            .wake
            .wait_timeout(finished, SESSION_CLOSE_WAIT)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *finished
    }
}

impl MultiTrackControl {
    fn status(&self) -> MultiTrackSessionStatus {
        let estimated_bitrate_bps = self.estimated_bitrate_bps.load(Ordering::Acquire);
        let tracks = self
            .tracks
            .iter()
            .map(|(track_id, control)| {
                let status = control.status();
                TrackStatus {
                    track_id: track_id.clone(),
                    requested_quality: status.requested_quality,
                    active_stream: status.active_stream,
                    estimated_bitrate_bps: status.estimated_bitrate_bps,
                }
            })
            .collect();
        MultiTrackSessionStatus {
            estimated_bitrate_bps: (estimated_bitrate_bps > 0).then_some(estimated_bitrate_bps),
            tracks,
        }
    }

    fn set_quality(&self, track_id: &TrackId, quality: StreamQuality) -> Option<TrackStatus> {
        let control = self.tracks.get(track_id)?;
        if quality != StreamQuality::Low {
            for (other_track_id, other_control) in &self.tracks {
                if other_track_id != track_id {
                    other_control
                        .requested_quality
                        .store(StreamQuality::Low.as_u8(), Ordering::Release);
                }
            }
        }
        control
            .requested_quality
            .store(quality.as_u8(), Ordering::Release);
        if let Err(error) = self.poller.notify() {
            tracing::debug!(%track_id, %error, "unable to wake shared WebRTC session for quality change");
        }
        let status = control.status();
        Some(TrackStatus {
            track_id: track_id.clone(),
            requested_quality: status.requested_quality,
            active_stream: status.active_stream,
            estimated_bitrate_bps: status.estimated_bitrate_bps,
        })
    }

    fn update_estimate(&self, bitrate: Bitrate) {
        self.estimated_bitrate_bps
            .store(bitrate.as_u64(), Ordering::Release);
    }

    fn close(&self) {
        self.shutdown.store(true, Ordering::Release);
        if let Err(error) = self.poller.notify() {
            tracing::debug!(%error, "unable to wake shared WebRTC session for shutdown");
        }
    }

    fn finish(&self) {
        self.completion.finish();
    }

    fn wait_for_finish(&self) -> bool {
        self.completion.wait_for_finish()
    }
}

const fn stream_as_u8(stream: StreamKind) -> u8 {
    match stream {
        StreamKind::Main => 0,
        StreamKind::Sub => 1,
    }
}

const fn stream_from_u8(value: u8) -> StreamKind {
    if value == 1 {
        StreamKind::Sub
    } else {
        StreamKind::Main
    }
}

impl SessionSender {
    fn try_send(
        &self,
        sequence: u64,
        source: Source,
        frame: MediaFrame,
    ) -> Result<(), TrySendError<SessionCommand>> {
        if frame.is_keyframe {
            *self
                .latest_keyframe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frame.clone());
        }
        let command = SessionCommand::Frame {
            sequence,
            source,
            frame,
        };
        let result = self.tx.try_send(command);
        match &result {
            Ok(()) => {
                let depth = self.tx.len();
                self.queue_stats
                    .high_water
                    .fetch_max(depth, Ordering::Relaxed);
                self.queue_high_water.fetch_max(depth, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                let full_drops = self.queue_stats.full_drops.fetch_add(1, Ordering::Relaxed) + 1;
                if full_drops == 1 || full_drops.is_power_of_two() {
                    tracing::warn!(
                        session_id = %self.id,
                        queue_depth = self.tx.len(),
                        queue_capacity = self.tx.capacity(),
                        full_drops,
                        "WebRTC session frame queue full"
                    );
                }
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
        if !matches!(result, Err(TrySendError::Disconnected(_)))
            && let Err(error) = self.poller.notify()
        {
            tracing::debug!(%error, "unable to wake WebRTC session");
        }
        result
    }
}

struct SourceState {
    subscribers: Vec<SessionSender>,
    keyframe: Option<MediaFrame>,
    next_sequence: u64,
    bitrate: SourceBitrate,
}

impl Default for SourceState {
    fn default() -> Self {
        Self {
            subscribers: Vec::new(),
            keyframe: None,
            next_sequence: 0,
            bitrate: SourceBitrate::new(),
        }
    }
}

struct SourceBitrate {
    window_started: Instant,
    window_bytes: u64,
    estimate_bps: Option<u64>,
    max_frame_bytes: usize,
}

impl SourceBitrate {
    fn new() -> Self {
        Self {
            window_started: Instant::now(),
            window_bytes: 0,
            estimate_bps: None,
            max_frame_bytes: 0,
        }
    }

    fn observe(&mut self, received_at: Instant, bytes: usize) {
        self.window_bytes = self.window_bytes.saturating_add(bytes as u64);
        self.max_frame_bytes = self.max_frame_bytes.max(bytes);
        let elapsed = received_at.saturating_duration_since(self.window_started);
        if elapsed < SOURCE_BITRATE_WINDOW {
            return;
        }
        let sample_bps = (self.window_bytes as f64 * 8.0 / elapsed.as_secs_f64()) as u64;
        self.estimate_bps = Some(self.estimate_bps.map_or(sample_bps, |estimate| {
            estimate.saturating_mul(3).saturating_add(sample_bps) / 4
        }));
        self.window_started = received_at;
        self.window_bytes = 0;
    }
}

#[derive(Default)]
struct Inner {
    sources: Mutex<HashMap<Source, SourceState>>,
    sessions: Mutex<HashMap<SessionId, Arc<SessionControl>>>,
    multi_track_sessions: Mutex<HashMap<SessionId, Arc<MultiTrackControl>>>,
    homekit_sessions: Mutex<HashMap<HapSessionId, Arc<HomeKitSessionControl>>>,
    threads: Mutex<Vec<SessionThread>>,
    next_session_id: AtomicU64,
    published_frames: AtomicU64,
    published_bytes: AtomicU64,
    delivered_frames: AtomicU64,
    written_frames: AtomicU64,
    queue_drops: AtomicU64,
    queue_high_water: Arc<AtomicUsize>,
    queue_discarded_frames: AtomicU64,
    queue_recovery_drops: AtomicU64,
}

struct SessionThread {
    session_id: SessionId,
    handle: JoinHandle<()>,
}

fn reap_finished_threads(inner: &Inner) {
    let finished = {
        let mut threads = inner
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut active = Vec::with_capacity(threads.len());
        let mut finished = Vec::new();
        for thread in std::mem::take(&mut *threads) {
            if thread.handle.is_finished() {
                finished.push(thread);
            } else {
                active.push(thread);
            }
        }
        *threads = active;
        finished
    };
    for thread in finished {
        if thread.handle.join().is_err() {
            tracing::warn!("WebRTC session thread panicked");
        }
    }
}

fn join_session_thread(inner: &Inner, session_id: SessionId) {
    let thread = {
        let mut threads = inner
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        threads
            .iter()
            .position(|thread| thread.session_id == session_id)
            .map(|index| threads.swap_remove(index))
    };
    if let Some(thread) = thread
        && thread.handle.join().is_err()
    {
        tracing::warn!(%session_id, "WebRTC session thread panicked");
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct WebRtcHealth {
    pub active_sessions: usize,
    pub adaptive_sessions: usize,
    pub multi_track_sessions: usize,
    pub multi_tracks: usize,
    pub fixed_sessions: usize,
    pub active_main: usize,
    pub active_sub: usize,
    pub requested_auto: usize,
    pub requested_high: usize,
    pub requested_low: usize,
    pub estimated_bitrate_min_bps: Option<u64>,
    pub estimated_bitrate_avg_bps: Option<u64>,
    pub estimated_bitrate_max_bps: Option<u64>,
    pub source_bitrate_bps: u64,
    pub published_frames: u64,
    pub published_bytes: u64,
    pub delivered_frames: u64,
    pub written_frames: u64,
    pub queue_capacity: usize,
    pub queued_frames: usize,
    pub queue_depth_max: usize,
    pub queue_high_water: usize,
    pub queue_drops: u64,
    pub queue_discarded_frames: u64,
    pub queue_recovery_drops: u64,
    pub session_queues: Vec<WebRtcSessionQueueHealth>,
    pub sources: Vec<WebRtcSourceHealth>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WebRtcSessionQueueHealth {
    pub session_id: SessionId,
    pub track_id: Option<TrackId>,
    pub camera_ip: IpAddr,
    pub stream: StreamKind,
    pub depth: usize,
    pub high_water: usize,
    pub written_frames: u64,
    pub full_drops: u64,
    pub discarded_frames: u64,
    pub recovery_drops: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct WebRtcSourceHealth {
    pub camera_ip: IpAddr,
    pub stream: StreamKind,
    pub subscribers: usize,
    pub bitrate_bps: Option<u64>,
    pub has_keyframe: bool,
    pub keyframe_age_ms: Option<u64>,
}

enum SessionPlan {
    Fixed {
        source: Source,
        demand_guard: Option<RecordingDemandGuard>,
    },
    Adaptive {
        camera_ip: IpAddr,
        has_sub_stream: bool,
        quality: StreamQuality,
        recording_label: String,
    },
}

struct SourceSubscription {
    inner: Arc<Inner>,
    sender: SessionSender,
    active_source: Source,
    pending_source: Option<Source>,
    high_source: Source,
    low_source: Option<Source>,
    control: Option<Arc<SessionControl>>,
    recording_demand: Option<RecordingDemand>,
    recording_label: Option<String>,
    demand_guard: Option<RecordingDemandGuard>,
    upgrade_since: Option<Instant>,
    downgrade_since: Option<Instant>,
}

impl SourceSubscription {
    fn record_written_frame(&self) {
        self.sender
            .queue_stats
            .written_frames
            .fetch_add(1, Ordering::Relaxed);
        self.inner.written_frames.fetch_add(1, Ordering::Relaxed);
    }

    fn record_discarded_frames(&self, count: u64) {
        if count == 0 {
            return;
        }
        self.sender
            .queue_stats
            .discarded_frames
            .fetch_add(count, Ordering::Relaxed);
        self.inner
            .queue_discarded_frames
            .fetch_add(count, Ordering::Relaxed);
    }

    fn discard_queued_frames(&self, rx: &Receiver<SessionCommand>) -> u64 {
        let mut discarded = 0u64;
        while rx.try_recv().is_ok() {
            discarded = discarded.saturating_add(1);
        }
        self.record_discarded_frames(discarded);
        discarded
    }

    fn fixed(
        inner: Arc<Inner>,
        sender: SessionSender,
        source: Source,
        demand_guard: Option<RecordingDemandGuard>,
    ) -> Self {
        let subscription = Self {
            inner,
            sender,
            active_source: source,
            pending_source: None,
            high_source: source,
            low_source: None,
            control: None,
            recording_demand: None,
            recording_label: None,
            demand_guard,
            upgrade_since: None,
            downgrade_since: None,
        };
        subscription.subscribe();
        subscription
    }

    fn adaptive(
        inner: Arc<Inner>,
        sender: SessionSender,
        high_source: Source,
        low_source: Option<Source>,
        control: Arc<SessionControl>,
        recording_demand: Option<RecordingDemand>,
        recording_label: String,
    ) -> Self {
        let active_source = match control.status().requested_quality {
            StreamQuality::High => high_source,
            StreamQuality::Auto | StreamQuality::Low => low_source.unwrap_or(high_source),
        };
        control
            .active_stream
            .store(stream_as_u8(active_source.stream), Ordering::Release);
        let demand_guard = recording_demand
            .as_ref()
            .map(|demand| demand.acquire(format!("{recording_label}/{}", active_source.stream)));
        let subscription = Self {
            inner,
            sender,
            active_source,
            pending_source: None,
            high_source,
            low_source,
            control: Some(control),
            recording_demand,
            recording_label: Some(recording_label),
            demand_guard,
            upgrade_since: None,
            downgrade_since: None,
        };
        subscription.subscribe();
        subscription
    }

    fn subscribe(&self) {
        let keyframe = {
            let mut sources = self
                .inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let state = sources.entry(self.active_source).or_default();
            state.subscribers.push(self.sender.clone());
            state.keyframe.clone()
        };
        *self
            .sender
            .latest_keyframe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = keyframe;
    }

    fn prepare_keyframe(&self, rx: &Receiver<SessionCommand>) {
        let keyframe = {
            let sources = self
                .inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.discard_queued_frames(rx);
            let Some(state) = sources.get(&self.active_source) else {
                return;
            };
            state.keyframe.clone()
        };
        *self
            .sender
            .latest_keyframe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = keyframe;
    }

    fn requested_quality(&self) -> Option<StreamQuality> {
        self.control.as_ref().map(|control| {
            StreamQuality::from_u8(control.requested_quality.load(Ordering::Acquire))
        })
    }

    fn desired_bitrate(&self, quality: StreamQuality) -> Bitrate {
        let source = match quality {
            StreamQuality::Auto | StreamQuality::High => self.high_source,
            StreamQuality::Low => self.low_source.unwrap_or(self.high_source),
        };
        let (average_bps, max_frame_bytes) = self.source_delivery_requirements(source);
        desired_egress_bitrate(average_bps, max_frame_bytes)
    }

    fn source_delivery_requirements(&self, source: Source) -> (u64, usize) {
        let sources = self
            .inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = sources.get(&source);
        let average_bps = state
            .and_then(|state| state.bitrate.estimate_bps)
            .unwrap_or_else(|| match source.stream {
                StreamKind::Main => DEFAULT_MAIN_BITRATE.as_u64(),
                StreamKind::Sub => DEFAULT_SUB_BITRATE.as_u64(),
            });
        let max_frame_bytes = state.map_or(0, |state| state.bitrate.max_frame_bytes);
        (average_bps, max_frame_bytes)
    }

    fn source_bitrate(&self, source: Source) -> Bitrate {
        self.inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&source)
            .and_then(|state| state.bitrate.estimate_bps)
            .map_or_else(
                || match source.stream {
                    StreamKind::Main => DEFAULT_MAIN_BITRATE,
                    StreamKind::Sub => DEFAULT_SUB_BITRATE,
                },
                Bitrate::bps,
            )
    }

    fn low_delivery_bitrate(&self) -> Bitrate {
        self.source_bitrate(self.low_source.unwrap_or(self.high_source))
    }

    fn source_codec(&self, source: Source) -> Option<VideoCodec> {
        self.inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&source)
            .and_then(|state| state.keyframe.as_ref())
            .map(|frame| frame.codec)
    }

    fn update_estimate(&self, bitrate: Bitrate) {
        if let Some(control) = &self.control {
            control
                .estimated_bitrate_bps
                .store(bitrate.as_u64(), Ordering::Release);
        }
    }

    fn select_source(&mut self, rtc: &mut Rtc, video_mid: Option<Mid>, now: Instant) {
        let Some(control) = &self.control else {
            return;
        };
        let requested = StreamQuality::from_u8(control.requested_quality.load(Ordering::Acquire));
        let target = match requested {
            StreamQuality::Low => self.low_source.unwrap_or(self.high_source),
            StreamQuality::High => self.high_source,
            StreamQuality::Auto => self.automatic_source(now),
        };
        if target == self.active_source || !self.codec_is_negotiated(rtc, video_mid, target) {
            self.cancel_pending_switch();
            return;
        }
        self.begin_switch(target);
    }

    fn automatic_source(&mut self, now: Instant) -> Source {
        let Some(low_source) = self.low_source else {
            return self.high_source;
        };
        let estimate = self
            .control
            .as_ref()
            .map(|control| control.estimated_bitrate_bps.load(Ordering::Acquire))
            .unwrap_or(0);
        if estimate == 0 {
            return low_source;
        }
        let required = self.source_bitrate(self.high_source).as_u64();
        if self.active_source == self.high_source {
            self.upgrade_since = None;
            if estimate.saturating_mul(100) < required.saturating_mul(105) {
                let since = self.downgrade_since.get_or_insert(now);
                if now.saturating_duration_since(*since) >= DOWNGRADE_HOLD {
                    return low_source;
                }
            } else {
                self.downgrade_since = None;
            }
            return self.high_source;
        }

        self.downgrade_since = None;
        if estimate.saturating_mul(100) >= required.saturating_mul(125) {
            let since = self.upgrade_since.get_or_insert(now);
            if now.saturating_duration_since(*since) >= UPGRADE_HOLD {
                return self.high_source;
            }
        } else {
            self.upgrade_since = None;
        }
        low_source
    }

    fn codec_is_negotiated(&self, rtc: &mut Rtc, video_mid: Option<Mid>, source: Source) -> bool {
        let Some(mid) = video_mid else {
            return false;
        };
        let Some(codec) = self.source_codec(source) else {
            return false;
        };
        let codec = match codec {
            VideoCodec::H264 => Codec::H264,
            VideoCodec::H265 => Codec::H265,
        };
        rtc.writer(mid).is_some_and(|writer| {
            writer
                .payload_params()
                .any(|params| params.spec().codec == codec)
        })
    }

    fn begin_switch(&mut self, target: Source) {
        if self.pending_source == Some(target) {
            return;
        }
        self.cancel_pending_switch();
        {
            let mut sources = self
                .inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let state = sources.entry(target).or_default();
            if !state
                .subscribers
                .iter()
                .any(|subscriber| subscriber.is_same_subscriber(&self.sender))
            {
                state.subscribers.push(self.sender.clone());
            }
        }
        self.pending_source = Some(target);
        tracing::debug!(
            session_id = %self.sender.id,
            stream = %target.stream,
            "WebRTC source switch armed"
        );
    }

    fn finish_switch(&mut self, target: Source) -> bool {
        if self.pending_source != Some(target) {
            return false;
        }
        let mut sources = self
            .inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = sources.get_mut(&self.active_source) {
            state
                .subscribers
                .retain(|subscriber| !subscriber.is_same_subscriber(&self.sender));
        }
        drop(sources);
        self.active_source = target;
        self.pending_source = None;
        self.upgrade_since = None;
        self.downgrade_since = None;
        if let Some(control) = &self.control {
            control
                .active_stream
                .store(stream_as_u8(target.stream), Ordering::Release);
        }
        self.demand_guard = self.recording_demand.as_ref().and_then(|demand| {
            self.recording_label
                .as_ref()
                .map(|label| demand.acquire(format!("{label}/{}", target.stream)))
        });
        tracing::debug!(
            session_id = %self.sender.id,
            stream = %target.stream,
            "WebRTC source switched"
        );
        true
    }

    fn finish_switch_on_frame(&mut self, source: Source, is_keyframe: bool) -> bool {
        is_keyframe && self.finish_switch(source)
    }

    fn cancel_pending_switch(&mut self) {
        let Some(pending) = self.pending_source.take() else {
            return;
        };
        let mut sources = self
            .inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = sources.get_mut(&pending) {
            state
                .subscribers
                .retain(|subscriber| !subscriber.is_same_subscriber(&self.sender));
        }
    }
}

fn desired_egress_bitrate(average_bps: u64, max_frame_bytes: usize) -> Bitrate {
    let average_with_headroom = average_bps.saturating_mul(5) / 4;
    let burst_bps = u64::try_from(max_frame_bytes)
        .unwrap_or(u64::MAX)
        .saturating_mul(8)
        .saturating_mul(1_000)
        / u64::try_from(TARGET_FRAME_DELIVERY.as_millis()).unwrap_or(1);
    Bitrate::bps(
        average_with_headroom
            .max(burst_bps)
            .min(MAX_DESIRED_BITRATE.as_u64()),
    )
}

impl Drop for SourceSubscription {
    fn drop(&mut self) {
        let mut sources = self
            .inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = sources.get_mut(&self.active_source) {
            state
                .subscribers
                .retain(|subscriber| !subscriber.is_same_subscriber(&self.sender));
        }
        if let Some(pending) = self.pending_source
            && let Some(state) = sources.get_mut(&pending)
        {
            state
                .subscribers
                .retain(|subscriber| !subscriber.is_same_subscriber(&self.sender));
        }
    }
}

struct TrackDeps {
    inner: Arc<Inner>,
    session_id: SessionId,
    control: Arc<SessionControl>,
    poller: Arc<Poller>,
    shutdown: Arc<AtomicBool>,
    recording_demand: Option<RecordingDemand>,
}

struct TrackRuntime {
    track_id: TrackId,
    mid: Mid,
    rx: Receiver<SessionCommand>,
    subscription: SourceSubscription,
    keyframe_gate: KeyframeGate,
    media_clock: MediaClock,
    last_frame_sequence: Option<u64>,
    recovering_queue_gap: bool,
    received_source_frame: bool,
    keyframe_prepared: bool,
}

impl TrackRuntime {
    fn new(plan: TrackPlan, mid: Mid, deps: TrackDeps) -> Self {
        let TrackDeps {
            inner,
            session_id,
            control,
            poller,
            shutdown,
            recording_demand,
        } = deps;
        let (tx, rx) = bounded(FRAME_QUEUE_CAPACITY);
        let sender = SessionSender {
            id: session_id,
            track_id: Some(plan.track_id.clone()),
            tx,
            queue_stats: Arc::new(SessionQueueStats::default()),
            queue_high_water: inner.queue_high_water.clone(),
            latest_keyframe: Arc::new(Mutex::new(None)),
            poller,
            shutdown,
        };
        let high_source = Source {
            camera_ip: plan.camera_ip,
            stream: StreamKind::Main,
        };
        let low_source = plan.has_sub_stream.then_some(Source {
            camera_ip: plan.camera_ip,
            stream: StreamKind::Sub,
        });
        let subscription = SourceSubscription::adaptive(
            inner,
            sender,
            high_source,
            low_source,
            control,
            recording_demand,
            plan.recording_label,
        );
        Self {
            track_id: plan.track_id,
            mid,
            rx,
            subscription,
            keyframe_gate: KeyframeGate::new(),
            media_clock: MediaClock::default(),
            last_frame_sequence: None,
            recovering_queue_gap: false,
            received_source_frame: false,
            keyframe_prepared: false,
        }
    }

    fn reset_source_state(&mut self) {
        self.media_clock.reset_source();
        self.last_frame_sequence = None;
        self.recovering_queue_gap = false;
        self.received_source_frame = false;
        self.keyframe_gate.reset();
    }
}

#[derive(Clone, Default)]
pub struct Publisher {
    inner: Arc<Inner>,
}

impl Publisher {
    pub fn publish(
        &self,
        source: Source,
        codec: VideoCodec,
        is_keyframe: bool,
        received_at: Instant,
        timestamp: Option<Duration>,
        avcc: Bytes,
    ) {
        self.inner.published_frames.fetch_add(1, Ordering::Relaxed);
        self.inner
            .published_bytes
            .fetch_add(avcc.len() as u64, Ordering::Relaxed);
        let mut sources = self
            .inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = sources.entry(source).or_default();
        state.bitrate.observe(received_at, avcc.len());
        if state.subscribers.is_empty() && !is_keyframe {
            return;
        }

        let frame = MediaFrame {
            codec,
            is_keyframe,
            received_at,
            timestamp,
            data: Arc::new(MediaFrameData::new(avcc)),
        };
        if is_keyframe {
            state.keyframe = Some(frame.clone());
        }
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);

        state.subscribers.retain(|subscriber| {
            match subscriber.try_send(sequence, source, frame.clone()) {
                Ok(()) => {
                    self.inner.delivered_frames.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(TrySendError::Full(_)) => {
                    self.inner.queue_drops.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }
}

#[derive(Clone, Default)]
pub struct WebRtc {
    live: Publisher,
    recording_demand: Option<RecordingDemand>,
}

impl WebRtc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn live(&self) -> Publisher {
        self.live.clone()
    }

    pub fn with_recording_demand(recording_demand: RecordingDemand) -> Self {
        Self {
            live: Publisher::default(),
            recording_demand: Some(recording_demand),
        }
    }

    pub fn accept_offer(&self, source: Source, offer: SdpOffer) -> anyhow::Result<SdpAnswer> {
        self.accept_offer_inner(
            SessionPlan::Fixed {
                source,
                demand_guard: None,
            },
            offer,
        )
        .map(|session| session.answer)
    }

    pub fn accept_offer_for_recording(
        &self,
        source: Source,
        recording_stream_id: &str,
        offer: SdpOffer,
    ) -> anyhow::Result<SdpAnswer> {
        let demand_guard = self
            .recording_demand
            .as_ref()
            .map(|demand| demand.acquire(recording_stream_id));
        self.accept_offer_inner(
            SessionPlan::Fixed {
                source,
                demand_guard,
            },
            offer,
        )
        .map(|session| session.answer)
    }

    pub(crate) fn accept_adaptive_offer(
        &self,
        camera_ip: IpAddr,
        has_sub_stream: bool,
        recording_label: &str,
        quality: StreamQuality,
        offer: SdpOffer,
    ) -> anyhow::Result<Session> {
        self.accept_offer_inner(
            SessionPlan::Adaptive {
                camera_ip,
                has_sub_stream,
                quality,
                recording_label: recording_label.to_owned(),
            },
            offer,
        )
    }

    pub(crate) fn accept_multi_track_offer(
        &self,
        plans: Vec<TrackPlan>,
        offer: SdpOffer,
    ) -> anyhow::Result<Session> {
        reap_finished_threads(&self.live.inner);
        validate_multi_tracks(&plans)?;
        let SessionIo {
            rtc,
            socket,
            poller,
            answer,
        } = accept_session(offer)?;
        let session_id = next_session_id(&self.live.inner);
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut controls = BTreeMap::new();
        for plan in &plans {
            let low_source = plan.has_sub_stream.then_some(Source {
                camera_ip: plan.camera_ip,
                stream: StreamKind::Sub,
            });
            let control = Arc::new(SessionControl::new(
                plan.quality,
                initial_stream(plan.quality, low_source),
                poller.clone(),
            ));
            let previous = controls.insert(plan.track_id.clone(), control);
            debug_assert!(
                previous.is_none(),
                "browser track IDs were validated as unique"
            );
        }
        let control = Arc::new(MultiTrackControl {
            tracks: controls,
            estimated_bitrate_bps: AtomicU64::new(0),
            poller: poller.clone(),
            shutdown: shutdown.clone(),
            completion: SessionCompletion::default(),
        });
        let mids = bind_mids(&plans);
        let mut tracks = Vec::with_capacity(plans.len());
        for plan in plans {
            let mid = *mids
                .get(&plan.track_id)
                .expect("validated browser track must have a negotiated MID");
            let track_control = control
                .tracks
                .get(&plan.track_id)
                .expect("validated browser track must have a control")
                .clone();
            tracks.push(TrackRuntime::new(
                plan,
                mid,
                TrackDeps {
                    inner: self.live.inner.clone(),
                    session_id,
                    control: track_control,
                    poller: poller.clone(),
                    shutdown: shutdown.clone(),
                    recording_demand: self.recording_demand.clone(),
                },
            ));
        }

        self.live
            .inner
            .multi_track_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id, control.clone());
        let thread_inner = self.live.inner.clone();
        let thread_control = control.clone();
        let thread = match std::thread::Builder::new()
            .name(format!("webrtc-browser-{session_id}"))
            .spawn(move || {
                if let Err(error) = run_multi_track_session(
                    rtc,
                    socket,
                    poller,
                    tracks,
                    thread_control.clone(),
                    shutdown,
                ) {
                    tracing::debug!(%error, "shared WebRTC session stopped with error");
                }
                thread_inner
                    .multi_track_sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&session_id);
                thread_control.finish();
            }) {
            Ok(thread) => thread,
            Err(error) => {
                control.finish();
                self.live
                    .inner
                    .multi_track_sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&session_id);
                return Err(error.into());
            }
        };
        self.live
            .inner
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(SessionThread {
                session_id,
                handle: thread,
            });

        Ok(Session {
            id: session_id,
            answer,
        })
    }

    pub(crate) fn multi_track_session_status(
        &self,
        session_id: SessionId,
    ) -> Option<MultiTrackSessionStatus> {
        reap_finished_threads(&self.live.inner);
        self.live
            .inner
            .multi_track_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|control| control.status())
    }

    pub(crate) fn set_multi_track_quality(
        &self,
        session_id: SessionId,
        track_id: &TrackId,
        quality: StreamQuality,
    ) -> Option<TrackStatus> {
        reap_finished_threads(&self.live.inner);
        self.live
            .inner
            .multi_track_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .and_then(|control| control.set_quality(track_id, quality))
    }

    pub(crate) fn close_multi_track_session(&self, session_id: SessionId) -> bool {
        reap_finished_threads(&self.live.inner);
        let control = self
            .live
            .inner
            .multi_track_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
        let Some(control) = control else {
            return false;
        };
        control.close();
        if !control.wait_for_finish() {
            tracing::warn!(%session_id, "shared WebRTC session did not finish before close timeout");
        } else {
            join_session_thread(&self.live.inner, session_id);
        }
        reap_finished_threads(&self.live.inner);
        true
    }

    pub(crate) fn set_quality(
        &self,
        session_id: SessionId,
        quality: StreamQuality,
    ) -> Option<SessionStatus> {
        reap_finished_threads(&self.live.inner);
        let control = self
            .live
            .inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .cloned()?;
        control
            .requested_quality
            .store(quality.as_u8(), Ordering::Release);
        if let Err(error) = control.poller.notify() {
            tracing::debug!(%session_id, %error, "unable to wake WebRTC session for quality change");
        }
        Some(control.status())
    }

    pub(crate) fn session_status(&self, session_id: SessionId) -> Option<SessionStatus> {
        reap_finished_threads(&self.live.inner);
        self.live
            .inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|control| control.status())
    }

    pub(crate) fn create_homekit_offer(
        &self,
        session_id: HapSessionId,
        source: Source,
    ) -> anyhow::Result<HapOfferDescription> {
        reap_finished_threads(&self.live.inner);
        if self
            .live
            .inner
            .homekit_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&session_id)
        {
            anyhow::bail!("duplicate HomeKit WebRTC session");
        }

        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        socket.set_nonblocking(true)?;
        let local_address = socket.local_addr()?;
        let candidates = host_candidates(local_address.port())?;
        tracing::debug!(
            session_id = ?session_id,
            ?source,
            %local_address,
            candidates = ?candidates,
            "HomeKit WebRTC UDP socket and host candidates prepared"
        );
        let rtc = rtc_config().build(Instant::now());
        let track_id = homekit_track_id(session_id);
        let (session, offer) = Str0mSession::create_video_offer(
            rtc,
            candidates,
            Some(track_id.clone()),
            Some(track_id),
        )?;
        tracing::debug!(
            session_id = ?session_id,
            ?source,
            sdp = %redacted_sdp(&offer.sdp),
            candidates = ?offer.candidates,
            "HomeKit WebRTC SDP offer"
        );
        let poller = Arc::new(Poller::new()?);
        // SAFETY: The session thread owns the socket and removes it before either resource drops.
        unsafe {
            poller.add(&socket, PollEvent::readable(UDP_EVENT_KEY))?;
        }

        let internal_id = next_session_id(&self.live.inner);
        let shutdown = Arc::new(AtomicBool::new(false));
        let (frame_tx, frame_rx) = bounded(FRAME_QUEUE_CAPACITY);
        let (command_tx, command_rx) = bounded(8);
        let sender = SessionSender {
            id: internal_id,
            track_id: None,
            tx: frame_tx,
            queue_stats: Arc::new(SessionQueueStats::default()),
            queue_high_water: self.live.inner.queue_high_water.clone(),
            latest_keyframe: Arc::new(Mutex::new(None)),
            poller: poller.clone(),
            shutdown: shutdown.clone(),
        };
        let subscription = SourceSubscription::fixed(self.live.inner.clone(), sender, source, None);
        let control = Arc::new(HomeKitSessionControl {
            session_id,
            internal_id,
            source,
            commands: command_tx,
            poller: poller.clone(),
            shutdown: shutdown.clone(),
            state: AtomicU8::new(HomeKitTransportState::AwaitingAnswer.as_u8()),
            completion: SessionCompletion::default(),
            udp_packets_sent: AtomicU64::new(0),
            udp_bytes_sent: AtomicU64::new(0),
            udp_packets_received: AtomicU64::new(0),
            udp_bytes_received: AtomicU64::new(0),
            video_frames_written: AtomicU64::new(0),
            video_bytes_written: AtomicU64::new(0),
        });
        let active_sessions = {
            let mut sessions = self
                .live
                .inner
                .homekit_sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sessions.insert(session_id, control.clone());
            sessions.len()
        };
        tracing::info!(
            session_id = ?session_id,
            internal_id = %internal_id,
            ?source,
            %local_address,
            active_sessions,
            "HomeKit WebRTC transport allocated"
        );

        let inner = self.live.inner.clone();
        let thread_inner = inner.clone();
        let thread_control = control;
        let thread = match std::thread::Builder::new()
            .name(format!("webrtc-homekit-{internal_id}"))
            .spawn(move || {
                tracing::info!(
                    session_id = ?thread_control.session_id,
                    internal_id = %thread_control.internal_id,
                    source = ?thread_control.source,
                    "HomeKit WebRTC session thread started"
                );
                if let Err(error) = run_homekit_session(HomeKitSessionRuntime {
                    session,
                    socket,
                    poller,
                    frame_rx,
                    command_rx,
                    subscription,
                    shutdown,
                    control: thread_control.clone(),
                }) {
                    tracing::warn!(
                        session_id = ?thread_control.session_id,
                        internal_id = %thread_control.internal_id,
                        source = ?thread_control.source,
                        %error,
                        "HomeKit WebRTC session stopped with error"
                    );
                }
                thread_control.log_summary();
                thread_control.set_state(HomeKitTransportState::Closed);
                let remaining_sessions = {
                    let mut sessions = thread_inner
                        .homekit_sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    sessions.remove(&session_id);
                    sessions.len()
                };
                tracing::info!(
                    session_id = ?thread_control.session_id,
                    internal_id = %thread_control.internal_id,
                    source = ?thread_control.source,
                    remaining_sessions,
                    "HomeKit WebRTC session thread stopped"
                );
                thread_control.finish();
            }) {
            Ok(thread) => thread,
            Err(error) => {
                inner
                    .homekit_sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&session_id);
                return Err(error.into());
            }
        };
        self.live
            .inner
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(SessionThread {
                session_id: internal_id,
                handle: thread,
            });
        Ok(offer)
    }

    pub(crate) fn apply_homekit_answer(
        &self,
        session_id: HapSessionId,
        sdp: String,
        candidates: Vec<HapIceCandidate>,
    ) -> anyhow::Result<()> {
        let control = self.homekit_control(session_id)?;
        tracing::info!(
            session_id = ?session_id,
            internal_id = %control.internal_id,
            source = ?control.source,
            sdp_bytes = sdp.len(),
            candidate_count = candidates.len(),
            "queueing HomeKit controller SDP answer"
        );
        tracing::debug!(
            session_id = ?session_id,
            internal_id = %control.internal_id,
            sdp = %redacted_sdp(&sdp),
            candidates = ?candidates,
            "HomeKit controller SDP answer"
        );
        let (response, result) = bounded(1);
        control
            .commands
            .send(HomeKitCommand::ApplyAnswer {
                sdp,
                candidates,
                response,
            })
            .map_err(|_| anyhow::anyhow!("HomeKit WebRTC session ended"))?;
        control.poller.notify()?;
        let result = result
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow::anyhow!("timed out applying HomeKit WebRTC answer"))?;
        match &result {
            Ok(()) => tracing::info!(
                session_id = ?session_id,
                internal_id = %control.internal_id,
                "HomeKit controller SDP answer accepted by str0m"
            ),
            Err(error) => tracing::warn!(
                session_id = ?session_id,
                internal_id = %control.internal_id,
                %error,
                "HomeKit controller SDP answer rejected by str0m"
            ),
        }
        result.map_err(anyhow::Error::msg)
    }

    pub(crate) fn accept_homekit_reoffer(
        &self,
        session_id: HapSessionId,
        sdp: String,
    ) -> anyhow::Result<String> {
        let control = self.homekit_control(session_id)?;
        tracing::info!(
            session_id = ?session_id,
            internal_id = %control.internal_id,
            source = ?control.source,
            sdp_bytes = sdp.len(),
            "queueing HomeKit controller SDP reoffer"
        );
        tracing::debug!(
            session_id = ?session_id,
            internal_id = %control.internal_id,
            sdp = %redacted_sdp(&sdp),
            "HomeKit controller SDP reoffer"
        );
        let (response, result) = bounded(1);
        control
            .commands
            .send(HomeKitCommand::AcceptReoffer { sdp, response })
            .map_err(|_| anyhow::anyhow!("HomeKit WebRTC session ended"))?;
        control.poller.notify()?;
        let result = result
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow::anyhow!("timed out accepting HomeKit WebRTC reoffer"))?;
        match &result {
            Ok(answer) => {
                tracing::info!(
                    session_id = ?session_id,
                    internal_id = %control.internal_id,
                    sdp_bytes = answer.len(),
                    "HomeKit controller SDP reoffer accepted"
                );
                tracing::debug!(
                    session_id = ?session_id,
                    internal_id = %control.internal_id,
                    sdp = %redacted_sdp(answer),
                    "HomeKit WebRTC SDP reoffer answer"
                );
            }
            Err(error) => tracing::warn!(
                session_id = ?session_id,
                internal_id = %control.internal_id,
                %error,
                "HomeKit controller SDP reoffer rejected"
            ),
        }
        result.map_err(anyhow::Error::msg)
    }

    pub(crate) fn homekit_transport_state(
        &self,
        session_id: HapSessionId,
    ) -> Option<HomeKitTransportState> {
        reap_finished_threads(&self.live.inner);
        self.live
            .inner
            .homekit_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|control| control.state())
    }

    pub(crate) fn close_homekit_session(&self, session_id: HapSessionId) -> bool {
        reap_finished_threads(&self.live.inner);
        let control = self
            .live
            .inner
            .homekit_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
        let Some(control) = control else {
            tracing::debug!(session_id = ?session_id, "HomeKit WebRTC close requested for unknown session");
            return false;
        };
        tracing::info!(
            session_id = ?session_id,
            internal_id = %control.internal_id,
            source = ?control.source,
            state = ?control.state(),
            "closing HomeKit WebRTC transport"
        );
        control.close();
        if !control.wait_for_finish() {
            tracing::warn!("HomeKit WebRTC session did not finish before close timeout");
        } else {
            join_session_thread(&self.live.inner, control.internal_id);
        }
        reap_finished_threads(&self.live.inner);
        true
    }

    fn homekit_control(
        &self,
        session_id: HapSessionId,
    ) -> anyhow::Result<Arc<HomeKitSessionControl>> {
        reap_finished_threads(&self.live.inner);
        self.live
            .inner
            .homekit_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown HomeKit WebRTC session"))
    }

    pub(crate) fn health_snapshot(&self) -> WebRtcHealth {
        reap_finished_threads(&self.live.inner);
        let now = Instant::now();
        let (sources, session_ids, active_main, active_sub, source_bitrate_bps, mut session_queues) = {
            let source_states = self
                .live
                .inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut session_ids = HashSet::new();
            let mut queue_ids = HashSet::new();
            let mut active_main = 0;
            let mut active_sub = 0;
            let mut source_bitrate_bps = 0u64;
            let mut session_queues = Vec::new();
            let mut sources = source_states
                .iter()
                .map(|(source, state)| {
                    for subscriber in &state.subscribers {
                        session_ids.insert(subscriber.id);
                        if queue_ids.insert((subscriber.id, subscriber.track_id.clone())) {
                            session_queues.push(WebRtcSessionQueueHealth {
                                session_id: subscriber.id,
                                track_id: subscriber.track_id.clone(),
                                camera_ip: source.camera_ip,
                                stream: source.stream,
                                depth: subscriber.tx.len(),
                                high_water: subscriber
                                    .queue_stats
                                    .high_water
                                    .load(Ordering::Relaxed),
                                written_frames: subscriber
                                    .queue_stats
                                    .written_frames
                                    .load(Ordering::Relaxed),
                                full_drops: subscriber
                                    .queue_stats
                                    .full_drops
                                    .load(Ordering::Relaxed),
                                discarded_frames: subscriber
                                    .queue_stats
                                    .discarded_frames
                                    .load(Ordering::Relaxed),
                                recovery_drops: subscriber
                                    .queue_stats
                                    .recovery_drops
                                    .load(Ordering::Relaxed),
                            });
                        }
                    }
                    match source.stream {
                        StreamKind::Main => active_main += state.subscribers.len(),
                        StreamKind::Sub => active_sub += state.subscribers.len(),
                    }
                    source_bitrate_bps =
                        source_bitrate_bps.saturating_add(state.bitrate.estimate_bps.unwrap_or(0));
                    WebRtcSourceHealth {
                        camera_ip: source.camera_ip,
                        stream: source.stream,
                        subscribers: state.subscribers.len(),
                        bitrate_bps: state.bitrate.estimate_bps,
                        has_keyframe: state.keyframe.is_some(),
                        keyframe_age_ms: state.keyframe.as_ref().map(|frame| {
                            now.saturating_duration_since(frame.received_at)
                                .as_millis()
                                .try_into()
                                .unwrap_or(u64::MAX)
                        }),
                    }
                })
                .collect::<Vec<_>>();
            sources.sort_unstable_by_key(|source| (source.camera_ip, source.stream.to_string()));
            (
                sources,
                session_ids,
                active_main,
                active_sub,
                source_bitrate_bps,
                session_queues,
            )
        };
        session_queues.sort_unstable_by_key(|queue| (queue.session_id.0, queue.track_id.clone()));
        let queued_frames = session_queues.iter().map(|queue| queue.depth).sum();
        let queue_depth_max = session_queues
            .iter()
            .map(|queue| queue.depth)
            .max()
            .unwrap_or(0);

        let (
            adaptive_sessions,
            mut requested_auto,
            mut requested_high,
            mut requested_low,
            mut estimates,
        ) = {
            let sessions = self
                .live
                .inner
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut requested_auto = 0;
            let mut requested_high = 0;
            let mut requested_low = 0;
            let mut estimates = Vec::new();
            for control in sessions.values() {
                let status = control.status();
                match status.requested_quality {
                    StreamQuality::Auto => requested_auto += 1,
                    StreamQuality::High => requested_high += 1,
                    StreamQuality::Low => requested_low += 1,
                }
                if let Some(estimate) = status.estimated_bitrate_bps {
                    estimates.push(estimate);
                }
            }
            (
                sessions.len(),
                requested_auto,
                requested_high,
                requested_low,
                estimates,
            )
        };
        let (multi_track_sessions, multi_tracks) = {
            let sessions = self
                .live
                .inner
                .multi_track_sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut multi_tracks = 0;
            for control in sessions.values() {
                let status = control.status();
                multi_tracks += status.tracks.len();
                for track in status.tracks {
                    match track.requested_quality {
                        StreamQuality::Auto => requested_auto += 1,
                        StreamQuality::High => requested_high += 1,
                        StreamQuality::Low => requested_low += 1,
                    }
                }
                if let Some(estimate) = status.estimated_bitrate_bps {
                    estimates.push(estimate);
                }
            }
            (sessions.len(), multi_tracks)
        };
        let estimated_bitrate_min_bps = estimates.iter().copied().min();
        let estimated_bitrate_max_bps = estimates.iter().copied().max();
        let estimated_bitrate_avg_bps = (!estimates.is_empty())
            .then(|| estimates.iter().copied().sum::<u64>() / estimates.len() as u64);
        let active_sessions = session_ids.len();

        WebRtcHealth {
            active_sessions,
            adaptive_sessions,
            multi_track_sessions,
            multi_tracks,
            fixed_sessions: active_sessions
                .saturating_sub(adaptive_sessions + multi_track_sessions),
            active_main,
            active_sub,
            requested_auto,
            requested_high,
            requested_low,
            estimated_bitrate_min_bps,
            estimated_bitrate_avg_bps,
            estimated_bitrate_max_bps,
            source_bitrate_bps,
            published_frames: self.live.inner.published_frames.load(Ordering::Relaxed),
            published_bytes: self.live.inner.published_bytes.load(Ordering::Relaxed),
            delivered_frames: self.live.inner.delivered_frames.load(Ordering::Relaxed),
            written_frames: self.live.inner.written_frames.load(Ordering::Relaxed),
            queue_capacity: FRAME_QUEUE_CAPACITY,
            queued_frames,
            queue_depth_max,
            queue_high_water: self.live.inner.queue_high_water.load(Ordering::Relaxed),
            queue_drops: self.live.inner.queue_drops.load(Ordering::Relaxed),
            queue_discarded_frames: self
                .live
                .inner
                .queue_discarded_frames
                .load(Ordering::Relaxed),
            queue_recovery_drops: self.live.inner.queue_recovery_drops.load(Ordering::Relaxed),
            session_queues,
            sources,
        }
    }

    fn accept_offer_inner(&self, plan: SessionPlan, offer: SdpOffer) -> anyhow::Result<Session> {
        reap_finished_threads(&self.live.inner);
        let SessionIo {
            rtc,
            socket,
            poller,
            answer,
        } = accept_session(offer)?;
        let (tx, rx) = bounded(FRAME_QUEUE_CAPACITY);
        let latest_keyframe = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let session_id = next_session_id(&self.live.inner);
        let sender = SessionSender {
            id: session_id,
            track_id: None,
            tx,
            queue_stats: Arc::new(SessionQueueStats::default()),
            queue_high_water: self.live.inner.queue_high_water.clone(),
            latest_keyframe,
            poller: poller.clone(),
            shutdown: shutdown.clone(),
        };

        let (subscription, control) = match plan {
            SessionPlan::Fixed {
                source,
                demand_guard,
            } => (
                SourceSubscription::fixed(self.live.inner.clone(), sender, source, demand_guard),
                None,
            ),
            SessionPlan::Adaptive {
                camera_ip,
                has_sub_stream,
                quality,
                recording_label,
            } => {
                let high_source = Source {
                    camera_ip,
                    stream: StreamKind::Main,
                };
                let low_source = has_sub_stream.then_some(Source {
                    camera_ip,
                    stream: StreamKind::Sub,
                });
                let initial_stream = match quality {
                    StreamQuality::High => StreamKind::Main,
                    StreamQuality::Auto | StreamQuality::Low => {
                        low_source.map_or(StreamKind::Main, |source| source.stream)
                    }
                };
                let control =
                    Arc::new(SessionControl::new(quality, initial_stream, poller.clone()));
                let subscription = SourceSubscription::adaptive(
                    self.live.inner.clone(),
                    sender,
                    high_source,
                    low_source,
                    control.clone(),
                    self.recording_demand.clone(),
                    recording_label,
                );
                self.live
                    .inner
                    .sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(session_id, control.clone());
                (subscription, Some(control))
            }
        };

        let thread_name = format!(
            "webrtc-{}-{}",
            subscription.active_source.camera_ip, subscription.active_source.stream
        );
        let inner = self.live.inner.clone();
        let thread_inner = inner.clone();
        let registered_session = control.is_some();
        let thread = match std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                if let Err(error) = run_session(rtc, socket, poller, rx, subscription, shutdown) {
                    tracing::debug!(%error, "WebRTC session stopped with error");
                }
                if registered_session {
                    thread_inner
                        .sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&session_id);
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                if registered_session {
                    inner
                        .sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&session_id);
                }
                return Err(error.into());
            }
        };
        self.live
            .inner
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(SessionThread {
                session_id,
                handle: thread,
            });

        Ok(Session {
            id: session_id,
            answer,
        })
    }

    pub fn shutdown(&self) {
        let homekit_controls = self
            .live
            .inner
            .homekit_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for control in homekit_controls {
            control.close();
        }
        let browser_controls = self
            .live
            .inner
            .multi_track_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for control in browser_controls {
            control.close();
        }
        let senders = {
            let mut sources = self
                .live
                .inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sources
                .values_mut()
                .flat_map(|state| state.subscribers.drain(..))
                .collect::<Vec<_>>()
        };
        for sender in senders {
            sender.shutdown.store(true, Ordering::Release);
            if let Err(error) = sender.poller.notify() {
                tracing::debug!(%error, "unable to wake WebRTC session for shutdown");
            }
        }

        let threads = std::mem::take(
            &mut *self
                .live
                .inner
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for thread in threads {
            if thread.handle.join().is_err() {
                tracing::warn!("WebRTC session thread panicked");
            }
        }
        self.live
            .inner
            .multi_track_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.live
            .inner
            .homekit_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

struct SessionIo {
    rtc: Rtc,
    socket: UdpSocket,
    poller: Arc<Poller>,
    answer: SdpAnswer,
}

fn accept_session(offer: SdpOffer) -> anyhow::Result<SessionIo> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_nonblocking(true)?;
    let port = socket.local_addr()?.port();
    let mut rtc = rtc_config().build(Instant::now());
    for candidate in host_candidates(port)? {
        let _ = rtc.add_local_candidate(candidate);
    }
    let answer = rtc.sdp_api().accept_offer(offer)?;
    let poller = Arc::new(Poller::new()?);
    // SAFETY: The session thread owns the socket and removes it before either resource drops.
    unsafe {
        poller.add(&socket, PollEvent::readable(UDP_EVENT_KEY))?;
    }
    Ok(SessionIo {
        rtc,
        socket,
        poller,
        answer,
    })
}

fn host_candidates(port: u16) -> anyhow::Result<Vec<Candidate>> {
    candidate_addresses()
        .into_iter()
        .map(|ip| Candidate::host(SocketAddr::new(IpAddr::V4(ip), port), "udp").map_err(Into::into))
        .collect()
}

fn homekit_track_id(session_id: HapSessionId) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(8 + session_id.as_bytes().len() * 2);
    value.push_str("homekit-");
    for byte in session_id.as_bytes() {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    value
}

fn next_session_id(inner: &Inner) -> SessionId {
    let sequence = inner.next_session_id.fetch_add(1, Ordering::Relaxed);
    SessionId(
        sequence
            .checked_add(1)
            .expect("WebRTC session ID sequence overflowed"),
    )
}

fn initial_stream(quality: StreamQuality, low_source: Option<Source>) -> StreamKind {
    match quality {
        StreamQuality::High => StreamKind::Main,
        StreamQuality::Auto | StreamQuality::Low => {
            low_source.map_or(StreamKind::Main, |source| source.stream)
        }
    }
}

fn validate_multi_tracks(plans: &[TrackPlan]) -> anyhow::Result<()> {
    if plans.is_empty() || plans.len() > MAX_TRACKS {
        anyhow::bail!("browser offer must contain 1 to {MAX_TRACKS} video tracks");
    }
    let mut track_ids = HashSet::with_capacity(plans.len());
    let mut mids = HashSet::with_capacity(plans.len());
    let mut promoted_tracks = 0;
    for plan in plans {
        if !track_ids.insert(plan.track_id.clone()) {
            anyhow::bail!(
                "browser offer contains duplicate live track ID '{}'",
                plan.track_id
            );
        }
        if plan.mid.is_empty()
            || plan.mid.len() > MAX_MID_BYTES
            || Mid::from(plan.mid.as_str()).to_string() != plan.mid
        {
            anyhow::bail!("browser offer contains invalid SDP MID '{}'", plan.mid);
        }
        if !mids.insert(plan.mid.as_str()) {
            anyhow::bail!("browser offer contains duplicate SDP MID '{}'", plan.mid);
        }
        if plan.quality != StreamQuality::Low {
            promoted_tracks += 1;
        }
    }
    if promoted_tracks > 1 {
        anyhow::bail!("browser offer may promote only one camera track");
    }
    Ok(())
}

fn bind_mids(plans: &[TrackPlan]) -> BTreeMap<TrackId, Mid> {
    plans
        .iter()
        .map(|plan| (plan.track_id.clone(), Mid::from(plan.mid.as_str())))
        .collect()
}

pub(crate) fn rtc_config() -> RtcConfig {
    #[cfg(target_os = "macos")]
    let provider = str0m_apple_crypto::default_provider();
    #[cfg(target_os = "linux")]
    let provider = str0m_openssl::default_provider();
    #[cfg(windows)]
    let provider = str0m_wincrypto::default_provider();

    RtcConfig::new()
        .set_crypto_provider(Arc::new(provider))
        .clear_codecs()
        .enable_h264(true)
        .enable_h265(true)
        .enable_bwe(Some(INITIAL_EGRESS_BITRATE))
}

/// Tunnel interfaces carry addresses a HomeKit controller on the LAN cannot
/// reach, so advertising them only adds ICE checks that time out.
fn is_tunnel_interface(name: &str) -> bool {
    const PREFIXES: [&str; 5] = ["utun", "ipsec", "ppp", "tun", "tap"];
    PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

fn candidate_addresses() -> Vec<Ipv4Addr> {
    let mut addresses = vec![Ipv4Addr::LOCALHOST];
    if let Ok(interfaces) = NetworkInterface::show() {
        addresses.extend(
            interfaces
                .into_iter()
                .filter(|interface| !is_tunnel_interface(&interface.name))
                .flat_map(|interface| {
                    interface.addr.into_iter().filter_map(|address| {
                        let Addr::V4(address) = address else {
                            return None;
                        };
                        // str0m rejects link-local addresses, so a self-assigned
                        // APIPA interface fails candidate gathering outright.
                        (!address.ip.is_unspecified()
                            && !address.ip.is_loopback()
                            && !address.ip.is_link_local())
                        .then_some(address.ip)
                    })
                }),
        );
    }
    addresses.sort_unstable();
    addresses.dedup();
    addresses
}

fn run_session(
    mut rtc: Rtc,
    socket: UdpSocket,
    poller: Arc<Poller>,
    rx: Receiver<SessionCommand>,
    mut subscription: SourceSubscription,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let result = drive_session(
        &mut rtc,
        &socket,
        &poller,
        &rx,
        &mut subscription,
        &shutdown,
    );
    let queue_stats = subscription.sender.queue_stats.clone();
    let inner = subscription.inner.clone();
    drop(subscription);
    let abandoned_frames = rx.try_iter().count() as u64;
    queue_stats
        .discarded_frames
        .fetch_add(abandoned_frames, Ordering::Relaxed);
    inner
        .queue_discarded_frames
        .fetch_add(abandoned_frames, Ordering::Relaxed);
    let delete_result = poller.delete(&socket);
    result.and_then(|()| delete_result.map_err(Into::into))
}

struct HomeKitSessionRuntime {
    session: Str0mSession,
    socket: UdpSocket,
    poller: Arc<Poller>,
    frame_rx: Receiver<SessionCommand>,
    command_rx: Receiver<HomeKitCommand>,
    subscription: SourceSubscription,
    shutdown: Arc<AtomicBool>,
    control: Arc<HomeKitSessionControl>,
}

fn run_homekit_session(mut runtime: HomeKitSessionRuntime) -> anyhow::Result<()> {
    let result = drive_homekit_session(
        &mut runtime.session,
        &runtime.socket,
        &runtime.poller,
        &runtime.frame_rx,
        &runtime.command_rx,
        &runtime.subscription,
        &runtime.shutdown,
        &runtime.control,
    );
    let queue_stats = runtime.subscription.sender.queue_stats.clone();
    let inner = runtime.subscription.inner.clone();
    drop(runtime.subscription);
    let abandoned_frames = runtime.frame_rx.try_iter().count() as u64;
    queue_stats
        .discarded_frames
        .fetch_add(abandoned_frames, Ordering::Relaxed);
    inner
        .queue_discarded_frames
        .fetch_add(abandoned_frames, Ordering::Relaxed);
    let delete_result = runtime.poller.delete(&runtime.socket);
    result.and_then(|()| delete_result.map_err(Into::into))
}

#[allow(clippy::too_many_arguments)]
fn drive_homekit_session(
    session: &mut Str0mSession,
    socket: &UdpSocket,
    poller: &Poller,
    frame_rx: &Receiver<SessionCommand>,
    command_rx: &Receiver<HomeKitCommand>,
    subscription: &SourceSubscription,
    shutdown: &AtomicBool,
    control: &HomeKitSessionControl,
) -> anyhow::Result<()> {
    let mut events = Events::new();
    let mut udp_buffer = vec![0; UDP_PACKET_CAPACITY];
    let mut keyframe_gate = KeyframeGate::new();
    let mut media_clock = MediaClock::default();
    let mut last_frame_sequence = None;
    let mut recovering_queue_gap = false;
    let mut keyframe_prepared = false;
    let mut consecutive_unwritable_frames = 0_u64;
    let mut next_bitrate_refresh = Instant::now();
    let mut peer_destinations = HashMap::new();
    let mut next_timeout = drain_homekit_outputs(session, socket, control, subscription)?;

    'session: loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        loop {
            match command_rx.try_recv() {
                Ok(HomeKitCommand::ApplyAnswer {
                    sdp,
                    candidates,
                    response,
                }) => {
                    tracing::debug!(
                        session_id = ?control.session_id,
                        internal_id = %control.internal_id,
                        sdp_bytes = sdp.len(),
                        candidate_count = candidates.len(),
                        "HomeKit WebRTC session thread applying controller answer"
                    );
                    match session.apply_answer(&sdp, &candidates) {
                        Ok(()) => {
                            control.set_state(HomeKitTransportState::Connecting);
                            tracing::info!(
                                session_id = ?control.session_id,
                                internal_id = %control.internal_id,
                                "str0m applied HomeKit controller answer; ICE/DTLS connecting"
                            );
                            match drain_homekit_outputs(session, socket, control, subscription) {
                                Ok(deadline) => {
                                    next_timeout = deadline;
                                    let _ = response.send(Ok(()));
                                }
                                Err(error) => {
                                    let message = error.to_string();
                                    let _ = response.send(Err(message));
                                    return Err(error);
                                }
                            }
                        }
                        Err(error) => {
                            let message = error.to_string();
                            let _ = response.send(Err(message.clone()));
                            anyhow::bail!(message);
                        }
                    }
                }
                Ok(HomeKitCommand::AcceptReoffer { sdp, response }) => {
                    tracing::debug!(
                        session_id = ?control.session_id,
                        internal_id = %control.internal_id,
                        sdp_bytes = sdp.len(),
                        "HomeKit WebRTC session thread accepting controller reoffer"
                    );
                    match session.accept_reoffer(&sdp) {
                        Ok(answer) => {
                            match drain_homekit_outputs(session, socket, control, subscription) {
                                Ok(deadline) => {
                                    next_timeout = deadline;
                                    let _ = response.send(Ok(answer));
                                }
                                Err(error) => {
                                    let message = error.to_string();
                                    let _ = response.send(Err(message));
                                    return Err(error);
                                }
                            }
                        }
                        Err(error) => {
                            let message = error.to_string();
                            let _ = response.send(Err(message.clone()));
                            anyhow::bail!(message);
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => break 'session,
                Err(TryRecvError::Empty) => break,
            }
        }

        let connected = control.state() == HomeKitTransportState::Connected;
        let now = Instant::now();
        if now >= next_bitrate_refresh {
            session.set_desired_bitrate(subscription.desired_bitrate(StreamQuality::High));
            next_bitrate_refresh = now + DESIRED_BITRATE_REFRESH;
        }
        if connected && !keyframe_prepared {
            subscription.prepare_keyframe(frame_rx);
            last_frame_sequence = None;
            recovering_queue_gap = false;
            keyframe_prepared = true;
            tracing::info!(
                session_id = ?control.session_id,
                internal_id = %control.internal_id,
                source = ?control.source,
                "HomeKit WebRTC transport connected; waiting for video keyframe"
            );
        }
        if connected && keyframe_gate.allows(FrameOrigin::Cached, true) {
            let keyframe = subscription
                .sender
                .latest_keyframe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(keyframe) = keyframe {
                subscription.discard_queued_frames(frame_rx);
                last_frame_sequence = None;
                recovering_queue_gap = false;
                if write_homekit_frame(session, &keyframe, &mut media_clock)? {
                    consecutive_unwritable_frames = 0;
                    keyframe_gate.mark_written(FrameOrigin::Cached);
                    control.record_video_frame(&keyframe, FrameOrigin::Cached, None);
                    next_timeout = drain_homekit_outputs(session, socket, control, subscription)?;
                } else {
                    consecutive_unwritable_frames += 1;
                    tracing::debug!(
                        session_id = ?control.session_id,
                        internal_id = %control.internal_id,
                        source = ?control.source,
                        codec = ?keyframe.codec,
                        h264_profile_level_id = ?media_clock.h264_profile_level_id,
                        "cached HomeKit keyframe had no negotiated str0m payload"
                    );
                }
            }
        }

        loop {
            match frame_rx.try_recv() {
                Ok(SessionCommand::Frame {
                    sequence,
                    source,
                    frame,
                }) => {
                    if source != subscription.active_source {
                        subscription.record_discarded_frames(1);
                        continue;
                    }
                    if !keyframe_gate.observe_sequence(&mut last_frame_sequence, sequence) {
                        recovering_queue_gap = true;
                        tracing::debug!(
                            session_id = ?control.session_id,
                            internal_id = %control.internal_id,
                            sequence,
                            "HomeKit WebRTC frame queue gap; waiting for keyframe"
                        );
                    }
                    let frame_allowed = keyframe_gate.allows(FrameOrigin::Live, frame.is_keyframe);
                    let wrote = connected
                        && frame_allowed
                        && write_homekit_frame(session, &frame, &mut media_clock)?;
                    if wrote {
                        consecutive_unwritable_frames = 0;
                        keyframe_gate.mark_written(FrameOrigin::Live);
                        recovering_queue_gap = false;
                        control.record_video_frame(&frame, FrameOrigin::Live, Some(sequence));
                        subscription.record_written_frame();
                        next_timeout =
                            drain_homekit_outputs(session, socket, control, subscription)?;
                    } else if connected && frame_allowed {
                        consecutive_unwritable_frames += 1;
                        if consecutive_unwritable_frames == 1 {
                            tracing::debug!(
                                session_id = ?control.session_id,
                                internal_id = %control.internal_id,
                                source = ?control.source,
                                codec = ?frame.codec,
                                keyframe = frame.is_keyframe,
                                h264_profile_level_id = ?media_clock.h264_profile_level_id,
                                "HomeKit video frame had no compatible negotiated payload"
                            );
                        } else if consecutive_unwritable_frames == 100 {
                            tracing::warn!(
                                session_id = ?control.session_id,
                                internal_id = %control.internal_id,
                                source = ?control.source,
                                codec = ?frame.codec,
                                h264_profile_level_id = ?media_clock.h264_profile_level_id,
                                consecutive_unwritable_frames,
                                "HomeKit WebRTC is connected but cannot write the camera codec to the negotiated video media"
                            );
                        }
                        subscription.record_discarded_frames(1);
                    } else if connected && recovering_queue_gap && !frame_allowed {
                        subscription.record_discarded_frames(1);
                        subscription
                            .sender
                            .queue_stats
                            .recovery_drops
                            .fetch_add(1, Ordering::Relaxed);
                        subscription
                            .inner
                            .queue_recovery_drops
                            .fetch_add(1, Ordering::Relaxed);
                    } else {
                        subscription.record_discarded_frames(1);
                    }
                }
                Err(TryRecvError::Disconnected) => break 'session,
                Err(TryRecvError::Empty) => break,
            }
        }

        events.clear();
        poller.wait(
            &mut events,
            Some(next_timeout.saturating_duration_since(Instant::now())),
        )?;
        if events.iter().any(|event| event.key == UDP_EVENT_KEY) {
            loop {
                match socket.recv_from(&mut udp_buffer) {
                    Ok((length, source)) => {
                        let destination =
                            if let Some(destination) = peer_destinations.get(&source.ip()) {
                                *destination
                            } else {
                                let destination =
                                    route_local_address(source, socket.local_addr()?.port())?;
                                peer_destinations.insert(source.ip(), destination);
                                destination
                            };
                        let receive = Receive {
                            proto: Protocol::Udp,
                            source,
                            destination,
                            contents: (&udp_buffer[..length]).try_into()?,
                        };
                        control.record_udp_received(length);
                        tracing::trace!(
                            session_id = ?control.session_id,
                            internal_id = %control.internal_id,
                            packet_kind = webrtc_datagram_kind(&udp_buffer[..length]),
                            bytes = length,
                            %source,
                            %destination,
                            "HomeKit WebRTC UDP datagram received"
                        );
                        session.handle_input(Input::Receive(Instant::now(), receive))?;
                        next_timeout =
                            drain_homekit_outputs(session, socket, control, subscription)?;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
            poller.modify(socket, PollEvent::readable(UDP_EVENT_KEY))?;
        }

        let now = Instant::now();
        if next_timeout <= now {
            session.handle_input(Input::Timeout(now))?;
            next_timeout = drain_homekit_outputs(session, socket, control, subscription)?;
        }
    }
    Ok(())
}

fn drain_homekit_outputs(
    session: &mut Str0mSession,
    socket: &UdpSocket,
    control: &HomeKitSessionControl,
    subscription: &SourceSubscription,
) -> anyhow::Result<Instant> {
    loop {
        match session.poll_output()? {
            Output::Timeout(deadline) => return Ok(deadline),
            Output::Transmit(transmit) => {
                socket.send_to(&transmit.contents, transmit.destination)?;
                control.record_udp_sent(transmit.contents.len());
                tracing::trace!(
                    session_id = ?control.session_id,
                    internal_id = %control.internal_id,
                    packet_kind = webrtc_datagram_kind(&transmit.contents),
                    bytes = transmit.contents.len(),
                    destination = %transmit.destination,
                    "HomeKit WebRTC UDP datagram sent"
                );
            }
            Output::Event(event) => {
                tracing::debug!(
                    session_id = ?control.session_id,
                    internal_id = %control.internal_id,
                    ?event,
                    "HomeKit str0m event"
                );
                if terminal_session_event(&event) {
                    tracing::warn!(
                        session_id = ?control.session_id,
                        internal_id = %control.internal_id,
                        ?event,
                        "HomeKit WebRTC transport reached terminal state"
                    );
                    anyhow::bail!("HomeKit WebRTC transport ended: {event:?}");
                }
                match event {
                    Event::Connected => {
                        control.set_state(HomeKitTransportState::Connected);
                        tracing::info!(
                            session_id = ?control.session_id,
                            internal_id = %control.internal_id,
                            source = ?control.source,
                            "HomeKit WebRTC ICE and DTLS connected"
                        );
                    }
                    Event::IceConnectionStateChange(state) => {
                        tracing::info!(
                            session_id = ?control.session_id,
                            internal_id = %control.internal_id,
                            ?state,
                            "HomeKit WebRTC ICE state changed"
                        );
                    }
                    Event::MediaAdded(media) => {
                        tracing::info!(
                            session_id = ?control.session_id,
                            internal_id = %control.internal_id,
                            ?media,
                            "HomeKit WebRTC media negotiated"
                        );
                    }
                    Event::EgressBitrateEstimate(estimate) => {
                        let bitrate = match estimate {
                            BweKind::Twcc(bitrate) | BweKind::Remb(_, bitrate) => bitrate,
                            _ => continue,
                        };
                        tracing::trace!(
                            session_id = ?control.session_id,
                            internal_id = %control.internal_id,
                            %bitrate,
                            "HomeKit WebRTC egress bitrate estimate updated"
                        );
                        subscription.update_estimate(bitrate);
                    }
                    _ => {}
                }
            }
        }
    }
}

fn write_homekit_frame(
    session: &mut Str0mSession,
    frame: &MediaFrame,
    media_clock: &mut MediaClock,
) -> anyhow::Result<bool> {
    if matches!(frame.codec, VideoCodec::H264)
        && frame.is_keyframe
        && let Some(profile_level_id) = frame.data.h264_profile_level_id()
    {
        media_clock.h264_profile_level_id = Some(profile_level_id);
    }
    let media_time = media_clock.media_time(frame);
    session
        .write_video(
            match frame.codec {
                VideoCodec::H264 => HapVideoCodec::H264,
                VideoCodec::H265 => HapVideoCodec::H265,
            },
            media_clock.h264_profile_level_id,
            frame.received_at,
            MediaTime::from_90khz(media_time),
            frame.data.annexb(),
        )
        .map_err(Into::into)
}

fn run_multi_track_session(
    mut rtc: Rtc,
    socket: UdpSocket,
    poller: Arc<Poller>,
    mut tracks: Vec<TrackRuntime>,
    control: Arc<MultiTrackControl>,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let result =
        drive_multi_track_session(&mut rtc, &socket, &poller, &mut tracks, &control, &shutdown);
    for track in tracks {
        let queue_stats = track.subscription.sender.queue_stats.clone();
        let inner = track.subscription.inner.clone();
        let abandoned_frames = track.rx.try_iter().count() as u64;
        queue_stats
            .discarded_frames
            .fetch_add(abandoned_frames, Ordering::Relaxed);
        inner
            .queue_discarded_frames
            .fetch_add(abandoned_frames, Ordering::Relaxed);
    }
    let delete_result = poller.delete(&socket);
    result.and_then(|()| delete_result.map_err(Into::into))
}

fn drive_multi_track_session(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    poller: &Poller,
    tracks: &mut [TrackRuntime],
    control: &MultiTrackControl,
    shutdown: &AtomicBool,
) -> anyhow::Result<()> {
    let mut events = Events::new();
    let mut udp_buffer = vec![0; UDP_PACKET_CAPACITY];
    let mut configured_desired_bitrate = None;
    let mut next_desired_bitrate_refresh = Instant::now();
    let mut peer_destinations = HashMap::new();
    let mut connected = false;
    let mut next_timeout = drain_multi_track_outputs(rtc, socket, &mut connected, control)?;

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let now = Instant::now();
        if now >= next_desired_bitrate_refresh {
            let desired_bitrate = desired_bitrate(tracks);
            if Some(desired_bitrate) != configured_desired_bitrate {
                rtc.bwe().set_desired_bitrate(desired_bitrate);
                configured_desired_bitrate = Some(desired_bitrate);
            }
            next_desired_bitrate_refresh = now + DESIRED_BITRATE_REFRESH;
        }
        update_track_estimates(
            tracks,
            control.estimated_bitrate_bps.load(Ordering::Acquire),
        );

        for track in &mut *tracks {
            track.subscription.select_source(rtc, Some(track.mid), now);
            if connected && !track.keyframe_prepared {
                track.subscription.prepare_keyframe(&track.rx);
                track.last_frame_sequence = None;
                track.recovering_queue_gap = false;
                track.keyframe_prepared = true;
            }
            if connected && track.keyframe_gate.allows(FrameOrigin::Cached, true) {
                let keyframe = track
                    .subscription
                    .sender
                    .latest_keyframe
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some(keyframe) = keyframe {
                    track.subscription.discard_queued_frames(&track.rx);
                    track.last_frame_sequence = None;
                    track.recovering_queue_gap = false;
                    let wrote_frame =
                        write_frame(rtc, Some(track.mid), &keyframe, &mut track.media_clock)?;
                    if wrote_frame {
                        track.keyframe_gate.mark_written(FrameOrigin::Cached);
                        next_timeout =
                            drain_multi_track_outputs(rtc, socket, &mut connected, control)?;
                    }
                }
            }
        }

        loop {
            let mut handled_frame = false;
            for track in &mut *tracks {
                let Ok(SessionCommand::Frame {
                    sequence,
                    source,
                    frame,
                }) = track.rx.try_recv()
                else {
                    continue;
                };
                handled_frame = true;
                if track
                    .subscription
                    .finish_switch_on_frame(source, frame.is_keyframe)
                {
                    track.reset_source_state();
                }
                if source != track.subscription.active_source {
                    track.subscription.record_discarded_frames(1);
                    continue;
                }
                if !track
                    .keyframe_gate
                    .observe_sequence(&mut track.last_frame_sequence, sequence)
                {
                    track.recovering_queue_gap = true;
                    tracing::debug!(track_id = %track.track_id, sequence, "shared WebRTC track queue gap; waiting for keyframe");
                }
                if !track.received_source_frame {
                    tracing::debug!(
                        track_id = %track.track_id,
                        codec = ?frame.codec,
                        keyframe = frame.is_keyframe,
                        "received first shared WebRTC source frame"
                    );
                    track.received_source_frame = true;
                }
                let frame_allowed = track
                    .keyframe_gate
                    .allows(FrameOrigin::Live, frame.is_keyframe);
                let wrote_frame = connected
                    && frame_allowed
                    && write_frame(rtc, Some(track.mid), &frame, &mut track.media_clock)?;
                if wrote_frame {
                    track.keyframe_gate.mark_written(FrameOrigin::Live);
                    track.recovering_queue_gap = false;
                    track.subscription.record_written_frame();
                    next_timeout = drain_multi_track_outputs(rtc, socket, &mut connected, control)?;
                } else if connected && track.recovering_queue_gap && !frame_allowed {
                    track.subscription.record_discarded_frames(1);
                    track
                        .subscription
                        .sender
                        .queue_stats
                        .recovery_drops
                        .fetch_add(1, Ordering::Relaxed);
                    track
                        .subscription
                        .inner
                        .queue_recovery_drops
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    track.subscription.record_discarded_frames(1);
                }
            }
            if !handled_frame {
                break;
            }
        }

        events.clear();
        poller.wait(
            &mut events,
            Some(next_timeout.saturating_duration_since(Instant::now())),
        )?;
        if events.iter().any(|event| event.key == UDP_EVENT_KEY) {
            loop {
                match socket.recv_from(&mut udp_buffer) {
                    Ok((length, source)) => {
                        let destination =
                            if let Some(destination) = peer_destinations.get(&source.ip()) {
                                *destination
                            } else {
                                let destination =
                                    route_local_address(source, socket.local_addr()?.port())?;
                                peer_destinations.insert(source.ip(), destination);
                                destination
                            };
                        let receive = Receive {
                            proto: Protocol::Udp,
                            source,
                            destination,
                            contents: (&udp_buffer[..length]).try_into()?,
                        };
                        rtc.handle_input(Input::Receive(Instant::now(), receive))?;
                        next_timeout =
                            drain_multi_track_outputs(rtc, socket, &mut connected, control)?;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
            poller.modify(socket, PollEvent::readable(UDP_EVENT_KEY))?;
        }

        let now = Instant::now();
        if next_timeout <= now {
            rtc.handle_input(Input::Timeout(now))?;
            next_timeout = drain_multi_track_outputs(rtc, socket, &mut connected, control)?;
        }
    }

    Ok(())
}

fn desired_bitrate(tracks: &[TrackRuntime]) -> Bitrate {
    let desired_bps = tracks.iter().fold(0u64, |total, track| {
        let quality = track
            .subscription
            .requested_quality()
            .unwrap_or(StreamQuality::Low);
        let bitrate = match quality {
            StreamQuality::Low => track.subscription.low_delivery_bitrate(),
            StreamQuality::Auto | StreamQuality::High => {
                track.subscription.desired_bitrate(quality)
            }
        };
        total.saturating_add(bitrate.as_u64())
    });
    Bitrate::bps(desired_bps.min(MAX_DESIRED_BITRATE.as_u64()))
}

fn update_track_estimates(tracks: &mut [TrackRuntime], aggregate_bps: u64) {
    let background_reserve_bps = tracks.iter().fold(0u64, |total, track| {
        total.saturating_add(track.subscription.low_delivery_bitrate().as_u64())
    });
    for track in tracks {
        let own_low_bps = track.subscription.low_delivery_bitrate().as_u64();
        let available_bps = available_bitrate(aggregate_bps, background_reserve_bps, own_low_bps);
        track
            .subscription
            .update_estimate(Bitrate::bps(available_bps));
    }
}

const fn available_bitrate(aggregate_bps: u64, total_low_bps: u64, own_low_bps: u64) -> u64 {
    aggregate_bps.saturating_sub(total_low_bps.saturating_sub(own_low_bps))
}

fn drive_session(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    poller: &Poller,
    rx: &Receiver<SessionCommand>,
    subscription: &mut SourceSubscription,
    shutdown: &AtomicBool,
) -> anyhow::Result<()> {
    let mut events = Events::new();
    let mut udp_buffer = vec![0; UDP_PACKET_CAPACITY];
    let mut video_mid = None;
    let mut connected = false;
    let mut keyframe_gate = KeyframeGate::new();
    let mut media_clock = MediaClock::default();
    let mut last_frame_sequence = None;
    let mut recovering_queue_gap = false;
    let mut received_source_frame = false;
    let mut keyframe_prepared = false;
    let mut configured_quality = None;
    let mut configured_desired_bitrate = None;
    let mut next_desired_bitrate_refresh = Instant::now();
    let mut peer_destinations = HashMap::new();
    let mut next_timeout =
        drain_outputs(rtc, socket, &mut video_mid, &mut connected, subscription)?;

    'session: loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let now = Instant::now();
        let requested_quality = subscription.requested_quality();
        if video_mid.is_some()
            && (requested_quality != configured_quality || now >= next_desired_bitrate_refresh)
        {
            let desired_bitrate =
                requested_quality.map(|quality| subscription.desired_bitrate(quality));
            if let Some(bitrate) = desired_bitrate
                && Some(bitrate) != configured_desired_bitrate
            {
                rtc.bwe().set_desired_bitrate(bitrate);
            }
            configured_quality = requested_quality;
            configured_desired_bitrate = desired_bitrate;
            next_desired_bitrate_refresh = now + DESIRED_BITRATE_REFRESH;
        }
        subscription.select_source(rtc, video_mid, Instant::now());
        if connected && !keyframe_prepared {
            subscription.prepare_keyframe(rx);
            last_frame_sequence = None;
            recovering_queue_gap = false;
            keyframe_prepared = true;
        }
        if connected && keyframe_gate.allows(FrameOrigin::Cached, true) {
            let keyframe = subscription
                .sender
                .latest_keyframe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(keyframe) = keyframe {
                subscription.discard_queued_frames(rx);
                last_frame_sequence = None;
                recovering_queue_gap = false;
                let wrote_frame = write_frame(rtc, video_mid, &keyframe, &mut media_clock)?;
                if wrote_frame {
                    keyframe_gate.mark_written(FrameOrigin::Cached);
                    next_timeout =
                        drain_outputs(rtc, socket, &mut video_mid, &mut connected, subscription)?;
                }
            }
        }
        loop {
            match rx.try_recv() {
                Ok(SessionCommand::Frame {
                    sequence,
                    source,
                    frame,
                }) => {
                    if subscription.finish_switch_on_frame(source, frame.is_keyframe) {
                        media_clock.reset_source();
                        last_frame_sequence = None;
                        recovering_queue_gap = false;
                        received_source_frame = false;
                        keyframe_gate.reset();
                    }
                    if source != subscription.active_source {
                        subscription.record_discarded_frames(1);
                        continue;
                    }
                    if !keyframe_gate.observe_sequence(&mut last_frame_sequence, sequence) {
                        recovering_queue_gap = true;
                        tracing::debug!(sequence, "WebRTC frame queue gap; waiting for keyframe");
                    }
                    if !received_source_frame {
                        tracing::debug!(
                            codec = ?frame.codec,
                            keyframe = frame.is_keyframe,
                            "received first WebRTC source frame"
                        );
                        received_source_frame = true;
                    }
                    let frame_allowed = keyframe_gate.allows(FrameOrigin::Live, frame.is_keyframe);
                    let wrote_frame = connected
                        && frame_allowed
                        && write_frame(rtc, video_mid, &frame, &mut media_clock)?;
                    if wrote_frame {
                        keyframe_gate.mark_written(FrameOrigin::Live);
                        recovering_queue_gap = false;
                        subscription.record_written_frame();
                        next_timeout = drain_outputs(
                            rtc,
                            socket,
                            &mut video_mid,
                            &mut connected,
                            subscription,
                        )?;
                    } else if connected && recovering_queue_gap && !frame_allowed {
                        subscription.record_discarded_frames(1);
                        subscription
                            .sender
                            .queue_stats
                            .recovery_drops
                            .fetch_add(1, Ordering::Relaxed);
                        subscription
                            .inner
                            .queue_recovery_drops
                            .fetch_add(1, Ordering::Relaxed);
                    } else {
                        subscription.record_discarded_frames(1);
                    }
                }
                Err(TryRecvError::Disconnected) => break 'session,
                Err(TryRecvError::Empty) => break,
            }
        }

        events.clear();
        poller.wait(
            &mut events,
            Some(next_timeout.saturating_duration_since(Instant::now())),
        )?;
        if events.iter().any(|event| event.key == UDP_EVENT_KEY) {
            loop {
                match socket.recv_from(&mut udp_buffer) {
                    Ok((length, source)) => {
                        let destination =
                            if let Some(destination) = peer_destinations.get(&source.ip()) {
                                *destination
                            } else {
                                let destination =
                                    route_local_address(source, socket.local_addr()?.port())?;
                                peer_destinations.insert(source.ip(), destination);
                                destination
                            };
                        let receive = Receive {
                            proto: Protocol::Udp,
                            source,
                            destination,
                            contents: (&udp_buffer[..length]).try_into()?,
                        };
                        rtc.handle_input(Input::Receive(Instant::now(), receive))?;
                        next_timeout = drain_outputs(
                            rtc,
                            socket,
                            &mut video_mid,
                            &mut connected,
                            subscription,
                        )?;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
            poller.modify(socket, PollEvent::readable(UDP_EVENT_KEY))?;
        }

        let now = Instant::now();
        if next_timeout <= now {
            rtc.handle_input(Input::Timeout(now))?;
            next_timeout =
                drain_outputs(rtc, socket, &mut video_mid, &mut connected, subscription)?;
        }
    }

    Ok(())
}

fn route_local_address(remote: SocketAddr, port: u16) -> std::io::Result<SocketAddr> {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    probe.connect(remote)?;
    Ok(SocketAddr::new(probe.local_addr()?.ip(), port))
}

const fn terminal_session_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Closed | Event::IceConnectionStateChange(IceConnectionState::Disconnected)
    )
}

fn drain_outputs(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    video_mid: &mut Option<Mid>,
    connected: &mut bool,
    subscription: &SourceSubscription,
) -> anyhow::Result<Instant> {
    loop {
        match rtc.poll_output()? {
            Output::Timeout(deadline) => return Ok(deadline),
            Output::Transmit(transmit) => {
                socket.send_to(&transmit.contents, transmit.destination)?;
            }
            Output::Event(event) if terminal_session_event(&event) => {
                anyhow::bail!("WebRTC transport ended")
            }
            Output::Event(Event::Connected) => {
                *connected = true;
                tracing::debug!("WebRTC session connected");
            }
            Output::Event(Event::MediaAdded(media)) if media.kind == MediaKind::Video => {
                let payloads = rtc
                    .writer(media.mid)
                    .map(|writer| {
                        writer
                            .payload_params()
                            .map(|params| (params.pt(), params.spec().codec, params.resend()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                tracing::debug!(?media, ?payloads, "WebRTC video media negotiated");
                *video_mid = Some(media.mid);
            }
            Output::Event(Event::EgressBitrateEstimate(estimate)) => {
                let bitrate = match estimate {
                    BweKind::Twcc(bitrate) | BweKind::Remb(_, bitrate) => bitrate,
                    _ => continue,
                };
                subscription.update_estimate(bitrate);
                tracing::trace!(%bitrate, "WebRTC egress bitrate estimate updated");
            }
            Output::Event(_) => {}
        }
    }
}

fn drain_multi_track_outputs(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    connected: &mut bool,
    control: &MultiTrackControl,
) -> anyhow::Result<Instant> {
    loop {
        match rtc.poll_output()? {
            Output::Timeout(deadline) => return Ok(deadline),
            Output::Transmit(transmit) => {
                socket.send_to(&transmit.contents, transmit.destination)?;
            }
            Output::Event(event) if terminal_session_event(&event) => {
                anyhow::bail!("WebRTC transport ended")
            }
            Output::Event(Event::Connected) => {
                *connected = true;
                tracing::debug!("shared WebRTC session connected");
            }
            Output::Event(Event::MediaAdded(media)) if media.kind == MediaKind::Video => {
                let payloads = rtc
                    .writer(media.mid)
                    .map(|writer| {
                        writer
                            .payload_params()
                            .map(|params| (params.pt(), params.spec().codec, params.resend()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                tracing::debug!(?media, ?payloads, "shared WebRTC video media negotiated");
            }
            Output::Event(Event::EgressBitrateEstimate(estimate)) => {
                let bitrate = match estimate {
                    BweKind::Twcc(bitrate) | BweKind::Remb(_, bitrate) => bitrate,
                    _ => continue,
                };
                control.update_estimate(bitrate);
                tracing::trace!(%bitrate, "shared WebRTC egress bitrate estimate updated");
            }
            Output::Event(_) => {}
        }
    }
}

#[derive(Default)]
struct MediaClock {
    first_timestamp: Option<Duration>,
    first_received_at: Option<Instant>,
    source_base_media_time: u64,
    last_media_time: Option<u64>,
    h264_profile_level_id: Option<u32>,
}

impl MediaClock {
    fn reset_source(&mut self) {
        self.first_timestamp = None;
        self.first_received_at = None;
        self.h264_profile_level_id = None;
        self.source_base_media_time = self
            .last_media_time
            .map_or(0, |last| last.saturating_add(DEFAULT_FRAME_TICKS));
    }

    fn media_time(&mut self, frame: &MediaFrame) -> u64 {
        let base_received_at = *self.first_received_at.get_or_insert(frame.received_at);
        let fallback = self
            .source_base_media_time
            .saturating_add(duration_to_ticks(
                frame
                    .received_at
                    .saturating_duration_since(base_received_at),
                90_000,
            ));
        let mut media_time = frame.timestamp.map_or(fallback, |timestamp| {
            let base = *self.first_timestamp.get_or_insert(timestamp);
            self.source_base_media_time
                .saturating_add(duration_to_ticks(timestamp.saturating_sub(base), 90_000))
        });
        if let Some(last) = self.last_media_time
            && media_time <= last
        {
            media_time = last.saturating_add(DEFAULT_FRAME_TICKS);
        }
        self.last_media_time = Some(media_time);
        media_time
    }
}

fn write_frame(
    rtc: &mut Rtc,
    video_mid: Option<Mid>,
    frame: &MediaFrame,
    media_clock: &mut MediaClock,
) -> anyhow::Result<bool> {
    let Some(mid) = video_mid else {
        return Ok(false);
    };
    let codec = match frame.codec {
        VideoCodec::H264 => Codec::H264,
        VideoCodec::H265 => Codec::H265,
    };
    if matches!(frame.codec, VideoCodec::H264)
        && frame.is_keyframe
        && let Some(profile_level_id) = frame.data.h264_profile_level_id()
    {
        media_clock.h264_profile_level_id = Some(profile_level_id);
    }
    let h264_profile_level_id = media_clock.h264_profile_level_id;
    let Some(payload_type) = rtc.writer(mid).and_then(|writer| {
        writer
            .payload_params()
            .find(|params| {
                if params.spec().codec != codec {
                    return false;
                }
                if codec != Codec::H264 {
                    return true;
                }
                if params.spec().format.packetization_mode != Some(1) {
                    return false;
                }
                h264_profile_level_id.is_none_or(|source_profile_level_id| {
                    params
                        .spec()
                        .format
                        .profile_level_id
                        .is_some_and(|payload_profile_level_id| {
                            h264_profiles_match(source_profile_level_id, payload_profile_level_id)
                        })
                })
            })
            .map(|params| params.pt())
    }) else {
        return Ok(false);
    };

    let first_write = media_clock.last_media_time.is_none();
    let media_time = media_clock.media_time(frame);
    rtc.writer(mid)
        .expect("video media disappeared after payload selection")
        .write(
            payload_type,
            frame.received_at,
            MediaTime::from_90khz(media_time),
            frame.data.annexb(),
        )?;
    if first_write {
        tracing::debug!(?codec, ?payload_type, "wrote first WebRTC video frame");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_frame(is_keyframe: bool) -> MediaFrame {
        live_frame_at(is_keyframe, Instant::now(), 4)
    }

    fn test_source() -> Source {
        Source {
            camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            stream: StreamKind::Sub,
        }
    }

    fn live_frame_at(is_keyframe: bool, received_at: Instant, bytes: usize) -> MediaFrame {
        MediaFrame {
            codec: VideoCodec::H264,
            is_keyframe,
            received_at,
            timestamp: None,
            data: Arc::new(MediaFrameData::new(Bytes::from(vec![0; bytes]))),
        }
    }

    #[test]
    fn homekit_sdp_diagnostics_redact_ice_passwords() {
        let sdp = "a=ice-ufrag:visible\r\na=ice-pwd:secret\r\na=candidate:1 1 UDP 1 127.0.0.1 1234 typ host\r\n";
        let redacted = redacted_sdp(sdp);

        assert!(redacted.contains("a=ice-ufrag:visible"));
        assert!(redacted.contains("a=ice-pwd:<redacted>"));
        assert!(redacted.contains("a=candidate:1 1 UDP 1 127.0.0.1 1234 typ host"));
        assert!(!redacted.contains("secret"));
    }

    #[test]
    fn homekit_packet_diagnostics_classify_ice_dtls_and_media() {
        assert_eq!(
            webrtc_datagram_kind(&[0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42]),
            "stun"
        );
        assert_eq!(webrtc_datagram_kind(&[22, 0xfe, 0xfd]), "dtls");
        assert_eq!(webrtc_datagram_kind(&[0x80, 96]), "rtp-or-rtcp");
        assert_eq!(webrtc_datagram_kind(&[0xff]), "unknown");
    }

    #[test]
    fn h264_profile_is_read_from_sps() {
        let frame = MediaFrameData::new(Bytes::from_static(&[0, 0, 0, 4, 0x67, 0x42, 0xc0, 0x1f]));

        assert_eq!(frame.h264_profile_level_id(), Some(0x42c01f));
    }

    #[test]
    fn constrained_baseline_matches_a_constrained_payload() {
        assert!(h264_profiles_match(0x42c01f, 0x42e01f));
        assert!(!h264_profiles_match(0x42c01f, 0x42001f));
    }

    #[test]
    fn desired_egress_bitrate_accounts_for_peak_frame_delivery() {
        assert_eq!(
            desired_egress_bitrate(512_000, 75 * 1024).as_u64(),
            6_144_000
        );
        assert_eq!(desired_egress_bitrate(512_000, 0).as_u64(), 640_000);
    }

    #[test]
    fn desired_egress_bitrate_respects_the_safety_cap() {
        assert_eq!(
            desired_egress_bitrate(8_000_000, 2 * 1024 * 1024),
            MAX_DESIRED_BITRATE
        );
    }

    #[test]
    fn focused_auto_track_keeps_capacity_after_low_stream_reservation() {
        let aggregate_bps = 20_000_000;
        let total_low_bps = 8 * 500_000;
        let focused_low_bps = 500_000;

        assert_eq!(
            available_bitrate(aggregate_bps, total_low_bps, focused_low_bps),
            16_500_000
        );
    }

    #[test]
    fn media_clock_rebases_each_source_onto_one_output_timeline() {
        let now = Instant::now();
        let mut clock = MediaClock::default();
        let mut low_keyframe = live_frame_at(true, now, 4);
        low_keyframe.timestamp = Some(Duration::from_secs(10));
        let mut low_delta = live_frame_at(false, now + Duration::from_millis(67), 4);
        low_delta.timestamp = Some(Duration::from_secs(10) + Duration::from_nanos(66_666_667));

        assert_eq!(clock.media_time(&low_keyframe), 0);
        assert_eq!(clock.media_time(&low_delta), 6_000);

        clock.reset_source();
        let mut high_keyframe = live_frame_at(true, now + Duration::from_secs(1), 4);
        high_keyframe.timestamp = Some(Duration::from_secs(42));
        let mut high_delta = live_frame_at(false, now + Duration::from_millis(40), 4);
        high_delta.timestamp = Some(Duration::from_secs(42) + Duration::from_millis(40));

        assert_eq!(clock.media_time(&high_keyframe), 9_000);
        assert_eq!(clock.media_time(&high_delta), 12_600);
    }

    #[test]
    fn candidate_list_always_supports_local_browser() {
        assert!(candidate_addresses().contains(&Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn candidate_list_excludes_link_local_and_tunnel_addresses() {
        for address in candidate_addresses() {
            assert!(
                !address.is_link_local(),
                "{address} is link-local and str0m rejects it as a candidate"
            );
        }
    }

    #[test]
    fn tunnel_interfaces_are_recognised_by_name() {
        for name in ["utun0", "utun7", "ipsec0", "ppp0", "tun1", "tap0"] {
            assert!(
                is_tunnel_interface(name),
                "{name} should be treated as a tunnel"
            );
        }
        for name in ["en0", "en11", "eth0", "bridge100", "lo0"] {
            assert!(!is_tunnel_interface(name), "{name} should be kept");
        }
    }

    #[test]
    fn loopback_route_uses_session_port() {
        let destination =
            route_local_address(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9), 12_345)
                .unwrap();

        assert_eq!(destination, SocketAddr::from(([127, 0, 0, 1], 12_345)));
    }

    #[test]
    fn configured_recording_demand_can_hold_session_activity() {
        let demand = RecordingDemand::new(std::time::Duration::ZERO);
        let webrtc = WebRtc::with_recording_demand(demand.clone());
        let guard = webrtc
            .recording_demand
            .as_ref()
            .map(|recording_demand| recording_demand.acquire("front-door/main"));

        assert!(demand.is_active("front-door/main"));
        drop(guard);
        assert!(!demand.is_active("front-door/main"));
    }

    #[test]
    fn full_session_queue_preserves_latest_keyframe() {
        let (tx, rx) = bounded(1);
        let latest_keyframe = Arc::new(Mutex::new(None));
        let sender = SessionSender {
            id: SessionId(1),
            track_id: None,
            tx,
            queue_stats: Arc::new(SessionQueueStats::default()),
            queue_high_water: Arc::new(AtomicUsize::new(0)),
            latest_keyframe: latest_keyframe.clone(),
            poller: Arc::new(Poller::new().unwrap()),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        sender
            .try_send(0, test_source(), live_frame(false))
            .unwrap();

        assert!(matches!(
            sender.try_send(1, test_source(), live_frame(true)),
            Err(TrySendError::Full(_))
        ));
        assert!(
            latest_keyframe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(|frame| frame.is_keyframe)
        );
        assert_eq!(sender.tx.len(), 1);
        assert_eq!(sender.queue_stats.high_water.load(Ordering::Relaxed), 1);
        assert_eq!(sender.queue_high_water.load(Ordering::Relaxed), 1);
        assert_eq!(sender.queue_stats.full_drops.load(Ordering::Relaxed), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(SessionCommand::Frame { sequence: 0, .. })
        ));
        assert_eq!(sender.tx.len(), 0);

        sender
            .try_send(2, test_source(), live_frame(false))
            .unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(SessionCommand::Frame { sequence: 2, .. })
        ));
    }

    #[test]
    fn health_snapshot_contains_current_and_lifetime_delivery_counters() {
        let webrtc = WebRtc::default();
        let inner = webrtc.live.inner.clone();
        let main_source = Source {
            camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            stream: StreamKind::Main,
        };
        let sub_source = Source {
            stream: StreamKind::Sub,
            ..main_source
        };
        let track_id = TrackId::parse("camera-0".to_owned()).unwrap();
        let queue_high_water = inner.queue_high_water.clone();
        let sender = |id, track_id| {
            let (tx, rx) = bounded(1);
            (
                SessionSender {
                    id: SessionId(id),
                    track_id,
                    tx,
                    queue_stats: Arc::new(SessionQueueStats::default()),
                    queue_high_water: queue_high_water.clone(),
                    latest_keyframe: Arc::new(Mutex::new(None)),
                    poller: Arc::new(Poller::new().unwrap()),
                    shutdown: Arc::new(AtomicBool::new(false)),
                },
                rx,
            )
        };
        let (adaptive_sender, _adaptive_rx) = sender(1, None);
        let (multi_track_sender, _multi_track_rx) = sender(2, Some(track_id.clone()));
        let (fixed_sender, _fixed_rx) = sender(3, None);
        let adaptive = SourceSubscription::fixed(inner.clone(), adaptive_sender, main_source, None);
        let browser =
            SourceSubscription::fixed(inner.clone(), multi_track_sender, sub_source, None);
        let fixed = SourceSubscription::fixed(inner.clone(), fixed_sender, main_source, None);

        let adaptive_control = Arc::new(SessionControl::new(
            StreamQuality::Auto,
            StreamKind::Main,
            Arc::new(Poller::new().unwrap()),
        ));
        adaptive_control
            .estimated_bitrate_bps
            .store(3_000_000, Ordering::Release);
        inner
            .sessions
            .lock()
            .unwrap()
            .insert(SessionId(1), adaptive_control);
        let multi_track = Arc::new(SessionControl::new(
            StreamQuality::Low,
            StreamKind::Sub,
            Arc::new(Poller::new().unwrap()),
        ));
        multi_track
            .estimated_bitrate_bps
            .store(1_500_000, Ordering::Release);
        let browser_control = Arc::new(MultiTrackControl {
            tracks: BTreeMap::from([(track_id, multi_track)]),
            estimated_bitrate_bps: AtomicU64::new(9_000_000),
            poller: Arc::new(Poller::new().unwrap()),
            shutdown: Arc::new(AtomicBool::new(false)),
            completion: SessionCompletion::default(),
        });
        inner
            .multi_track_sessions
            .lock()
            .unwrap()
            .insert(SessionId(2), browser_control);
        {
            let mut sources = inner.sources.lock().unwrap();
            sources.get_mut(&main_source).unwrap().bitrate.estimate_bps = Some(8_000_000);
            sources.get_mut(&sub_source).unwrap().bitrate.estimate_bps = Some(500_000);
        }

        webrtc.live.publish(
            main_source,
            VideoCodec::H264,
            true,
            Instant::now(),
            None,
            Bytes::from_static(&[1, 2, 3, 4]),
        );
        webrtc.live.publish(
            sub_source,
            VideoCodec::H264,
            true,
            Instant::now(),
            None,
            Bytes::from_static(&[1, 2, 3, 4]),
        );
        for source in [main_source, sub_source] {
            webrtc.live.publish(
                source,
                VideoCodec::H264,
                false,
                Instant::now(),
                None,
                Bytes::from_static(&[5, 6, 7, 8]),
            );
        }
        adaptive.record_written_frame();
        browser.record_written_frame();
        adaptive.record_discarded_frames(2);
        browser
            .sender
            .queue_stats
            .recovery_drops
            .store(3, Ordering::Relaxed);
        inner.queue_recovery_drops.store(3, Ordering::Relaxed);

        let health = webrtc.health_snapshot();

        assert_eq!(health.active_sessions, 3);
        assert_eq!(health.adaptive_sessions, 1);
        assert_eq!(health.multi_track_sessions, 1);
        assert_eq!(health.multi_tracks, 1);
        assert_eq!(health.fixed_sessions, 1);
        assert_eq!(health.active_main, 2);
        assert_eq!(health.active_sub, 1);
        assert_eq!(health.requested_auto, 1);
        assert_eq!(health.requested_high, 0);
        assert_eq!(health.requested_low, 1);
        assert_eq!(health.estimated_bitrate_min_bps, Some(3_000_000));
        assert_eq!(health.estimated_bitrate_avg_bps, Some(6_000_000));
        assert_eq!(health.estimated_bitrate_max_bps, Some(9_000_000));
        assert_eq!(health.source_bitrate_bps, 8_500_000);
        assert_eq!(health.published_frames, 4);
        assert_eq!(health.published_bytes, 16);
        assert_eq!(health.delivered_frames, 3);
        assert_eq!(health.written_frames, 2);
        assert_eq!(health.queued_frames, 3);
        assert_eq!(health.queue_depth_max, 1);
        assert_eq!(health.queue_high_water, 1);
        assert_eq!(health.queue_drops, 3);
        assert_eq!(health.queue_discarded_frames, 2);
        assert_eq!(health.queue_recovery_drops, 3);
        assert_eq!(health.session_queues.len(), 3);
        assert_eq!(health.sources.len(), 2);
        assert!(health.sources.iter().all(|source| source.has_keyframe));

        drop((adaptive, browser, fixed));
    }

    #[test]
    fn frame_sequence_gap_rearms_keyframe_gate() {
        let mut gate = KeyframeGate::new();
        let mut last_sequence = None;

        assert!(gate.observe_sequence(&mut last_sequence, 0));
        assert!(gate.allows(FrameOrigin::Live, true));
        gate.mark_written(FrameOrigin::Live);
        assert!(gate.allows(FrameOrigin::Live, false));

        assert!(!gate.observe_sequence(&mut last_sequence, 2));
        assert!(!gate.allows(FrameOrigin::Live, false));
        assert!(!gate.allows(FrameOrigin::Cached, true));
        assert!(gate.allows(FrameOrigin::Live, true));
    }

    #[test]
    fn cached_keyframe_waits_for_a_contiguous_live_gop() {
        let mut gate = KeyframeGate::new();
        let cached_keyframe = live_frame(true);
        let live_keyframe = live_frame(true);
        let live_p_frame = live_frame(false);

        assert!(gate.allows(FrameOrigin::Cached, cached_keyframe.is_keyframe));
        gate.mark_written(FrameOrigin::Cached);
        assert!(!gate.allows(FrameOrigin::Cached, cached_keyframe.is_keyframe));
        for _ in 0..3 {
            assert!(!gate.allows(FrameOrigin::Live, live_p_frame.is_keyframe));
        }

        assert!(gate.allows(FrameOrigin::Live, live_keyframe.is_keyframe));
        gate.mark_written(FrameOrigin::Live);
        for _ in 0..3 {
            assert!(gate.allows(FrameOrigin::Live, live_p_frame.is_keyframe));
        }
    }

    #[test]
    fn source_switch_rearms_cached_keyframe_protection() {
        let mut gate = KeyframeGate::new();

        assert!(gate.allows(FrameOrigin::Live, true));
        gate.mark_written(FrameOrigin::Live);
        assert!(gate.allows(FrameOrigin::Live, false));

        gate.reset();
        assert!(gate.allows(FrameOrigin::Cached, true));
        gate.mark_written(FrameOrigin::Cached);
        assert!(!gate.allows(FrameOrigin::Live, false));
        assert!(gate.allows(FrameOrigin::Live, true));
        gate.mark_written(FrameOrigin::Live);
        assert!(gate.allows(FrameOrigin::Live, false));
    }

    #[test]
    fn switching_source_moves_one_subscriber() {
        let inner = Arc::new(Inner::default());
        let (tx, _rx) = bounded(1);
        let poller = Arc::new(Poller::new().unwrap());
        let sender = SessionSender {
            id: SessionId(1),
            track_id: None,
            tx,
            queue_stats: Arc::new(SessionQueueStats::default()),
            queue_high_water: Arc::new(AtomicUsize::new(0)),
            latest_keyframe: Arc::new(Mutex::new(None)),
            poller: poller.clone(),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        let high_source = Source {
            camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            stream: StreamKind::Main,
        };
        let low_source = Source {
            stream: StreamKind::Sub,
            ..high_source
        };
        let control = Arc::new(SessionControl::new(
            StreamQuality::High,
            StreamKind::Main,
            poller,
        ));
        let mut subscription = SourceSubscription::adaptive(
            inner.clone(),
            sender,
            high_source,
            Some(low_source),
            control,
            None,
            "camera".to_owned(),
        );

        {
            let sources = inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(sources[&high_source].subscribers.len(), 1);
            assert_eq!(
                sources
                    .get(&low_source)
                    .map_or(0, |state| state.subscribers.len()),
                0
            );
        }

        subscription.begin_switch(low_source);
        assert_eq!(subscription.active_source, high_source);
        assert_eq!(subscription.pending_source, Some(low_source));
        {
            let sources = inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(sources[&high_source].subscribers.len(), 1);
            assert_eq!(sources[&low_source].subscribers.len(), 1);
        }
        assert!(!subscription.finish_switch_on_frame(low_source, false));
        assert_eq!(subscription.active_source, high_source);
        assert!(subscription.finish_switch_on_frame(low_source, true));
        assert_eq!(subscription.active_source, low_source);
        assert_eq!(subscription.pending_source, None);

        let sources = inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(sources[&high_source].subscribers.len(), 0);
        assert_eq!(sources[&low_source].subscribers.len(), 1);
        drop(sources);
        drop(subscription);
        let sources = inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(sources[&high_source].subscribers.len(), 0);
        assert_eq!(sources[&low_source].subscribers.len(), 0);
    }

    #[test]
    fn automatic_quality_uses_hysteresis() {
        let inner = Arc::new(Inner::default());
        let (tx, _rx) = bounded(1);
        let poller = Arc::new(Poller::new().unwrap());
        let sender = SessionSender {
            id: SessionId(1),
            track_id: None,
            tx,
            queue_stats: Arc::new(SessionQueueStats::default()),
            queue_high_water: Arc::new(AtomicUsize::new(0)),
            latest_keyframe: Arc::new(Mutex::new(None)),
            poller: poller.clone(),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        let high_source = Source {
            camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            stream: StreamKind::Main,
        };
        let low_source = Source {
            stream: StreamKind::Sub,
            ..high_source
        };
        inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(high_source)
            .or_default()
            .bitrate
            .estimate_bps = Some(4_000_000);
        let control = Arc::new(SessionControl::new(
            StreamQuality::Auto,
            StreamKind::Sub,
            poller,
        ));
        let mut subscription = SourceSubscription::adaptive(
            inner,
            sender,
            high_source,
            Some(low_source),
            control.clone(),
            None,
            "camera".to_owned(),
        );
        let now = Instant::now();
        control
            .estimated_bitrate_bps
            .store(5_000_000, Ordering::Release);

        assert_eq!(subscription.automatic_source(now), low_source);
        assert_eq!(
            subscription.automatic_source(now + UPGRADE_HOLD),
            high_source
        );
        subscription.begin_switch(high_source);
        assert!(subscription.finish_switch_on_frame(high_source, true));
        control
            .estimated_bitrate_bps
            .store(4_000_000, Ordering::Release);

        assert_eq!(subscription.automatic_source(now), high_source);
        assert_eq!(
            subscription.automatic_source(now + DOWNGRADE_HOLD),
            low_source
        );
    }

    #[test]
    fn dtls_close_and_ice_disconnect_end_session_delivery() {
        assert!(terminal_session_event(&Event::Closed));
        assert!(terminal_session_event(&Event::IceConnectionStateChange(
            IceConnectionState::Disconnected
        )));
        assert!(!terminal_session_event(&Event::Connected));
    }

    #[test]
    fn browser_offer_binds_each_negotiated_video_mid_to_its_track() {
        use str0m::media::Direction;

        let mut offerer = rtc_config().build(Instant::now());
        let mut changes = offerer.sdp_api();
        let first_mid = changes.add_media(
            MediaKind::Video,
            Direction::RecvOnly,
            Some("browser".to_owned()),
            Some("kitchen".to_owned()),
            None,
        );
        let second_mid = changes.add_media(
            MediaKind::Video,
            Direction::RecvOnly,
            Some("browser".to_owned()),
            Some("garden".to_owned()),
            None,
        );
        let (_offer, _) = changes.apply().unwrap();

        let kitchen = TrackId::parse("kitchen".to_owned()).unwrap();
        let garden = TrackId::parse("garden".to_owned()).unwrap();
        let plans = vec![
            TrackPlan {
                track_id: kitchen.clone(),
                mid: first_mid.to_string(),
                camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                has_sub_stream: true,
                recording_label: "kitchen".to_owned(),
                quality: StreamQuality::Low,
            },
            TrackPlan {
                track_id: garden.clone(),
                mid: second_mid.to_string(),
                camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                has_sub_stream: true,
                recording_label: "garden".to_owned(),
                quality: StreamQuality::Low,
            },
        ];

        let bindings = bind_mids(&plans);
        assert_eq!(bindings.get(&kitchen), Some(&first_mid));
        assert_eq!(bindings.get(&garden), Some(&second_mid));
    }

    #[test]
    fn multi_track_drop_keeps_sibling_subscription_active() {
        let inner = Arc::new(Inner::default());
        let poller = Arc::new(Poller::new().unwrap());
        let source = test_source();
        let first_id = TrackId::parse("first".to_owned()).unwrap();
        let second_id = TrackId::parse("second".to_owned()).unwrap();
        let first = SessionSender {
            id: SessionId(1),
            track_id: Some(first_id),
            tx: bounded(1).0,
            queue_stats: Arc::new(SessionQueueStats::default()),
            queue_high_water: Arc::new(AtomicUsize::new(0)),
            latest_keyframe: Arc::new(Mutex::new(None)),
            poller: poller.clone(),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        let second = SessionSender {
            id: SessionId(1),
            track_id: Some(second_id),
            tx: bounded(1).0,
            queue_stats: Arc::new(SessionQueueStats::default()),
            queue_high_water: Arc::new(AtomicUsize::new(0)),
            latest_keyframe: Arc::new(Mutex::new(None)),
            poller,
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        let first_subscription = SourceSubscription::fixed(inner.clone(), first, source, None);
        let second_subscription = SourceSubscription::fixed(inner.clone(), second, source, None);

        assert_eq!(inner.sources.lock().unwrap()[&source].subscribers.len(), 2);
        drop(first_subscription);
        assert_eq!(inner.sources.lock().unwrap()[&source].subscribers.len(), 1);
        drop(second_subscription);
        assert_eq!(inner.sources.lock().unwrap()[&source].subscribers.len(), 0);
    }

    #[test]
    fn browser_session_accepts_multiple_camera_tracks() {
        use str0m::media::Direction;

        let mut offerer = rtc_config().build(Instant::now());
        let mut changes = offerer.sdp_api();
        let kitchen_mid = changes.add_media(
            MediaKind::Video,
            Direction::RecvOnly,
            Some("browser".to_owned()),
            Some("kitchen".to_owned()),
            None,
        );
        let garden_mid = changes.add_media(
            MediaKind::Video,
            Direction::RecvOnly,
            Some("browser".to_owned()),
            Some("garden".to_owned()),
            None,
        );
        let (offer, _) = changes.apply().unwrap();
        let kitchen = TrackId::parse("kitchen".to_owned()).unwrap();
        let garden = TrackId::parse("garden".to_owned()).unwrap();
        let plans = vec![
            TrackPlan {
                track_id: kitchen.clone(),
                mid: kitchen_mid.to_string(),
                camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                has_sub_stream: true,
                recording_label: "kitchen".to_owned(),
                quality: StreamQuality::Low,
            },
            TrackPlan {
                track_id: garden.clone(),
                mid: garden_mid.to_string(),
                camera_ip: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
                has_sub_stream: true,
                recording_label: "garden".to_owned(),
                quality: StreamQuality::Low,
            },
        ];
        let webrtc = WebRtc::new();

        let session = webrtc.accept_multi_track_offer(plans, offer).unwrap();
        let status = webrtc.multi_track_session_status(session.id).unwrap();
        assert_eq!(status.tracks.len(), 2);
        assert_eq!(status.tracks[0].track_id, garden);
        assert_eq!(status.tracks[1].track_id, kitchen);
        assert!(webrtc.close_multi_track_session(session.id));
        assert!(webrtc.multi_track_session_status(session.id).is_none());
        assert!(
            webrtc
                .live
                .inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
                .all(|source| source.subscribers.is_empty())
        );
        assert!(
            webrtc
                .live
                .inner
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        webrtc.shutdown();
    }

    #[test]
    fn shutdown_closes_active_browser_session() {
        use str0m::media::Direction;

        let mut offerer = rtc_config().build(Instant::now());
        let mut changes = offerer.sdp_api();
        let mid = changes.add_media(
            MediaKind::Video,
            Direction::RecvOnly,
            Some("browser".to_owned()),
            Some("camera".to_owned()),
            None,
        );
        let (offer, _) = changes.apply().unwrap();
        let track_id = TrackId::parse("camera".to_owned()).unwrap();
        let plans = vec![TrackPlan {
            track_id,
            mid: mid.to_string(),
            camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            has_sub_stream: true,
            recording_label: "camera".to_owned(),
            quality: StreamQuality::Low,
        }];
        let webrtc = WebRtc::new();

        let session = webrtc.accept_multi_track_offer(plans, offer).unwrap();
        assert!(webrtc.multi_track_session_status(session.id).is_some());
        assert_eq!(
            webrtc
                .live
                .inner
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );

        let shutdown_started = Instant::now();
        webrtc.shutdown();
        assert!(
            shutdown_started.elapsed() < Duration::from_secs(2),
            "active WebRTC shutdown took {:?}",
            shutdown_started.elapsed()
        );

        assert!(webrtc.multi_track_session_status(session.id).is_none());
        assert!(
            webrtc
                .live
                .inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
                .all(|source| source.subscribers.is_empty())
        );
        assert!(
            webrtc
                .live
                .inner
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }

    #[test]
    fn homekit_offer_accepts_controller_answer() {
        let webrtc = WebRtc::new();
        let session_id = HapSessionId::new([0x42; 16]);
        let offer = webrtc
            .create_homekit_offer(session_id, test_source())
            .unwrap();
        let controller_socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        controller_socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let controller_address = SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            controller_socket.local_addr().unwrap().port(),
        ));
        let controller_candidate = Candidate::host(controller_address, "udp").unwrap();
        let controller_rtc = rtc_config().build(Instant::now());
        let (mut controller, answer) = Str0mSession::accept_video_offer(
            controller_rtc,
            vec![controller_candidate],
            &offer.sdp,
        )
        .unwrap();

        webrtc
            .apply_homekit_answer(session_id, answer, Vec::new())
            .unwrap();
        assert_eq!(
            webrtc.homekit_transport_state(session_id),
            Some(HomeKitTransportState::Connecting)
        );

        let test_deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < test_deadline {
            let rtc_deadline = loop {
                match controller.poll_output().unwrap() {
                    Output::Timeout(deadline) => {
                        break deadline;
                    }
                    Output::Transmit(transmit) => {
                        controller_socket
                            .send_to(&transmit.contents, transmit.destination)
                            .unwrap();
                    }
                    Output::Event(_) => {}
                }
            };

            if controller.is_connected()
                && webrtc.homekit_transport_state(session_id)
                    == Some(HomeKitTransportState::Connected)
            {
                break;
            }

            let mut buffer = [0_u8; UDP_PACKET_CAPACITY];
            match controller_socket.recv_from(&mut buffer) {
                Ok((length, source)) => {
                    controller
                        .handle_input(Input::Receive(
                            Instant::now(),
                            Receive {
                                proto: Protocol::Udp,
                                source,
                                destination: controller_address,
                                contents: (&buffer[..length]).try_into().unwrap(),
                            },
                        ))
                        .unwrap();
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    let now = Instant::now();
                    if rtc_deadline <= now {
                        controller.handle_input(Input::Timeout(now)).unwrap();
                    }
                }
                Err(error) => panic!("controller UDP receive failed: {error}"),
            }
        }

        assert!(controller.is_connected());
        assert_eq!(
            webrtc.homekit_transport_state(session_id),
            Some(HomeKitTransportState::Connected)
        );
        assert!(webrtc.close_homekit_session(session_id));
        webrtc.shutdown();
    }

    #[test]
    fn homekit_allocates_six_concurrent_transport_sessions() {
        let webrtc = WebRtc::new();
        let session_ids = (1..=6)
            .map(|value| HapSessionId::new([value; 16]))
            .collect::<Vec<_>>();

        for session_id in &session_ids {
            webrtc
                .create_homekit_offer(*session_id, test_source())
                .unwrap();
        }
        assert!(session_ids.iter().all(|session_id| {
            webrtc.homekit_transport_state(*session_id)
                == Some(HomeKitTransportState::AwaitingAnswer)
        }));

        for session_id in session_ids {
            assert!(webrtc.close_homekit_session(session_id));
        }
        webrtc.shutdown();
    }

    #[test]
    fn browser_quality_promotion_demotes_sibling_tracks() {
        let poller = Arc::new(Poller::new().unwrap());
        let kitchen = TrackId::parse("kitchen".to_owned()).unwrap();
        let garden = TrackId::parse("garden".to_owned()).unwrap();
        let kitchen_control = Arc::new(SessionControl::new(
            StreamQuality::Low,
            StreamKind::Sub,
            poller.clone(),
        ));
        let garden_control = Arc::new(SessionControl::new(
            StreamQuality::Auto,
            StreamKind::Sub,
            poller.clone(),
        ));
        let control = MultiTrackControl {
            tracks: BTreeMap::from([
                (kitchen.clone(), kitchen_control),
                (garden, garden_control.clone()),
            ]),
            estimated_bitrate_bps: AtomicU64::new(0),
            poller,
            shutdown: Arc::new(AtomicBool::new(false)),
            completion: SessionCompletion::default(),
        };

        let status = control.set_quality(&kitchen, StreamQuality::High).unwrap();
        assert_eq!(status.requested_quality, StreamQuality::High);
        assert_eq!(
            StreamQuality::from_u8(garden_control.requested_quality.load(Ordering::Acquire)),
            StreamQuality::Low
        );
    }
}
