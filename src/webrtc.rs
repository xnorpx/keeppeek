//! Provides thread-based WebRTC delivery for encoded camera frames.

use crate::{
    api::proto::{
        ControlEnvelope, Error as ControlError, ErrorCode, Response as ControlResponse,
        control_envelope, response as control_response,
    },
    keeppeek::StreamKind,
    media_time::duration_to_ticks,
    storage::{RecordingDemand, RecordingDemandGuard, VideoCodec, nal},
};
use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use polling::{Event as PollEvent, Events, Poller};
use prost::Message;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{
        Arc, Condvar, Mutex, OnceLock, RwLock, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use str0m::{
    Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig,
    bwe::{Bitrate, BweKind},
    change::{SdpAnswer, SdpOffer},
    channel::{ChannelConfig, ChannelId, Reliability},
    crypto::dtls::DtlsVersion,
    format::Codec,
    media::{MediaKind, MediaTime, Mid},
    net::{Protocol, Receive},
};

mod session_registry;

use session_registry::SessionRegistry;

const FRAME_QUEUE_CAPACITY: usize = 1_000;
const API_DATA_QUEUE_CAPACITY: usize = 64;
const API_BACKGROUND_OUTBOUND_MAX_BYTES: usize = 8 * 1_024 * 1_024;
const API_BACKGROUND_OUTBOUND_MAX_MESSAGES: usize = 512;
const API_OUTBOUND_MAX_BYTES: usize = 520 * 1_024 * 1_024;
const API_OUTBOUND_MAX_MESSAGES: usize = 20_000;
const API_EVENT_ATTACHMENT_CHUNK_BYTES: usize = 32 * 1_024;
const API_EVENT_ATTACHMENT_MAX_BYTES: usize = 8 * 1_024 * 1_024;
const API_PENDING_EVENT_MAX_BYTES: usize =
    API_EVENT_ATTACHMENT_MAX_BYTES + MAX_CONTROL_MESSAGE_BYTES;
const API_CONTROL_NOTIFICATION_MAX_BYTES: usize = 2 * 1_024 * 1_024;
const API_CONTROL_NOTIFICATION_MAX_MESSAGES: usize = 512;
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
/// Limits client-generated track identifiers kept in server session state.
const MAX_TRACK_ID_BYTES: usize = 64;
const CONTROL_CHANNEL_LABEL: &str = "control-channel";
const RELIABLE_DATA_CHANNEL_LABEL: &str = "reliable-data";
const UNRELIABLE_DATA_CHANNEL_LABEL: &str = "unreliable-data";
const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1_024;
const MAX_DATA_MESSAGE_BYTES: usize = 64 * 1_024;
const API_MEDIA_FRAME_CHUNK_BYTES: usize = 48 * 1_024;

#[derive(Debug, Clone, Copy)]
struct SessionChannels {
    control: ChannelId,
    reliable_data: ChannelId,
    unreliable_data: ChannelId,
}

fn session_channel_configs() -> [ChannelConfig; 3] {
    [
        ChannelConfig {
            label: CONTROL_CHANNEL_LABEL.to_owned(),
            negotiated: Some(0),
            ..ChannelConfig::default()
        },
        ChannelConfig {
            label: RELIABLE_DATA_CHANNEL_LABEL.to_owned(),
            negotiated: Some(1),
            ..ChannelConfig::default()
        },
        ChannelConfig {
            label: UNRELIABLE_DATA_CHANNEL_LABEL.to_owned(),
            ordered: false,
            reliability: Reliability::MaxRetransmits { retransmits: 0 },
            negotiated: Some(2),
            ..ChannelConfig::default()
        },
    ]
}

fn configure_session_channels(rtc: &mut Rtc) -> SessionChannels {
    let mut direct = rtc.direct_api();
    let [control_config, reliable_config, unreliable_config] = session_channel_configs();
    let control = direct.create_data_channel(control_config);
    let reliable_data = direct.create_data_channel(reliable_config);
    let unreliable_data = direct.create_data_channel(unreliable_config);
    SessionChannels {
        control,
        reliable_data,
        unreliable_data,
    }
}

#[cfg(test)]
pub(crate) fn test_api_offer() -> SdpOffer {
    let mut offerer = rtc_config().build(Instant::now());
    let mut changes = offerer.sdp_api();
    for config in session_channel_configs() {
        changes.add_channel_with_config(config);
    }
    changes
        .apply()
        .expect("documented API channel offer must be valid")
        .0
}

pub(crate) trait ControlRequestHandler: Send + Sync {
    fn handle(&self, request: crate::api::proto::Request) -> ControlDispatch;

    fn authorize_session_command(
        &self,
        _session_id: SessionId,
        _request: &crate::api::proto::Request,
    ) -> Result<(), ControlHandlerError> {
        Ok(())
    }

    fn handle_for_session(
        &self,
        _session_id: SessionId,
        request: crate::api::proto::Request,
    ) -> ControlDispatch {
        self.handle(request)
    }

    fn handle_data_for_session(
        &self,
        _session_id: SessionId,
        _channel: crate::api::proto::DataChannelKind,
        _message: crate::api::proto::Message,
    ) -> Result<(), ControlHandlerError> {
        Err(ControlHandlerError::new(
            ErrorCode::UnsupportedRequest,
            "data messages are not implemented by this server",
        ))
    }

    fn unsubscribe_for_session(&self, _session_id: SessionId, _subscription_ids: &[String]) {}

    fn has_event_subscription(&self, _session_id: SessionId, _subscription_id: &str) -> bool {
        false
    }

    fn source_reset(&self, _camera_ip: IpAddr) {}

    fn session_closed(&self, _session_id: SessionId) {}

    fn resolve_media_subscription(
        &self,
        _request: &crate::api::proto::SubscribeMedia,
    ) -> Result<MediaSubscriptionPlan, ControlHandlerError> {
        Err(ControlHandlerError::new(
            ErrorCode::UnsupportedRequest,
            "media subscriptions are not implemented by this server",
        ))
    }

    fn initial_capabilities(
        &self,
        _session_id: SessionId,
    ) -> Option<crate::api::proto::ServerCapabilities> {
        None
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MediaSubscriptionPlan {
    pub(crate) source_session_id: String,
    pub(crate) camera_ip: IpAddr,
    pub(crate) has_sub_stream: bool,
    pub(crate) recording_label: String,
    pub(crate) quality: StreamQuality,
    pub(crate) delivery_transport: crate::api::proto::DeliveryTransport,
    pub(crate) codec: crate::api::proto::CodecDescriptor,
    pub(crate) format: crate::api::proto::MediaDataFormat,
    pub(crate) selected_variant_id: String,
}

#[derive(Debug)]
pub(crate) struct ControlHandlerError {
    pub(crate) code: ErrorCode,
    pub(crate) message: String,
}

impl ControlHandlerError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub(crate) type PostSendAction = Box<dyn FnOnce() + Send>;

pub(crate) struct ControlDispatch {
    pub(crate) response: ControlResponse,
    pub(crate) after_send: Option<PostSendAction>,
    pub(crate) data_messages: Vec<OutboundDataMessage>,
    pub(crate) notifications: Vec<crate::api::proto::Notification>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataChannelTarget {
    Reliable,
    Unreliable,
}

#[derive(Debug)]
pub(crate) struct OutboundDataMessage {
    pub(crate) target: DataChannelTarget,
    pub(crate) group: String,
    pub(crate) message: crate::api::proto::Message,
}

pub(crate) struct OutboundEventDelivery {
    pub(crate) event: crate::api::proto::Event,
    pub(crate) attachment_target: Option<DataChannelTarget>,
    pub(crate) attachment_bytes: Option<Arc<[u8]>>,
}

struct EnvelopeDispatch {
    envelope: ControlEnvelope,
    after_send: Option<PostSendAction>,
    data_messages: Vec<OutboundDataMessage>,
    notifications: Vec<crate::api::proto::Notification>,
}

struct ControlDecodeError {
    request_id: u64,
    message: &'static str,
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

    pub(crate) const fn as_u64(self) -> u64 {
        self.0
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

#[derive(Debug, Clone)]
pub(crate) struct TrackPlan {
    pub(crate) track_id: TrackId,
    pub(crate) camera_ip: IpAddr,
    pub(crate) has_sub_stream: bool,
    pub(crate) selected_stream: StreamKind,
    pub(crate) recording_label: String,
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

enum ApiSessionCommand {
    Data {
        message: Box<OutboundDataMessage>,
        cancelled: Arc<AtomicBool>,
    },
    Complete {
        group: String,
        completion: ApiDataCompletion,
    },
    Notification(Box<crate::api::proto::Notification>),
    Event {
        delivery: Box<OutboundEventDelivery>,
        reservation: PendingEventReservation,
    },
}

struct PendingEventReservation {
    bytes: usize,
    pending_bytes: Arc<AtomicUsize>,
}

impl Drop for PendingEventReservation {
    fn drop(&mut self) {
        self.pending_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_sub(self.bytes)
            })
            .expect("pending event byte reservation must balance exactly");
    }
}

struct ApiDataCompletion(Option<PostSendAction>);

impl ApiDataCompletion {
    const fn new(action: PostSendAction) -> Self {
        Self(Some(action))
    }

    fn finish(mut self) {
        if let Some(action) = self.0.take() {
            action();
        }
    }
}

impl Drop for ApiDataCompletion {
    fn drop(&mut self) {
        if let Some(action) = self.0.take() {
            action();
        }
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

    const fn has_live_gop(&self) -> bool {
        !self.waiting
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
}

impl SessionControl {
    const fn new(quality: StreamQuality, active_stream: StreamKind) -> Self {
        Self {
            requested_quality: AtomicU8::new(quality.as_u8()),
            active_stream: AtomicU8::new(stream_as_u8(active_stream)),
            estimated_bitrate_bps: AtomicU64::new(0),
        }
    }
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

struct ApiSessionControl {
    session_id: SessionId,
    inner: Arc<Inner>,
    recording_demand: Option<RecordingDemand>,
    poller: Arc<Poller>,
    shutdown: Arc<AtomicBool>,
    completion: SessionCompletion,
    control_handler: Arc<RwLock<Option<Weak<dyn ControlRequestHandler>>>>,
    data_tx: Sender<ApiSessionCommand>,
    pending_event_bytes: Arc<AtomicUsize>,
    media_camera_ips: Mutex<HashSet<IpAddr>>,
    background_operation_in_flight: Arc<AtomicBool>,
}

struct ApiControlRuntime {
    dispatch_tx: Sender<ControlDispatch>,
    dispatch_rx: Receiver<ControlDispatch>,
    outbound: VecDeque<QueuedControlOutput>,
    outbound_bytes: usize,
    outbound_messages: usize,
    after_send: Vec<PostSendAction>,
}

enum QueuedControlOutput {
    Payload(Vec<u8>),
    Data {
        messages: Vec<OutboundDataMessage>,
        byte_len: usize,
    },
    Action(PostSendAction),
}

impl ApiControlRuntime {
    fn enqueue(&mut self, dispatch: EnvelopeDispatch) -> anyhow::Result<()> {
        let EnvelopeDispatch {
            envelope,
            after_send,
            data_messages,
            notifications,
        } = dispatch;
        let response_payload = envelope.encode_to_vec();
        let notification_payloads = notifications
            .into_iter()
            .map(|notification| {
                ControlEnvelope {
                    message: Some(control_envelope::Message::Notification(notification)),
                }
                .encode_to_vec()
            })
            .collect::<Vec<_>>();
        let data_bytes = data_messages.iter().try_fold(0usize, |total, message| {
            total
                .checked_add(message.message.encoded_len())
                .ok_or_else(|| anyhow::anyhow!("API control queue byte count overflowed"))
        })?;
        let added_bytes = notification_payloads
            .iter()
            .try_fold(response_payload.len(), |total, payload| {
                total
                    .checked_add(payload.len())
                    .ok_or_else(|| anyhow::anyhow!("API control queue byte count overflowed"))
            })?
            .checked_add(data_bytes)
            .ok_or_else(|| anyhow::anyhow!("API control queue byte count overflowed"))?;
        let added_messages = 1usize
            .checked_add(notification_payloads.len())
            .and_then(|total| total.checked_add(data_messages.len()))
            .ok_or_else(|| anyhow::anyhow!("API control queue message count overflowed"))?;
        let total_bytes = self
            .outbound_bytes
            .checked_add(added_bytes)
            .ok_or_else(|| anyhow::anyhow!("API control queue byte count overflowed"))?;
        let total_messages = self
            .outbound_messages
            .checked_add(added_messages)
            .ok_or_else(|| anyhow::anyhow!("API control queue message count overflowed"))?;
        if total_bytes > API_OUTBOUND_MAX_BYTES || total_messages > API_OUTBOUND_MAX_MESSAGES {
            anyhow::bail!("API control queue limit exceeded");
        }

        self.outbound_bytes = total_bytes;
        self.outbound_messages = total_messages;
        self.outbound
            .push_back(QueuedControlOutput::Payload(response_payload));
        if !data_messages.is_empty() {
            self.outbound.push_back(QueuedControlOutput::Data {
                messages: data_messages,
                byte_len: data_bytes,
            });
        }
        self.outbound.extend(
            notification_payloads
                .into_iter()
                .map(QueuedControlOutput::Payload),
        );
        if let Some(action) = after_send {
            self.outbound.push_back(QueuedControlOutput::Action(action));
        }
        Ok(())
    }
}

struct ApiMediaTrack {
    source_session_id: String,
    runtime: TrackRuntime,
}

enum TrackDelivery {
    Rtp(Mid),
    ReliableData(Box<DataMediaBinding>),
}

struct DataMediaBinding {
    stream_binding_id: String,
    codec: crate::api::proto::CodecDescriptor,
    format: crate::api::proto::MediaDataFormat,
    configuration_revision: u64,
    next_frame_id: u64,
    source_clock: Option<SourceDataClock>,
    pending_configuration: Option<crate::api::proto::MediaDataConfiguration>,
}

struct SourceDataClock {
    source_timestamp: Duration,
    system_time: SystemTime,
}

impl TrackDelivery {
    const fn mid(&self) -> Option<Mid> {
        match self {
            Self::Rtp(mid) => Some(*mid),
            Self::ReliableData(_) => None,
        }
    }

    const fn reset_source(&mut self) {
        if let Self::ReliableData(binding) = self {
            binding.source_clock = None;
        }
    }

    fn take_configuration_notification(&mut self) -> Option<crate::api::proto::Notification> {
        let Self::ReliableData(binding) = self else {
            return None;
        };
        binding
            .pending_configuration
            .take()
            .map(|configuration| crate::api::proto::Notification {
                event: Some(
                    crate::api::proto::notification::Event::MediaDataConfiguration(configuration),
                ),
            })
    }

    fn result(&self) -> crate::api::proto::subscription_result::Delivery {
        match self {
            Self::Rtp(mid) => crate::api::proto::subscription_result::Delivery::Rtp(
                crate::api::proto::RtpDelivery {
                    mid: mid.to_string(),
                },
            ),
            Self::ReliableData(binding) => {
                crate::api::proto::subscription_result::Delivery::MediaData(
                    crate::api::proto::MediaDataDelivery {
                        stream_binding_id: binding.stream_binding_id.clone(),
                        channel: crate::api::proto::DataChannelKind::ReliableData as i32,
                        codec: Some(binding.codec.clone()),
                        format: Some(binding.format.clone()),
                        configuration_revision: binding.configuration_revision,
                    },
                )
            }
        }
    }
}

#[derive(Default)]
struct ApiMediaRuntime {
    available_video_mids: Vec<Mid>,
    tracks: Vec<ApiMediaTrack>,
    outbound: VecDeque<QueuedApiData>,
    outbound_bytes: usize,
    control_notifications: VecDeque<crate::api::proto::Notification>,
    control_notification_bytes: usize,
}

enum QueuedApiData {
    Message(QueuedDataMessage),
    Complete {
        group: String,
        completion: ApiDataCompletion,
    },
}

impl QueuedApiData {
    fn group(&self) -> &str {
        match self {
            Self::Message(message) => &message.group,
            Self::Complete { group, .. } => group,
        }
    }

    const fn byte_len(&self) -> usize {
        match self {
            Self::Message(message) => message.payload.len(),
            Self::Complete { .. } => 0,
        }
    }
}

struct QueuedDataMessage {
    target: DataChannelTarget,
    group: String,
    payload: Vec<u8>,
}

fn control_notification_encoded_len(notification: &crate::api::proto::Notification) -> usize {
    ControlEnvelope {
        message: Some(control_envelope::Message::Notification(
            notification.clone(),
        )),
    }
    .encoded_len()
}

impl ApiMediaRuntime {
    fn has_subscription(&self, subscription_id: &str) -> bool {
        self.tracks
            .iter()
            .any(|track| track.runtime.track_id.0 == subscription_id)
    }

    fn enqueue(&mut self, messages: Vec<OutboundDataMessage>) -> anyhow::Result<()> {
        let mut queued = Vec::with_capacity(messages.len());
        let mut added_bytes = 0usize;
        for message in messages {
            let payload = message.message.encode_to_vec();
            added_bytes = added_bytes
                .checked_add(payload.len())
                .ok_or_else(|| anyhow::anyhow!("API data queue byte count overflowed"))?;
            queued.push(QueuedApiData::Message(QueuedDataMessage {
                target: message.target,
                group: message.group,
                payload,
            }));
        }
        let total_bytes = self
            .outbound_bytes
            .checked_add(added_bytes)
            .ok_or_else(|| anyhow::anyhow!("API data queue byte count overflowed"))?;
        let total_messages = self
            .outbound
            .len()
            .checked_add(queued.len())
            .ok_or_else(|| anyhow::anyhow!("API data queue message count overflowed"))?;
        if total_bytes > API_OUTBOUND_MAX_BYTES || total_messages > API_OUTBOUND_MAX_MESSAGES {
            anyhow::bail!("API data queue limit exceeded");
        }
        self.outbound_bytes = total_bytes;
        self.outbound.extend(queued);
        Ok(())
    }

    fn enqueue_background(&mut self, message: OutboundDataMessage) {
        let payload = message.message.encode_to_vec();
        self.outbound_bytes = self.outbound_bytes.saturating_add(payload.len());
        self.outbound
            .push_back(QueuedApiData::Message(QueuedDataMessage {
                target: message.target,
                group: message.group,
                payload,
            }));
    }

    fn can_drain_background(&self) -> bool {
        self.outbound_bytes < API_BACKGROUND_OUTBOUND_MAX_BYTES
            && self.outbound.len() < API_BACKGROUND_OUTBOUND_MAX_MESSAGES
            && self.control_notification_bytes < API_CONTROL_NOTIFICATION_MAX_BYTES
            && self.control_notifications.len() < API_CONTROL_NOTIFICATION_MAX_MESSAGES
    }

    fn cancel_group(&mut self, group: &str) {
        let mut removed_bytes = 0usize;
        self.outbound.retain(|message| {
            if message.group() == group {
                removed_bytes = removed_bytes.saturating_add(message.byte_len());
                false
            } else {
                true
            }
        });
        self.outbound_bytes = self.outbound_bytes.saturating_sub(removed_bytes);
    }

    fn cancel_event_subscription(&mut self, subscription_id: &str) {
        let group_prefix = format!("event:{subscription_id}:");
        let mut removed_bytes = 0usize;
        self.outbound.retain(|message| {
            if message.group().starts_with(&group_prefix) {
                removed_bytes = removed_bytes.saturating_add(message.byte_len());
                false
            } else {
                true
            }
        });
        self.outbound_bytes = self.outbound_bytes.saturating_sub(removed_bytes);
        let mut removed_notification_bytes = 0usize;
        self.control_notifications.retain(|notification| {
            let remove = matches!(
                notification.event.as_ref(),
                Some(crate::api::proto::notification::Event::LiveEvent(event))
                    if event.subscription_id.as_deref() == Some(subscription_id)
            );
            if remove {
                removed_notification_bytes = removed_notification_bytes
                    .saturating_add(control_notification_encoded_len(notification));
            }
            !remove
        });
        self.control_notification_bytes = self
            .control_notification_bytes
            .saturating_sub(removed_notification_bytes);
    }

    fn enqueue_event(&mut self, delivery: OutboundEventDelivery) -> anyhow::Result<()> {
        let OutboundEventDelivery {
            event,
            attachment_target,
            attachment_bytes,
        } = delivery;
        let subscription_id = event
            .subscription_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("live event delivery has no subscription ID"))?;
        let group = format!(
            "event:{subscription_id}:{}:{}",
            event.event_id, event.revision
        );
        let attachment = match (attachment_target, attachment_bytes) {
            (Some(target), Some(bytes)) => Some((target, bytes)),
            (None, None) => None,
            _ => anyhow::bail!("live event attachment route and bytes are inconsistent"),
        };
        let data_messages = if let Some((target, bytes)) = attachment {
            if bytes.is_empty() || bytes.len() > API_EVENT_ATTACHMENT_MAX_BYTES {
                anyhow::bail!("live event attachment exceeds delivery bounds");
            }
            let descriptor = event
                .canonical_attachment_id
                .as_deref()
                .and_then(|id| {
                    event
                        .attachments
                        .iter()
                        .find(|attachment| attachment.attachment_id == id)
                })
                .ok_or_else(|| anyhow::anyhow!("live event canonical attachment is missing"))?;
            if descriptor.byte_len != Some(bytes.len() as u64) {
                anyhow::bail!("live event attachment bytes do not match the descriptor");
            }
            let chunk_count = u32::try_from(bytes.len().div_ceil(API_EVENT_ATTACHMENT_CHUNK_BYTES))
                .map_err(|_| anyhow::anyhow!("live event attachment has too many chunks"))?;
            bytes
                .chunks(API_EVENT_ATTACHMENT_CHUNK_BYTES)
                .enumerate()
                .map(|(chunk_index, payload)| OutboundDataMessage {
                    target,
                    group: group.clone(),
                    message: crate::api::proto::Message {
                        message: Some(crate::api::proto::message::Message::Event(
                            crate::api::proto::EventMessage {
                                message: Some(
                                    crate::api::proto::event_message::Message::Attachment(
                                        crate::api::proto::EventAttachmentChunk {
                                            context: Some(
                                                crate::api::proto::event_attachment_chunk::Context::SubscriptionId(
                                                    subscription_id.clone(),
                                                ),
                                            ),
                                            event_id: event.event_id.clone(),
                                            revision: event.revision,
                                            attachment_id: descriptor.attachment_id.clone(),
                                            attachment_type: descriptor.attachment_type.clone(),
                                            content_type: descriptor.content_type.clone(),
                                            ordinal: descriptor.ordinal,
                                            timestamp: descriptor.timestamp,
                                            sequence: 1,
                                            chunk_index: u32::try_from(chunk_index)
                                                .unwrap_or(u32::MAX),
                                            chunk_count,
                                            payload: payload.to_vec(),
                                        },
                                    ),
                                ),
                            },
                        )),
                    },
                })
                .collect()
        } else {
            Vec::new()
        };
        let notification = crate::api::proto::Notification {
            event: Some(crate::api::proto::notification::Event::LiveEvent(event)),
        };
        let notification_bytes = self.validate_control_notification(&notification)?;
        self.enqueue(data_messages)?;
        self.control_notification_bytes = self
            .control_notification_bytes
            .checked_add(notification_bytes)
            .expect("validated control notification byte sum must fit");
        self.control_notifications.push_back(notification);
        Ok(())
    }

    fn validate_control_notification(
        &self,
        notification: &crate::api::proto::Notification,
    ) -> anyhow::Result<usize> {
        let notification_bytes = control_notification_encoded_len(notification);
        let total_bytes = self
            .control_notification_bytes
            .checked_add(notification_bytes)
            .ok_or_else(|| anyhow::anyhow!("API notification queue byte count overflowed"))?;
        let total_messages = self
            .control_notifications
            .len()
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("API notification queue count overflowed"))?;
        if total_bytes > API_CONTROL_NOTIFICATION_MAX_BYTES
            || total_messages > API_CONTROL_NOTIFICATION_MAX_MESSAGES
        {
            anyhow::bail!("API notification queue limit exceeded");
        }
        Ok(notification_bytes)
    }

    fn enqueue_control_notification(
        &mut self,
        notification: crate::api::proto::Notification,
    ) -> anyhow::Result<()> {
        let notification_bytes = self.validate_control_notification(&notification)?;
        self.control_notification_bytes = self
            .control_notification_bytes
            .checked_add(notification_bytes)
            .expect("validated control notification byte sum must fit");
        self.control_notifications.push_back(notification);
        Ok(())
    }

    fn flush_outbound(&mut self, rtc: &mut Rtc, channels: SessionChannels) -> anyhow::Result<()> {
        while let Some(queued) = self.outbound.pop_front() {
            let message = match queued {
                QueuedApiData::Complete {
                    group: _,
                    completion,
                } => {
                    completion.finish();
                    continue;
                }
                QueuedApiData::Message(message) => message,
            };
            let channel_id = match message.target {
                DataChannelTarget::Reliable => channels.reliable_data,
                DataChannelTarget::Unreliable => channels.unreliable_data,
            };
            let Some(mut channel) = rtc.channel(channel_id) else {
                anyhow::bail!("WebRTC data channel disappeared before queued delivery");
            };
            if !channel.write(true, &message.payload)? {
                self.outbound.push_front(QueuedApiData::Message(message));
                break;
            }
            self.outbound_bytes = self.outbound_bytes.saturating_sub(message.payload.len());
        }
        Ok(())
    }

    fn enqueue_stream_state(&mut self, track_id: &TrackId, stream: StreamKind) {
        if let Err(error) = self.enqueue_control_notification(crate::api::proto::Notification {
            event: Some(
                crate::api::proto::notification::Event::SubscriptionStreamState(
                    crate::api::proto::SubscriptionStreamState {
                        subscription_id: track_id.0.clone(),
                        active_variant_id: stream.to_string(),
                    },
                ),
            ),
        }) {
            tracing::warn!(%error, "dropping subscription stream-state notification");
        }
    }

    fn flush_control_notifications(
        &mut self,
        mut write: impl FnMut(&[u8]) -> anyhow::Result<bool>,
    ) -> anyhow::Result<()> {
        while let Some(notification) = self.control_notifications.pop_front() {
            let payload = ControlEnvelope {
                message: Some(control_envelope::Message::Notification(
                    notification.clone(),
                )),
            }
            .encode_to_vec();
            if !write(&payload)? {
                self.control_notifications.push_front(notification);
                break;
            }
            self.control_notification_bytes = self
                .control_notification_bytes
                .saturating_sub(payload.len());
        }
        Ok(())
    }

    fn subscribe(
        &mut self,
        session: &ApiSessionControl,
        request: &crate::api::proto::SubscribeMedia,
        plan: MediaSubscriptionPlan,
    ) -> Result<crate::api::proto::SubscriptionResult, ControlHandlerError> {
        let track_id = TrackId::parse(request.subscription_id.clone()).map_err(|error| {
            ControlHandlerError::new(ErrorCode::InvalidRequest, error.to_string())
        })?;
        let existing_index = self
            .tracks
            .iter()
            .position(|track| track.runtime.track_id == track_id);
        if let Some(index) = existing_index {
            if self.tracks[index].source_session_id != plan.source_session_id {
                return Err(ControlHandlerError::new(
                    ErrorCode::InvalidRequest,
                    "a media subscription replacement must keep the same source session",
                ));
            }
            let transport_matches = matches!(
                (
                    &self.tracks[index].runtime.delivery,
                    plan.delivery_transport
                ),
                (
                    TrackDelivery::Rtp(_),
                    crate::api::proto::DeliveryTransport::Rtp
                ) | (
                    TrackDelivery::ReliableData(_),
                    crate::api::proto::DeliveryTransport::ReliableData
                )
            );
            if !transport_matches {
                return Err(ControlHandlerError::new(
                    ErrorCode::InvalidRequest,
                    "a media subscription replacement must keep its delivery transport",
                ));
            }
            self.tracks[index]
                .runtime
                .subscription
                .set_requested_quality(plan.quality);
            session
                .media_camera_ips
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(plan.camera_ip);
            return Ok(subscription_result(
                request.subscription_id.clone(),
                &self.tracks[index].runtime.delivery,
                plan.selected_variant_id,
            ));
        }

        let delivery = match plan.delivery_transport {
            crate::api::proto::DeliveryTransport::Rtp => {
                TrackDelivery::Rtp(self.available_video_mids.pop().ok_or_else(|| {
                    ControlHandlerError::new(
                        ErrorCode::Unavailable,
                        "no negotiated video MID is available",
                    )
                })?)
            }
            crate::api::proto::DeliveryTransport::ReliableData => {
                TrackDelivery::ReliableData(Box::new(DataMediaBinding {
                    stream_binding_id: format!("media:{}", request.subscription_id),
                    codec: plan.codec.clone(),
                    format: plan.format.clone(),
                    configuration_revision: 1,
                    next_frame_id: 1,
                    source_clock: None,
                    pending_configuration: None,
                }))
            }
            _ => {
                return Err(ControlHandlerError::new(
                    ErrorCode::InvalidRequest,
                    "media subscription plan has an unsupported delivery transport",
                ));
            }
        };
        let selected_variant_id = plan.selected_variant_id.clone();
        let selected_stream = match selected_variant_id.as_str() {
            "main" => StreamKind::Main,
            "sub" => StreamKind::Sub,
            _ => {
                return Err(ControlHandlerError::new(
                    ErrorCode::Internal,
                    "media subscription plan selected an invalid variant",
                ));
            }
        };
        let initial_stream = if matches!(delivery, TrackDelivery::ReliableData(_)) {
            selected_stream
        } else {
            initial_stream(plan.has_sub_stream)
        };
        let control = Arc::new(SessionControl::new(plan.quality, initial_stream));
        let runtime = TrackRuntime::new(
            TrackPlan {
                track_id: track_id.clone(),
                camera_ip: plan.camera_ip,
                has_sub_stream: plan.has_sub_stream,
                selected_stream,
                recording_label: plan.recording_label,
            },
            delivery,
            TrackDeps {
                inner: session.inner.clone(),
                session_id: session.session_id,
                control,
                poller: session.poller.clone(),
                shutdown: session.shutdown.clone(),
                recording_demand: session.recording_demand.clone(),
            },
        );
        let track = ApiMediaTrack {
            source_session_id: plan.source_session_id,
            runtime,
        };
        self.tracks.push(track);
        session
            .media_camera_ips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(plan.camera_ip);
        self.enqueue_stream_state(&track_id, initial_stream);
        Ok(subscription_result(
            request.subscription_id.clone(),
            &self
                .tracks
                .last()
                .expect("media track was inserted before result creation")
                .runtime
                .delivery,
            selected_variant_id,
        ))
    }

    fn unsubscribe(&mut self, session: &ApiSessionControl, subscription_ids: &[String]) {
        for subscription_id in subscription_ids {
            if let Some(index) = self
                .tracks
                .iter()
                .position(|track| track.runtime.track_id.0 == *subscription_id)
            {
                let track = self.tracks.swap_remove(index);
                if let TrackDelivery::Rtp(mid) = track.runtime.delivery {
                    self.available_video_mids.push(mid);
                }
            }
        }
        let camera_ips = self
            .tracks
            .iter()
            .map(|track| track.runtime.subscription.active_source.camera_ip)
            .collect();
        *session
            .media_camera_ips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = camera_ips;
    }
}

fn subscription_result(
    subscription_id: String,
    delivery: &TrackDelivery,
    selected_variant_id: String,
) -> crate::api::proto::SubscriptionResult {
    crate::api::proto::SubscriptionResult {
        subscription_id,
        delivery: Some(delivery.result()),
        selected_variant_id,
        selected_lineage: Vec::new(),
    }
}

impl ApiSessionControl {
    fn close(&self) {
        self.shutdown.store(true, Ordering::Release);
        if let Err(error) = self.poller.notify() {
            tracing::debug!(%error, "unable to wake API WebRTC session for shutdown");
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

#[derive(Clone)]
struct CameraPreviewKeyframe {
    frame: MediaFrame,
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
    camera_generations: Mutex<HashMap<IpAddr, u64>>,
    camera_preview_keyframes: Mutex<HashMap<IpAddr, CameraPreviewKeyframe>>,
    sessions: SessionRegistry,
    control_handler: Arc<RwLock<Option<Weak<dyn ControlRequestHandler>>>>,
    published_frames: AtomicU64,
    published_bytes: AtomicU64,
    delivered_frames: AtomicU64,
    written_frames: AtomicU64,
    queue_drops: AtomicU64,
    queue_high_water: Arc<AtomicUsize>,
    queue_discarded_frames: AtomicU64,
    queue_recovery_drops: AtomicU64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveVideoSourceCapability {
    pub(crate) stream: StreamKind,
    pub(crate) codec: &'static str,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) decoder_config: Vec<u8>,
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
    fn preview_keyframe(&self) -> Option<MediaFrame> {
        if self.control.is_some() {
            return self
                .inner
                .camera_preview_keyframes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&self.high_source.camera_ip)
                .map(|preview| preview.frame.clone());
        }
        self.inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&self.active_source)
            .and_then(|state| state.keyframe.clone())
    }

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
        let active_source = low_source.unwrap_or(high_source);
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
        {
            let mut sources = self
                .inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let state = sources.entry(self.active_source).or_default();
            state.subscribers.push(self.sender.clone());
        }
        let keyframe = self.preview_keyframe();
        *self
            .sender
            .latest_keyframe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = keyframe;
    }

    fn prepare_keyframe(&self, rx: &Receiver<SessionCommand>) {
        self.discard_queued_frames(rx);
        let keyframe = self.preview_keyframe();
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

    fn set_requested_quality(&self, quality: StreamQuality) {
        if let Some(control) = &self.control {
            control
                .requested_quality
                .store(quality.as_u8(), Ordering::Release);
        }
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

    fn arm_startup_fallback(&mut self, rtc: &mut Rtc, video_mid: Option<Mid>) {
        let active_codec_negotiated = self.codec_is_negotiated(rtc, video_mid, self.active_source);
        if let Some(target) = self.startup_fallback(active_codec_negotiated) {
            self.begin_switch(target);
        }
    }

    fn startup_fallback(&self, active_codec_negotiated: bool) -> Option<Source> {
        (self.requested_quality() == Some(StreamQuality::High)
            && self.active_source != self.high_source
            && self.source_codec(self.active_source).is_some()
            && !active_codec_negotiated)
            .then_some(self.high_source)
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
    delivery: TrackDelivery,
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
    fn new(plan: TrackPlan, delivery: TrackDelivery, deps: TrackDeps) -> Self {
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
        let subscription = match &delivery {
            TrackDelivery::ReliableData(_) => {
                let source = Source {
                    camera_ip: plan.camera_ip,
                    stream: plan.selected_stream,
                };
                let demand_guard = recording_demand.as_ref().map(|demand| {
                    demand.acquire(format!("{}/{}", plan.recording_label, source.stream))
                });
                SourceSubscription::fixed(inner, sender, source, demand_guard)
            }
            TrackDelivery::Rtp(_) => SourceSubscription::adaptive(
                inner,
                sender,
                high_source,
                low_source,
                control,
                recording_demand,
                plan.recording_label,
            ),
        };
        Self {
            track_id: plan.track_id,
            delivery,
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
        self.delivery.reset_source();
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
    pub(crate) fn reset_camera(&self, camera_ip: IpAddr) {
        let mut generations = self
            .inner
            .camera_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = generations.entry(camera_ip).or_default();
        *generation = generation.saturating_add(1);
        drop(generations);
        self.inner
            .camera_preview_keyframes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&camera_ip);
        let mut sources = self
            .inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (source, state) in sources.iter_mut() {
            if source.camera_ip == camera_ip {
                state.keyframe = None;
                state.bitrate = SourceBitrate::new();
                state.next_sequence = state.next_sequence.wrapping_add(1);
                for subscriber in &state.subscribers {
                    *subscriber
                        .latest_keyframe
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                }
            }
        }
        drop(sources);
        for control in self.inner.sessions.api_controls() {
            let affected = control
                .media_camera_ips
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&camera_ip);
            if affected {
                control.close();
            }
        }
        let handler = self
            .inner
            .control_handler
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(handler) = handler {
            handler.source_reset(camera_ip);
        }
    }

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
        let frame_bytes = avcc.len();
        self.inner
            .published_bytes
            .fetch_add(frame_bytes as u64, Ordering::Relaxed);
        let frame = MediaFrame {
            codec,
            is_keyframe,
            received_at,
            timestamp,
            data: Arc::new(MediaFrameData::new(avcc)),
        };
        if is_keyframe && source.stream == StreamKind::Sub {
            let mut previews = self
                .inner
                .camera_preview_keyframes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            previews.insert(
                source.camera_ip,
                CameraPreviewKeyframe {
                    frame: frame.clone(),
                },
            );
        }
        let mut sources = self
            .inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = sources.entry(source).or_default();
        state.bitrate.observe(received_at, frame_bytes);
        if state.subscribers.is_empty() && !is_keyframe {
            return;
        }
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

fn try_reserve_bytes(counter: &AtomicUsize, bytes: usize, maximum: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(bytes) else {
            return false;
        };
        if next > maximum {
            return false;
        }
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

impl WebRtc {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_control_handler(&self, handler: Weak<dyn ControlRequestHandler>) {
        *self
            .live
            .inner
            .control_handler
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handler);
    }

    pub(crate) fn has_api_session(&self, session_id: SessionId) -> bool {
        self.live.inner.sessions.contains_api(session_id)
    }

    pub(crate) fn enqueue_api_data(
        &self,
        session_id: SessionId,
        message: OutboundDataMessage,
        cancelled: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let control = self
            .live
            .inner
            .sessions
            .api_control(session_id)
            .ok_or_else(|| anyhow::anyhow!("API WebRTC session is unavailable"))?;
        control
            .data_tx
            .send(ApiSessionCommand::Data {
                message: Box::new(message),
                cancelled,
            })
            .map_err(|_| anyhow::anyhow!("API WebRTC session data queue is closed"))?;
        control.poller.notify()?;
        Ok(())
    }

    pub(crate) fn complete_api_data_group(
        &self,
        session_id: SessionId,
        group: String,
        completion: PostSendAction,
    ) -> anyhow::Result<()> {
        let completion = ApiDataCompletion::new(completion);
        let control = self
            .live
            .inner
            .sessions
            .api_control(session_id)
            .ok_or_else(|| anyhow::anyhow!("API WebRTC session is unavailable"))?;
        control
            .data_tx
            .send(ApiSessionCommand::Complete { group, completion })
            .map_err(|_| anyhow::anyhow!("API WebRTC session data queue is closed"))?;
        control.poller.notify()?;
        Ok(())
    }

    pub(crate) fn try_enqueue_api_notification(
        &self,
        session_id: SessionId,
        notification: crate::api::proto::Notification,
    ) -> anyhow::Result<bool> {
        let control = self
            .live
            .inner
            .sessions
            .api_control(session_id)
            .ok_or_else(|| anyhow::anyhow!("API WebRTC session is unavailable"))?;
        match control
            .data_tx
            .try_send(ApiSessionCommand::Notification(Box::new(notification)))
        {
            Ok(()) => {
                if let Err(error) = control.poller.notify() {
                    control.close();
                    return Err(error.into());
                }
                Ok(true)
            }
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err(anyhow::anyhow!("API WebRTC session data queue is closed"))
            }
        }
    }

    pub(crate) fn try_enqueue_api_event(
        &self,
        session_id: SessionId,
        delivery: OutboundEventDelivery,
    ) -> anyhow::Result<bool> {
        let control = self
            .live
            .inner
            .sessions
            .api_control(session_id)
            .ok_or_else(|| anyhow::anyhow!("API WebRTC session is unavailable"))?;
        let reserved_bytes = delivery
            .event
            .encoded_len()
            .checked_add(
                delivery
                    .attachment_bytes
                    .as_ref()
                    .map_or(0, |bytes| bytes.len()),
            )
            .ok_or_else(|| anyhow::anyhow!("live event queue byte count overflowed"))?;
        if !try_reserve_bytes(
            &control.pending_event_bytes,
            reserved_bytes,
            API_PENDING_EVENT_MAX_BYTES,
        ) {
            return Ok(false);
        }
        match control.data_tx.try_send(ApiSessionCommand::Event {
            delivery: Box::new(delivery),
            reservation: PendingEventReservation {
                bytes: reserved_bytes,
                pending_bytes: control.pending_event_bytes.clone(),
            },
        }) {
            Ok(()) => {
                if let Err(error) = control.poller.notify() {
                    control.close();
                    return Err(error.into());
                }
                Ok(true)
            }
            Err(TrySendError::Full(command)) => {
                drop(command);
                Ok(false)
            }
            Err(TrySendError::Disconnected(command)) => {
                drop(command);
                Err(anyhow::anyhow!("API WebRTC session data queue is closed"))
            }
        }
    }

    pub(crate) fn accept_api_offer(&self, offer: SdpOffer) -> anyhow::Result<Session> {
        self.live.inner.sessions.reap_finished();
        let (
            SessionIo {
                rtc,
                socket,
                poller,
                answer,
            },
            channels,
        ) = accept_api_session(offer)?;
        let session_id = next_session_id(&self.live.inner);
        let shutdown = Arc::new(AtomicBool::new(false));
        let (data_tx, data_rx) = bounded(API_DATA_QUEUE_CAPACITY);
        let control = Arc::new(ApiSessionControl {
            session_id,
            inner: self.live.inner.clone(),
            recording_demand: self.recording_demand.clone(),
            poller: poller.clone(),
            shutdown: shutdown.clone(),
            completion: SessionCompletion::default(),
            control_handler: self.live.inner.control_handler.clone(),
            data_tx,
            pending_event_bytes: Arc::new(AtomicUsize::new(0)),
            media_camera_ips: Mutex::new(HashSet::new()),
            background_operation_in_flight: Arc::new(AtomicBool::new(false)),
        });
        self.live
            .inner
            .sessions
            .insert_api(session_id, control.clone());
        let thread_inner = self.live.inner.clone();
        let thread_control = control.clone();
        let thread = match std::thread::Builder::new()
            .name(format!("webrtc-api-{session_id}"))
            .spawn(move || {
                if let Err(error) = run_api_session(
                    rtc,
                    socket,
                    poller,
                    channels,
                    thread_control.clone(),
                    shutdown,
                    data_rx,
                ) {
                    tracing::debug!(%error, "API WebRTC session stopped with error");
                }
                if let Some(handler) = thread_control
                    .control_handler
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .and_then(Weak::upgrade)
                {
                    handler.session_closed(session_id);
                }
                thread_inner.sessions.remove_api(session_id);
                thread_control.finish();
            }) {
            Ok(thread) => thread,
            Err(error) => {
                control.finish();
                self.live.inner.sessions.remove_api(session_id);
                self.live.inner.sessions.release_id(session_id);
                return Err(error.into());
            }
        };
        self.live.inner.sessions.push_thread(session_id, thread);

        Ok(Session {
            id: session_id,
            answer,
        })
    }

    pub(crate) fn close_api_session(&self, session_id: SessionId) -> bool {
        self.live.inner.sessions.reap_finished();
        let control = self.live.inner.sessions.remove_api(session_id);
        let Some(control) = control else {
            return false;
        };
        control.close();
        if !control.wait_for_finish() {
            tracing::warn!(%session_id, "API WebRTC session did not finish before close timeout");
        } else {
            self.live.inner.sessions.join_thread(session_id);
        }
        self.live.inner.sessions.reap_finished();
        true
    }

    pub(crate) fn request_api_session_close(&self, session_id: SessionId) -> bool {
        self.live.inner.sessions.reap_finished();
        let control = self.live.inner.sessions.remove_api(session_id);
        let Some(control) = control else {
            return false;
        };
        control.close();
        true
    }

    pub(crate) fn active_api_session_ids(&self) -> HashSet<SessionId> {
        self.live.inner.sessions.reap_finished();
        self.live.inner.sessions.active_api_ids()
    }

    pub fn live(&self) -> Publisher {
        self.live.clone()
    }

    pub(crate) fn live_video_sources(&self, camera_ip: IpAddr) -> Vec<LiveVideoSourceCapability> {
        let sources = self
            .live
            .inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        [StreamKind::Main, StreamKind::Sub]
            .into_iter()
            .filter_map(|stream| {
                let source = sources.get(&Source { camera_ip, stream })?;
                let keyframe = source.keyframe.as_ref()?;
                let codec = match keyframe.codec {
                    VideoCodec::H264 => "h264",
                    VideoCodec::H265 => "h265",
                };
                let (width, height, decoder_config) =
                    live_video_decoder_config(keyframe).unwrap_or_else(|| (0, 0, Vec::new()));
                Some(LiveVideoSourceCapability {
                    stream,
                    codec,
                    width,
                    height,
                    decoder_config,
                })
            })
            .collect()
    }

    pub(crate) fn camera_generation(&self, camera_ip: IpAddr) -> u64 {
        self.live
            .inner
            .camera_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&camera_ip)
            .copied()
            .unwrap_or_default()
    }

    pub fn with_recording_demand(recording_demand: RecordingDemand) -> Self {
        Self {
            live: Publisher::default(),
            recording_demand: Some(recording_demand),
        }
    }

    pub fn accept_offer(&self, source: Source, offer: SdpOffer) -> anyhow::Result<SdpAnswer> {
        self.accept_offer_inner(source, None, offer)
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
        self.accept_offer_inner(source, demand_guard, offer)
            .map(|session| session.answer)
    }

    pub(crate) fn health_snapshot(&self) -> WebRtcHealth {
        self.live.inner.sessions.reap_finished();
        let api_session_ids = self.live.inner.sessions.active_api_ids();
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

        let adaptive_sessions = 0;
        let multi_track_sessions = api_session_ids.len();
        let multi_tracks = session_queues
            .iter()
            .filter(|queue| queue.track_id.is_some() && api_session_ids.contains(&queue.session_id))
            .count();
        let requested_auto = 0;
        let requested_high = 0;
        let requested_low = 0;
        let estimates: Vec<u64> = Vec::new();
        let estimated_bitrate_min_bps = estimates.iter().copied().min();
        let estimated_bitrate_max_bps = estimates.iter().copied().max();
        let estimated_bitrate_avg_bps = (!estimates.is_empty())
            .then(|| estimates.iter().copied().sum::<u64>() / estimates.len() as u64);
        let active_sessions = session_ids.union(&api_session_ids).count();

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

    fn accept_offer_inner(
        &self,
        source: Source,
        demand_guard: Option<RecordingDemandGuard>,
        offer: SdpOffer,
    ) -> anyhow::Result<Session> {
        self.live.inner.sessions.reap_finished();
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

        let subscription =
            SourceSubscription::fixed(self.live.inner.clone(), sender, source, demand_guard);

        let thread_name = format!(
            "webrtc-{}-{}",
            subscription.active_source.camera_ip, subscription.active_source.stream
        );
        let thread = match std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                if let Err(error) = run_session(rtc, socket, poller, rx, subscription, shutdown) {
                    tracing::debug!(%error, "WebRTC session stopped with error");
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                self.live.inner.sessions.release_id(session_id);
                return Err(error.into());
            }
        };
        self.live.inner.sessions.push_thread(session_id, thread);

        Ok(Session {
            id: session_id,
            answer,
        })
    }

    pub fn shutdown(&self) {
        let api_controls = self.live.inner.sessions.api_controls();
        for control in api_controls {
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

        self.live.inner.sessions.join_all_threads();
        self.live.inner.sessions.clear_api();
    }
}

fn live_video_decoder_config(frame: &MediaFrame) -> Option<(u32, u32, Vec<u8>)> {
    let parameters = match frame.codec {
        VideoCodec::H264 => {
            let (Some(sps), Some(pps)) = nal::extract_h264_sps_pps(&frame.data.avcc) else {
                return None;
            };
            retina::codec::h264::parameters_from_sps_and_pps(
                &sps,
                &pps,
                retina::codec::h26x::Framing::FourByteLength,
            )
            .ok()?
        }
        VideoCodec::H265 => {
            let (Some(vps), Some(sps), Some(pps)) = nal::extract_h265_params(&frame.data.avcc)
            else {
                return None;
            };
            retina::codec::h265::parameters_from_vps_sps_pps(
                &vps,
                &sps,
                &pps,
                retina::codec::h26x::Framing::FourByteLength,
            )
            .ok()?
        }
    };
    let (width, height) = parameters.coded_pixel_dimensions();
    Some((width, height, parameters.extra_data().to_vec()))
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
    for ip in candidate_addresses() {
        let candidate = Candidate::host(SocketAddr::new(IpAddr::V4(ip), port), "udp")?;
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

fn accept_api_session(offer: SdpOffer) -> anyhow::Result<(SessionIo, SessionChannels)> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_nonblocking(true)?;
    let port = socket.local_addr()?.port();
    let mut rtc = rtc_config().set_ice_lite(true).build(Instant::now());
    let channels = configure_session_channels(&mut rtc);
    for ip in candidate_addresses() {
        let candidate = Candidate::host(SocketAddr::new(IpAddr::V4(ip), port), "udp")?;
        let _ = rtc.add_local_candidate(candidate);
    }
    let answer = rtc.sdp_api().accept_offer(offer)?;
    let poller = Arc::new(Poller::new()?);
    // SAFETY: The session thread owns the socket and removes it before either resource drops.
    unsafe {
        poller.add(&socket, PollEvent::readable(UDP_EVENT_KEY))?;
    }
    Ok((
        SessionIo {
            rtc,
            socket,
            poller,
            answer,
        },
        channels,
    ))
}

fn next_session_id(inner: &Inner) -> SessionId {
    inner.sessions.reserve_id()
}

const fn initial_stream(has_sub_stream: bool) -> StreamKind {
    if has_sub_stream {
        StreamKind::Sub
    } else {
        StreamKind::Main
    }
}

fn rtc_config() -> RtcConfig {
    #[cfg(all(target_os = "macos", feature = "macos-test-aws-crypto"))]
    let provider = str0m_aws_lc_rs::default_provider();
    #[cfg(all(target_os = "macos", not(feature = "macos-test-aws-crypto")))]
    let provider = str0m_apple_crypto::default_provider();
    #[cfg(target_os = "linux")]
    let provider = str0m::crypto::from_feature_flags();
    #[cfg(windows)]
    let provider = str0m::crypto::from_feature_flags();

    RtcConfig::new()
        .set_crypto_provider(Arc::new(provider))
        .set_snap_enabled(true)
        .set_dtls_version(DtlsVersion::Auto)
        .clear_codecs()
        .enable_h264(true)
        .enable_h265(true)
        .enable_bwe(Some(INITIAL_EGRESS_BITRATE))
}

fn candidate_addresses() -> Vec<Ipv4Addr> {
    let mut addresses = vec![Ipv4Addr::LOCALHOST];
    if let Ok(interfaces) = NetworkInterface::show() {
        addresses.extend(interfaces.into_iter().flat_map(|interface| {
            interface.addr.into_iter().filter_map(|address| {
                let Addr::V4(address) = address else {
                    return None;
                };
                usable_ipv4_ice_address(address.ip).then_some(address.ip)
            })
        }));
    }
    addresses.sort_unstable();
    addresses.dedup();
    addresses
}

const fn usable_ipv4_ice_address(address: Ipv4Addr) -> bool {
    !address.is_link_local()
        && !address.is_broadcast()
        && !address.is_multicast()
        && !address.is_unspecified()
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

fn run_api_session(
    mut rtc: Rtc,
    socket: UdpSocket,
    poller: Arc<Poller>,
    channels: SessionChannels,
    control: Arc<ApiSessionControl>,
    shutdown: Arc<AtomicBool>,
    data_rx: Receiver<ApiSessionCommand>,
) -> anyhow::Result<()> {
    let mut media = ApiMediaRuntime::default();
    let (dispatch_tx, dispatch_rx) = bounded(1);
    let mut control_runtime = ApiControlRuntime {
        dispatch_tx,
        dispatch_rx,
        outbound: VecDeque::new(),
        outbound_bytes: 0,
        outbound_messages: 0,
        after_send: Vec::new(),
    };
    let deps = ApiSessionLoopDeps {
        channels,
        control: &control,
        shutdown: &shutdown,
        data_rx: &data_rx,
    };
    let result = drive_api_session(
        &mut rtc,
        &socket,
        &poller,
        deps,
        &mut media,
        &mut control_runtime,
    );
    control.inner.sessions.remove_api(control.session_id);
    discard_api_session_commands(&data_rx);
    for track in media.tracks {
        let queue_stats = track.runtime.subscription.sender.queue_stats.clone();
        let inner = track.runtime.subscription.inner.clone();
        let abandoned_frames = track.runtime.rx.try_iter().count() as u64;
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

fn discard_api_session_commands(data_rx: &Receiver<ApiSessionCommand>) {
    while let Ok(command) = data_rx.try_recv() {
        drop(command);
    }
}

struct ApiSessionLoopDeps<'a> {
    channels: SessionChannels,
    control: &'a ApiSessionControl,
    shutdown: &'a AtomicBool,
    data_rx: &'a Receiver<ApiSessionCommand>,
}

fn drive_api_session(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    poller: &Poller,
    deps: ApiSessionLoopDeps<'_>,
    media: &mut ApiMediaRuntime,
    control_runtime: &mut ApiControlRuntime,
) -> anyhow::Result<()> {
    let mut events = Events::new();
    let mut udp_buffer = vec![0; UDP_PACKET_CAPACITY];
    let mut peer_destinations = HashMap::new();
    let mut connected = false;
    let mut next_timeout = drain_api_outputs(
        rtc,
        socket,
        deps.channels,
        deps.control,
        media,
        &mut connected,
        control_runtime,
    )?;

    loop {
        if deps.shutdown.load(Ordering::Acquire) {
            break;
        }
        drain_api_session_commands(deps.data_rx, media);
        if drain_pending_control_dispatches(control_runtime)? {
            next_timeout = drain_api_outputs(
                rtc,
                socket,
                deps.channels,
                deps.control,
                media,
                &mut connected,
                control_runtime,
            )?;
        }
        let now = Instant::now();
        let mut wrote_media = false;
        let mut applied_streams = Vec::new();
        let mut media_data_messages = Vec::new();
        let mut control_notifications = Vec::new();
        for track in &mut media.tracks {
            let result = drive_track_runtime(rtc, &mut track.runtime, connected, now)?;
            wrote_media |= result.wrote_media;
            media_data_messages.extend(result.data_messages);
            control_notifications.extend(result.notifications);
            if let Some(stream) = result.applied_stream {
                applied_streams.push((track.runtime.track_id.clone(), stream));
            }
        }
        if !media_data_messages.is_empty() {
            media.enqueue(media_data_messages)?;
        }
        for notification in control_notifications {
            media.enqueue_control_notification(notification)?;
        }
        for (track_id, stream) in applied_streams {
            media.enqueue_stream_state(&track_id, stream);
        }
        if wrote_media {
            next_timeout = drain_api_outputs(
                rtc,
                socket,
                deps.channels,
                deps.control,
                media,
                &mut connected,
                control_runtime,
            )?;
        }
        events.clear();
        poller.wait(
            &mut events,
            Some(next_timeout.saturating_duration_since(Instant::now())),
        )?;
        if drain_pending_control_dispatches(control_runtime)? {
            next_timeout = drain_api_outputs(
                rtc,
                socket,
                deps.channels,
                deps.control,
                media,
                &mut connected,
                control_runtime,
            )?;
        }
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
                        next_timeout = drain_api_outputs(
                            rtc,
                            socket,
                            deps.channels,
                            deps.control,
                            media,
                            &mut connected,
                            control_runtime,
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
            next_timeout = drain_api_outputs(
                rtc,
                socket,
                deps.channels,
                deps.control,
                media,
                &mut connected,
                control_runtime,
            )?;
        }
    }

    Ok(())
}

fn drain_api_session_commands(data_rx: &Receiver<ApiSessionCommand>, media: &mut ApiMediaRuntime) {
    while media.can_drain_background() {
        let Ok(command) = data_rx.try_recv() else {
            break;
        };
        match command {
            ApiSessionCommand::Data { message, cancelled } => {
                if !cancelled.load(Ordering::Acquire) {
                    media.enqueue_background(*message);
                }
            }
            ApiSessionCommand::Complete { group, completion } => media
                .outbound
                .push_back(QueuedApiData::Complete { group, completion }),
            ApiSessionCommand::Notification(notification) => {
                if let Err(error) = media.enqueue_control_notification(*notification) {
                    tracing::warn!(%error, "dropping control notification at its queue limit");
                }
            }
            ApiSessionCommand::Event {
                delivery,
                reservation,
            } => {
                drop(reservation);
                if let Err(error) = media.enqueue_event(*delivery) {
                    tracing::warn!(%error, "dropping invalid live event delivery");
                }
            }
        }
    }
}

struct TrackDriveResult {
    wrote_media: bool,
    applied_stream: Option<StreamKind>,
    data_messages: Vec<OutboundDataMessage>,
    notifications: Vec<crate::api::proto::Notification>,
}

fn drive_track_runtime(
    rtc: &mut Rtc,
    track: &mut TrackRuntime,
    connected: bool,
    now: Instant,
) -> anyhow::Result<TrackDriveResult> {
    let mut wrote_media = false;
    let mut applied_stream = None;
    let mut data_messages = Vec::new();
    let mut notifications = Vec::new();
    let mid = track.delivery.mid();
    if track.keyframe_gate.has_live_gop() {
        track.subscription.select_source(rtc, mid, now);
    } else {
        track.subscription.arm_startup_fallback(rtc, mid);
    }
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
            let (wrote_frame, messages) =
                write_track_frame(rtc, &mut track.delivery, &keyframe, &mut track.media_clock)?;
            if wrote_frame {
                if let Some(notification) = track.delivery.take_configuration_notification() {
                    notifications.push(notification);
                }
                track.keyframe_gate.mark_written(FrameOrigin::Cached);
                wrote_media = true;
                data_messages.extend(messages);
            }
        }
    }

    while let Ok(SessionCommand::Frame {
        sequence,
        source,
        frame,
    }) = track.rx.try_recv()
    {
        let switched_stream = if track
            .subscription
            .finish_switch_on_frame(source, frame.is_keyframe)
        {
            track.reset_source_state();
            Some(source.stream)
        } else {
            None
        };
        if source != track.subscription.active_source {
            track.subscription.record_discarded_frames(1);
            continue;
        }
        if !track
            .keyframe_gate
            .observe_sequence(&mut track.last_frame_sequence, sequence)
        {
            track.media_clock.reset_source();
            track.delivery.reset_source();
            track.received_source_frame = false;
            track.recovering_queue_gap = true;
            tracing::debug!(track_id = %track.track_id, sequence, "API WebRTC track queue gap; waiting for keyframe");
        }
        if !track.received_source_frame {
            tracing::debug!(
                track_id = %track.track_id,
                codec = ?frame.codec,
                keyframe = frame.is_keyframe,
                "received first API WebRTC source frame"
            );
            track.received_source_frame = true;
        }
        let frame_allowed = track
            .keyframe_gate
            .allows(FrameOrigin::Live, frame.is_keyframe);
        let (wrote_frame, messages) = if connected && frame_allowed {
            write_track_frame(rtc, &mut track.delivery, &frame, &mut track.media_clock)?
        } else {
            (false, Vec::new())
        };
        if wrote_frame {
            if let Some(notification) = track.delivery.take_configuration_notification() {
                notifications.push(notification);
            }
            track.keyframe_gate.mark_written(FrameOrigin::Live);
            track.recovering_queue_gap = false;
            track.subscription.record_written_frame();
            wrote_media = true;
            data_messages.extend(messages);
            applied_stream = applied_stream.or(switched_stream);
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
    Ok(TrackDriveResult {
        wrote_media,
        applied_stream,
        data_messages,
        notifications,
    })
}

fn write_track_frame(
    rtc: &mut Rtc,
    delivery: &mut TrackDelivery,
    frame: &MediaFrame,
    media_clock: &mut MediaClock,
) -> anyhow::Result<(bool, Vec<OutboundDataMessage>)> {
    match delivery {
        TrackDelivery::Rtp(mid) => Ok((
            write_frame(rtc, Some(*mid), frame, media_clock)?,
            Vec::new(),
        )),
        TrackDelivery::ReliableData(binding) => {
            let messages = encode_media_data_frame(binding, frame)?;
            Ok((!messages.is_empty(), messages))
        }
    }
}

fn encode_media_data_frame(
    binding: &mut DataMediaBinding,
    frame: &MediaFrame,
) -> anyhow::Result<Vec<OutboundDataMessage>> {
    update_media_data_configuration(binding, frame)?;
    let codec_matches = binding.codec.name.eq_ignore_ascii_case(match frame.codec {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
    });
    if !codec_matches || frame.data.avcc.is_empty() {
        return Ok(Vec::new());
    }
    let chunk_count = u32::try_from(frame.data.avcc.len().div_ceil(API_MEDIA_FRAME_CHUNK_BYTES))
        .map_err(|_| anyhow::anyhow!("media frame has too many data-channel fragments"))?;
    let frame_id = binding.next_frame_id;
    binding.next_frame_id = binding.next_frame_id.saturating_add(1);
    let timestamp = media_data_timestamp(binding, frame);
    let messages = frame
        .data
        .avcc
        .chunks(API_MEDIA_FRAME_CHUNK_BYTES)
        .enumerate()
        .map(|(fragment_index, payload)| OutboundDataMessage {
            target: DataChannelTarget::Reliable,
            group: binding.stream_binding_id.clone(),
            message: crate::api::proto::Message {
                message: Some(crate::api::proto::message::Message::Video(
                    crate::api::proto::VideoMessage {
                        message: Some(crate::api::proto::video_message::Message::Frame(
                            crate::api::proto::VideoDataFrame {
                                stream_binding_id: binding.stream_binding_id.clone(),
                                frame_id,
                                timestamp: Some(timestamp),
                                fragment_index: u32::try_from(fragment_index).unwrap_or(u32::MAX),
                                fragment_count: chunk_count,
                                key_frame: frame.is_keyframe,
                                payload: payload.to_vec(),
                                decode_time: None,
                                configuration_revision: binding.configuration_revision,
                            },
                        )),
                    },
                )),
            },
        })
        .collect();
    Ok(messages)
}

fn update_media_data_configuration(
    binding: &mut DataMediaBinding,
    frame: &MediaFrame,
) -> anyhow::Result<()> {
    if !frame.is_keyframe {
        return Ok(());
    }
    let Some((width, height, decoder_config)) = live_video_decoder_config(frame) else {
        return Ok(());
    };
    let codec_name = match frame.codec {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
    };
    let format = crate::api::proto::MediaDataFormat {
        format: Some(crate::api::proto::media_data_format::Format::Video(
            crate::api::proto::VideoDataFormat {
                width,
                height,
                decoder_config,
            },
        )),
    };
    if binding.codec.name.eq_ignore_ascii_case(codec_name) && binding.format == format {
        return Ok(());
    }
    binding.configuration_revision = binding
        .configuration_revision
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("media data configuration revision overflowed"))?;
    binding.codec.name = codec_name.to_owned();
    binding.codec.parameters.clear();
    binding.format = format;
    binding.source_clock = None;
    binding.pending_configuration = Some(crate::api::proto::MediaDataConfiguration {
        stream_binding_id: binding.stream_binding_id.clone(),
        codec: Some(binding.codec.clone()),
        format: Some(binding.format.clone()),
        configuration_revision: binding.configuration_revision,
    });
    Ok(())
}

fn media_data_timestamp(
    binding: &mut DataMediaBinding,
    frame: &MediaFrame,
) -> prost_types::Timestamp {
    let received_at = system_time_for_instant(frame.received_at);
    let timestamp = frame.timestamp.map_or(received_at, |source_timestamp| {
        if binding
            .source_clock
            .as_ref()
            .is_none_or(|clock| source_timestamp < clock.source_timestamp)
        {
            binding.source_clock = Some(SourceDataClock {
                source_timestamp,
                system_time: received_at,
            });
        }
        let clock = binding
            .source_clock
            .as_ref()
            .expect("source clock is initialized above");
        clock
            .system_time
            .checked_add(source_timestamp.saturating_sub(clock.source_timestamp))
            .unwrap_or(received_at)
    });
    protobuf_timestamp(timestamp)
}

fn system_time_for_instant(received_at: Instant) -> SystemTime {
    let elapsed = Instant::now().saturating_duration_since(received_at);
    SystemTime::now().checked_sub(elapsed).unwrap_or(UNIX_EPOCH)
}

fn protobuf_timestamp(source_time: SystemTime) -> prost_types::Timestamp {
    let source_time = source_time.duration_since(UNIX_EPOCH).unwrap_or_default();
    prost_types::Timestamp {
        seconds: i64::try_from(source_time.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(source_time.subsec_nanos()).unwrap_or(i32::MAX),
    }
}

fn drain_api_outputs(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    channels: SessionChannels,
    control: &ApiSessionControl,
    media: &mut ApiMediaRuntime,
    connected: &mut bool,
    control_runtime: &mut ApiControlRuntime,
) -> anyhow::Result<Instant> {
    loop {
        flush_control_channel_outputs(control_runtime, media, |payload| {
            let Some(mut channel) = rtc.channel(channels.control) else {
                anyhow::bail!("WebRTC control channel disappeared before queued delivery");
            };
            Ok(channel.write(true, payload)?)
        })?;
        media.flush_outbound(rtc, channels)?;
        match rtc.poll_output()? {
            Output::Timeout(deadline) => {
                for action in std::mem::take(&mut control_runtime.after_send) {
                    action();
                }
                return Ok(deadline);
            }
            Output::Transmit(transmit) => {
                socket.send_to(&transmit.contents, transmit.destination)?;
            }
            Output::Event(event) if terminal_session_event(&event) => {
                anyhow::bail!("WebRTC transport ended")
            }
            Output::Event(Event::Connected) => {
                *connected = true;
                tracing::debug!("API WebRTC session connected");
            }
            Output::Event(Event::MediaAdded(added))
                if added.kind == MediaKind::Video && added.direction.is_sending() =>
            {
                if !media.available_video_mids.contains(&added.mid)
                    && !media
                        .tracks
                        .iter()
                        .any(|track| track.runtime.delivery.mid() == Some(added.mid))
                {
                    media.available_video_mids.push(added.mid);
                }
            }
            Output::Event(Event::ChannelOpen(channel_id, label)) => {
                let expected_label = if channel_id == channels.control {
                    CONTROL_CHANNEL_LABEL
                } else if channel_id == channels.reliable_data {
                    RELIABLE_DATA_CHANNEL_LABEL
                } else if channel_id == channels.unreliable_data {
                    UNRELIABLE_DATA_CHANNEL_LABEL
                } else {
                    anyhow::bail!("unexpected WebRTC data channel opened");
                };
                if label != expected_label {
                    anyhow::bail!(
                        "WebRTC data channel label '{label}' does not match '{expected_label}'"
                    );
                }
                if channel_id == channels.control {
                    let handler = control
                        .control_handler
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        .and_then(Weak::upgrade);
                    if let Some(capabilities) = handler
                        .as_deref()
                        .and_then(|handler| handler.initial_capabilities(control.session_id))
                    {
                        control_runtime.enqueue(EnvelopeDispatch {
                            envelope: ControlEnvelope {
                                message: Some(control_envelope::Message::Notification(
                                    crate::api::proto::Notification {
                                        event: Some(
                                            crate::api::proto::notification::Event::InitialCapabilities(
                                                capabilities,
                                            ),
                                        ),
                                    },
                                )),
                            },
                            after_send: None,
                            data_messages: Vec::new(),
                            notifications: Vec::new(),
                        })?;
                    }
                }
            }
            Output::Event(Event::ChannelData(data)) if data.id == channels.control => {
                let handler = control
                    .control_handler
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .and_then(Weak::upgrade);
                if let Ok(request) = decode_control_request(data.binary, &data.data)
                    && is_background_control_request(&request)
                    && let Some(handler) = handler.clone()
                {
                    let request_id = request.request_id;
                    if control
                        .background_operation_in_flight
                        .swap(true, Ordering::AcqRel)
                    {
                        enqueue_control_dispatch(
                            envelope_dispatch(failed_control_dispatch(
                                request_id,
                                ErrorCode::Rejected,
                                "another background control operation is already in progress",
                            )),
                            control_runtime,
                        )?;
                    } else if let Err(error) = spawn_background_control_operation(
                        request,
                        handler,
                        control,
                        control_runtime.dispatch_tx.clone(),
                    ) {
                        control
                            .background_operation_in_flight
                            .store(false, Ordering::Release);
                        enqueue_control_dispatch(
                            envelope_dispatch(failed_control_dispatch(
                                request_id,
                                ErrorCode::Unavailable,
                                format!("unable to start background control operation: {error}"),
                            )),
                            control_runtime,
                        )?;
                    }
                    continue;
                }
                let reply =
                    api_control_reply(data.binary, &data.data, handler.as_deref(), control, media);
                enqueue_control_dispatch(reply, control_runtime)?;
            }
            Output::Event(Event::ChannelData(data))
                if data.id == channels.reliable_data || data.id == channels.unreliable_data =>
            {
                let handler = control
                    .control_handler
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .and_then(Weak::upgrade);
                let channel = if data.id == channels.reliable_data {
                    crate::api::proto::DataChannelKind::ReliableData
                } else {
                    crate::api::proto::DataChannelKind::UnreliableData
                };
                if let Err(error) = api_data_message(
                    data.binary,
                    &data.data,
                    handler.as_deref(),
                    control.session_id,
                    channel,
                ) {
                    tracing::debug!(
                        session_id = %control.session_id,
                        ?channel,
                        message = %error.message,
                        "rejected API data message"
                    );
                }
            }
            Output::Event(Event::ChannelClose(channel_id)) if channel_id == channels.control => {
                anyhow::bail!("WebRTC control channel closed")
            }
            Output::Event(_) => {}
        }
    }
}

fn drain_pending_control_dispatches(
    control_runtime: &mut ApiControlRuntime,
) -> anyhow::Result<bool> {
    let mut dispatched = false;
    while let Ok(dispatch) = control_runtime.dispatch_rx.try_recv() {
        enqueue_control_dispatch(envelope_dispatch(dispatch), control_runtime)?;
        dispatched = true;
    }
    Ok(dispatched)
}

fn enqueue_control_dispatch(
    dispatch: EnvelopeDispatch,
    control_runtime: &mut ApiControlRuntime,
) -> anyhow::Result<()> {
    control_runtime.enqueue(dispatch)
}

fn flush_control_outputs(
    control_runtime: &mut ApiControlRuntime,
    media: &mut ApiMediaRuntime,
    mut write: impl FnMut(&[u8]) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    while let Some(output) = control_runtime.outbound.pop_front() {
        match output {
            QueuedControlOutput::Payload(payload) => {
                if !write(&payload)? {
                    control_runtime
                        .outbound
                        .push_front(QueuedControlOutput::Payload(payload));
                    break;
                }
                control_runtime.outbound_bytes =
                    control_runtime.outbound_bytes.saturating_sub(payload.len());
                control_runtime.outbound_messages =
                    control_runtime.outbound_messages.saturating_sub(1);
            }
            QueuedControlOutput::Data { messages, byte_len } => {
                let message_count = messages.len();
                media.enqueue(messages)?;
                control_runtime.outbound_bytes =
                    control_runtime.outbound_bytes.saturating_sub(byte_len);
                control_runtime.outbound_messages = control_runtime
                    .outbound_messages
                    .saturating_sub(message_count);
            }
            QueuedControlOutput::Action(action) => control_runtime.after_send.push(action),
        }
    }
    Ok(())
}

fn flush_control_channel_outputs(
    control_runtime: &mut ApiControlRuntime,
    media: &mut ApiMediaRuntime,
    mut write: impl FnMut(&[u8]) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    flush_control_outputs(control_runtime, media, &mut write)?;
    if control_runtime.outbound.is_empty() {
        media.flush_control_notifications(write)?;
    }
    Ok(())
}

const fn is_background_control_request(request: &crate::api::proto::Request) -> bool {
    match request.command.as_ref() {
        Some(crate::api::proto::request::Command::CameraConfigurationCommand(command)) => {
            matches!(
                command.action.as_ref(),
                Some(
                    crate::api::proto::camera_configuration_command::Action::Discover(_)
                        | crate::api::proto::camera_configuration_command::Action::ProbeStreams(_)
                )
            )
        }
        Some(crate::api::proto::request::Command::EventPublicationCommand(command)) => matches!(
            command.action.as_ref(),
            Some(crate::api::proto::event_publication_command::Action::Commit(_))
        ),
        _ => false,
    }
}

fn spawn_background_control_operation(
    request: crate::api::proto::Request,
    handler: Arc<dyn ControlRequestHandler>,
    control: &ApiSessionControl,
    dispatch_tx: Sender<ControlDispatch>,
) -> std::io::Result<()> {
    let session_id = control.session_id;
    let poller = control.poller.clone();
    let operation_in_flight = control.background_operation_in_flight.clone();
    std::thread::Builder::new()
        .name(format!("webrtc-control-operation-{session_id}"))
        .spawn(move || {
            let dispatch = handler.handle_for_session(session_id, request);
            operation_in_flight.store(false, Ordering::Release);
            if dispatch_tx.try_send(dispatch).is_ok() {
                let _ = poller.notify();
            }
        })
        .map(|_| ())
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
        if keyframe_gate.has_live_gop() {
            subscription.select_source(rtc, video_mid, Instant::now());
        }
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
                        media_clock.reset_source();
                        received_source_frame = false;
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

#[cfg(test)]
fn control_reply(
    binary: bool,
    payload: &[u8],
    handler: Option<&dyn ControlRequestHandler>,
) -> EnvelopeDispatch {
    let request = match decode_control_request(binary, payload) {
        Ok(request) => request,
        Err(error) => {
            return control_error(error.request_id, ErrorCode::InvalidRequest, error.message);
        }
    };
    let dispatch = if let Some(handler) = handler {
        handler.handle(request)
    } else {
        unavailable_control_dispatch(request.request_id)
    };
    envelope_dispatch(dispatch)
}

fn api_control_reply(
    binary: bool,
    payload: &[u8],
    handler: Option<&dyn ControlRequestHandler>,
    control: &ApiSessionControl,
    media: &mut ApiMediaRuntime,
) -> EnvelopeDispatch {
    let request = match decode_control_request(binary, payload) {
        Ok(request) => request,
        Err(error) => {
            return control_error(error.request_id, ErrorCode::InvalidRequest, error.message);
        }
    };
    let request_id = request.request_id;
    let Some(handler) = handler else {
        return envelope_dispatch(unavailable_control_dispatch(request_id));
    };
    if let Err(error) = handler.authorize_session_command(control.session_id, &request) {
        return envelope_dispatch(failed_control_dispatch(
            request_id,
            error.code,
            error.message,
        ));
    }
    let dispatch = match request.command.as_ref() {
        Some(crate::api::proto::request::Command::SubscribeMedia(subscribe)) => {
            if handler.has_event_subscription(control.session_id, &subscribe.subscription_id) {
                return envelope_dispatch(failed_control_dispatch(
                    request_id,
                    ErrorCode::InvalidRequest,
                    "subscription replacement must keep its target kind",
                ));
            }
            match handler
                .resolve_media_subscription(subscribe)
                .and_then(|plan| media.subscribe(control, subscribe, plan))
            {
                Ok(result) => ControlDispatch {
                    response: ControlResponse {
                        request_id,
                        result: Some(control_response::Result::Ok(crate::api::proto::Ok {
                            result: Some(crate::api::proto::ok::Result::SubscriptionResult(result)),
                        })),
                    },
                    after_send: None,
                    data_messages: Vec::new(),
                    notifications: Vec::new(),
                },
                Err(error) => failed_control_dispatch(request_id, error.code, error.message),
            }
        }
        Some(crate::api::proto::request::Command::SubscribeEvents(subscribe))
            if media.has_subscription(&subscribe.subscription_id) =>
        {
            failed_control_dispatch(
                request_id,
                ErrorCode::InvalidRequest,
                "subscription replacement must keep its target kind",
            )
        }
        Some(crate::api::proto::request::Command::Unsubscribe(unsubscribe)) => {
            media.unsubscribe(control, &unsubscribe.subscription_ids);
            for subscription_id in &unsubscribe.subscription_ids {
                media.cancel_event_subscription(subscription_id);
            }
            handler.unsubscribe_for_session(control.session_id, &unsubscribe.subscription_ids);
            ControlDispatch {
                response: ControlResponse {
                    request_id,
                    result: Some(control_response::Result::Ok(crate::api::proto::Ok {
                        result: None,
                    })),
                },
                after_send: None,
                data_messages: Vec::new(),
                notifications: Vec::new(),
            }
        }
        _ => {
            let data_group = control_data_group(request.command.as_ref());
            let dispatch = handler.handle_for_session(control.session_id, request);
            if matches!(
                dispatch.response.result,
                Some(control_response::Result::Ok(_))
            ) && let Some(data_group) = data_group
            {
                media.cancel_group(&data_group);
            }
            dispatch
        }
    };
    envelope_dispatch(dispatch)
}

fn control_data_group(command: Option<&crate::api::proto::request::Command>) -> Option<String> {
    match command? {
        crate::api::proto::request::Command::StoredMediaCommand(command) => {
            use crate::api::proto::stored_media_command::Action;
            match command.action.as_ref()? {
                Action::Open(open) => Some(format!("stored:{}", open.stored_media_id)),
                Action::Seek(seek) => Some(format!("stored:{}", seek.stored_media_id)),
                Action::Close(close) => Some(format!("stored:{}", close.stored_media_id)),
                Action::QueryTimeline(query) => Some(format!("query:{}", query.query_id)),
                Action::CancelTimelineQuery(cancel) => Some(format!("query:{}", cancel.query_id)),
                Action::SetPlayback(_) | Action::Refill(_) => None,
            }
        }
        crate::api::proto::request::Command::ExportCommand(command) => {
            use crate::api::proto::export_command::Action;
            match command.action.as_ref()? {
                Action::Download(download) => Some(format!("export:{}", download.job_id)),
                Action::Cancel(cancel) => Some(format!("export:{}", cancel.job_id)),
                _ => None,
            }
        }
        crate::api::proto::request::Command::EventSearchCommand(command) => {
            use crate::api::proto::event_search_command::Action;
            match command.action.as_ref()? {
                Action::Query(query) => Some(format!("event-search-query:{}", query.query_id)),
                Action::CancelQuery(cancel) => {
                    Some(format!("event-search-query:{}", cancel.query_id))
                }
                Action::FetchMedia(fetch) => {
                    Some(format!("event-search-media:{}", fetch.transfer_id))
                }
                Action::CancelMedia(cancel) => {
                    Some(format!("event-search-media:{}", cancel.transfer_id))
                }
                Action::ReplaceTerms(_) | Action::SetEmbedding(_) => None,
            }
        }
        _ => None,
    }
}

fn decode_control_request(
    binary: bool,
    payload: &[u8],
) -> Result<crate::api::proto::Request, ControlDecodeError> {
    if !binary {
        return Err(ControlDecodeError {
            request_id: 0,
            message: "control messages must be binary",
        });
    }
    if payload.len() > MAX_CONTROL_MESSAGE_BYTES {
        return Err(ControlDecodeError {
            request_id: 0,
            message: "control message exceeds 64 KiB",
        });
    }
    let Ok(envelope) = ControlEnvelope::decode(payload) else {
        return Err(ControlDecodeError {
            request_id: 0,
            message: "invalid control envelope",
        });
    };
    let Some(control_envelope::Message::Request(request)) = envelope.message else {
        return Err(ControlDecodeError {
            request_id: 0,
            message: "expected a control request",
        });
    };
    if request.command.is_none() {
        return Err(ControlDecodeError {
            request_id: request.request_id,
            message: "control request has no command",
        });
    }
    Ok(request)
}

fn api_data_message(
    binary: bool,
    payload: &[u8],
    handler: Option<&dyn ControlRequestHandler>,
    session_id: SessionId,
    channel: crate::api::proto::DataChannelKind,
) -> Result<(), ControlHandlerError> {
    if !binary {
        return Err(ControlHandlerError::new(
            ErrorCode::InvalidRequest,
            "data messages must be binary",
        ));
    }
    if payload.len() > MAX_DATA_MESSAGE_BYTES {
        return Err(ControlHandlerError::new(
            ErrorCode::InvalidRequest,
            "data message exceeds 64 KiB",
        ));
    }
    let message = crate::api::proto::Message::decode(payload)
        .map_err(|_| ControlHandlerError::new(ErrorCode::InvalidRequest, "invalid data message"))?;
    if message.message.is_none() {
        return Err(ControlHandlerError::new(
            ErrorCode::InvalidRequest,
            "data message has no payload",
        ));
    }
    let handler = handler.ok_or_else(|| {
        ControlHandlerError::new(ErrorCode::Unavailable, "data service is unavailable")
    })?;
    handler.handle_data_for_session(session_id, channel, message)
}

fn unavailable_control_dispatch(request_id: u64) -> ControlDispatch {
    failed_control_dispatch(
        request_id,
        ErrorCode::Unavailable,
        "control service is unavailable",
    )
}

fn failed_control_dispatch(
    request_id: u64,
    code: ErrorCode,
    message: impl Into<String>,
) -> ControlDispatch {
    ControlDispatch {
        response: ControlResponse {
            request_id,
            result: Some(control_response::Result::Error(ControlError {
                code: code as i32,
                message: message.into(),
                details: Vec::new(),
            })),
        },
        after_send: None,
        data_messages: Vec::new(),
        notifications: Vec::new(),
    }
}

fn envelope_dispatch(dispatch: ControlDispatch) -> EnvelopeDispatch {
    EnvelopeDispatch {
        envelope: ControlEnvelope {
            message: Some(control_envelope::Message::Response(dispatch.response)),
        },
        after_send: dispatch.after_send,
        data_messages: dispatch.data_messages,
        notifications: dispatch.notifications,
    }
}

fn control_error(request_id: u64, code: ErrorCode, message: &str) -> EnvelopeDispatch {
    EnvelopeDispatch {
        envelope: ControlEnvelope {
            message: Some(control_envelope::Message::Response(ControlResponse {
                request_id,
                result: Some(control_response::Result::Error(ControlError {
                    code: code as i32,
                    message: message.to_owned(),
                    details: Vec::new(),
                })),
            })),
        },
        after_send: None,
        data_messages: Vec::new(),
        notifications: Vec::new(),
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

    fn fixture_video_keyframe(name: &str, media_type: mp4::MediaType) -> Bytes {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata")
            .join(name);
        let mut reader = mp4::read_mp4(std::fs::File::open(path).unwrap()).unwrap();
        let (&track_id, track) = reader
            .tracks()
            .iter()
            .find(|(_, track)| track.media_type().ok() == Some(media_type))
            .unwrap();
        let config = track.media_config_for_description(1).unwrap();
        let sample = (1..=track.sample_count())
            .find_map(|sample_id| {
                let sample = reader.read_sample(track_id, sample_id).unwrap().unwrap();
                sample.is_sync.then_some(sample)
            })
            .unwrap();
        let mut data = Vec::new();
        let mut append = |nal: &[u8]| {
            data.extend_from_slice(&u32::try_from(nal.len()).unwrap().to_be_bytes());
            data.extend_from_slice(nal);
        };
        match config {
            mp4::MediaConfig::AvcConfig(config) => {
                append(&config.seq_param_set);
                append(&config.pic_param_set);
            }
            mp4::MediaConfig::HevcConfig(config) => {
                append(&config.vps);
                append(&config.sps);
                append(&config.pps);
            }
            _ => panic!("fixture must use H.264 or H.265"),
        }
        data.extend_from_slice(&sample.bytes);
        Bytes::from(data)
    }

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

    fn test_event_data_message() -> crate::api::proto::Message {
        crate::api::proto::Message {
            message: Some(crate::api::proto::message::Message::Event(
                crate::api::proto::EventMessage {
                    message: Some(crate::api::proto::event_message::Message::Attachment(
                        crate::api::proto::EventAttachmentChunk {
                            context: Some(
                                crate::api::proto::event_attachment_chunk::Context::PublicationId(
                                    "publication-1".to_owned(),
                                ),
                            ),
                            event_id: "event-1".to_owned(),
                            revision: 1,
                            attachment_id: "snapshot-1".to_owned(),
                            attachment_type: "snapshot".to_owned(),
                            content_type: "image/jpeg".to_owned(),
                            ordinal: 0,
                            timestamp: None,
                            sequence: 1,
                            chunk_index: 0,
                            chunk_count: 1,
                            payload: vec![0xff, 0xd8, 0xff, 0xd9],
                        },
                    )),
                },
            )),
        }
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
    fn data_cancellation_targets_the_matching_outbound_group() {
        use crate::api::proto::{event_search_command, request, stored_media_command};

        let group = |action| {
            control_data_group(Some(&request::Command::EventSearchCommand(
                crate::api::proto::EventSearchCommand {
                    action: Some(action),
                },
            )))
        };
        assert_eq!(
            group(event_search_command::Action::Query(
                crate::api::proto::QueryEvents {
                    query_id: "query-1".to_owned(),
                    ..Default::default()
                },
            )),
            Some("event-search-query:query-1".to_owned())
        );
        assert_eq!(
            group(event_search_command::Action::CancelQuery(
                crate::api::proto::CancelEventSearchQuery {
                    query_id: "query-1".to_owned(),
                },
            )),
            Some("event-search-query:query-1".to_owned())
        );
        assert_eq!(
            group(event_search_command::Action::FetchMedia(
                crate::api::proto::FetchEventSearchMedia {
                    transfer_id: "media-1".to_owned(),
                    ..Default::default()
                },
            )),
            Some("event-search-media:media-1".to_owned())
        );
        assert_eq!(
            group(event_search_command::Action::CancelMedia(
                crate::api::proto::CancelEventSearchMedia {
                    transfer_id: "media-1".to_owned(),
                },
            )),
            Some("event-search-media:media-1".to_owned())
        );

        let timeline_group = |action| {
            control_data_group(Some(&request::Command::StoredMediaCommand(
                crate::api::proto::StoredMediaCommand {
                    action: Some(action),
                },
            )))
        };
        assert_eq!(
            timeline_group(stored_media_command::Action::QueryTimeline(
                crate::api::proto::QueryStoredMediaTimeline {
                    query_id: "timeline-1".to_owned(),
                    ..Default::default()
                },
            )),
            Some("query:timeline-1".to_owned())
        );
        assert_eq!(
            timeline_group(stored_media_command::Action::CancelTimelineQuery(
                crate::api::proto::CancelStoredMediaTimelineQuery {
                    query_id: "timeline-1".to_owned(),
                },
            )),
            Some("query:timeline-1".to_owned())
        );
    }

    #[test]
    fn cancelled_background_data_never_enters_the_outbound_queue() {
        let (tx, rx) = bounded(4);
        let active = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(true));
        for (group, token) in [
            ("event-search-query:active", active.clone()),
            ("event-search-query:cancelled", cancelled),
        ] {
            tx.send(ApiSessionCommand::Data {
                message: Box::new(OutboundDataMessage {
                    target: DataChannelTarget::Reliable,
                    group: group.to_owned(),
                    message: crate::api::proto::Message::default(),
                }),
                cancelled: token,
            })
            .unwrap();
        }
        let mut media = ApiMediaRuntime::default();
        drain_api_session_commands(&rx, &mut media);
        assert_eq!(media.outbound.len(), 1);
        assert_eq!(media.outbound[0].group(), "event-search-query:active");
        active.store(true, Ordering::Release);
        media.cancel_group("event-search-query:active");
        assert!(media.outbound.is_empty());
        tx.send(ApiSessionCommand::Data {
            message: Box::new(OutboundDataMessage {
                target: DataChannelTarget::Reliable,
                group: "event-search-query:active".to_owned(),
                message: crate::api::proto::Message::default(),
            }),
            cancelled: active,
        })
        .unwrap();
        let completed = Arc::new(AtomicBool::new(false));
        let completed_from_queue = completed.clone();
        tx.send(ApiSessionCommand::Complete {
            group: "event-search-query:active".to_owned(),
            completion: ApiDataCompletion::new(Box::new(move || {
                completed_from_queue.store(true, Ordering::Release);
            })),
        })
        .unwrap();
        drain_api_session_commands(&rx, &mut media);
        assert_eq!(media.outbound.len(), 1);
        let QueuedApiData::Complete { group, completion } = media.outbound.pop_front().unwrap()
        else {
            panic!("completion marker must follow cancelled data");
        };
        assert_eq!(group, "event-search-query:active");
        completion.finish();
        assert!(completed.load(Ordering::Acquire));
    }

    #[test]
    fn background_data_waits_at_the_outbound_byte_limit() {
        let (tx, rx) = bounded(2);
        tx.send(ApiSessionCommand::Data {
            message: Box::new(OutboundDataMessage {
                target: DataChannelTarget::Reliable,
                group: "event-search-media:waiting".to_owned(),
                message: crate::api::proto::Message::default(),
            }),
            cancelled: Arc::new(AtomicBool::new(false)),
        })
        .unwrap();
        let mut media = ApiMediaRuntime::default();
        media
            .outbound
            .push_back(QueuedApiData::Message(QueuedDataMessage {
                target: DataChannelTarget::Reliable,
                group: "full".to_owned(),
                payload: vec![0; API_BACKGROUND_OUTBOUND_MAX_BYTES],
            }));
        media.outbound_bytes = API_BACKGROUND_OUTBOUND_MAX_BYTES;

        drain_api_session_commands(&rx, &mut media);
        assert_eq!(rx.len(), 1);
        media.cancel_group("full");
        drain_api_session_commands(&rx, &mut media);
        assert!(rx.is_empty());
        assert_eq!(media.outbound.len(), 1);
        assert_eq!(media.outbound[0].group(), "event-search-media:waiting");
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
    fn session_rtc_config_enables_snap_and_automatic_dtls() {
        assert_eq!(rtc_config().dtls_version(), DtlsVersion::Auto);
        assert!(
            test_api_offer()
                .to_sdp_string()
                .lines()
                .any(|line| line.starts_with("a=sctp-init:"))
        );
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
        let (first_sender, _first_rx) = sender(1, None);
        let (second_sender, _second_rx) = sender(2, Some(track_id));
        let (third_sender, _third_rx) = sender(3, None);
        let first = SourceSubscription::fixed(inner.clone(), first_sender, main_source, None);
        let second = SourceSubscription::fixed(inner.clone(), second_sender, sub_source, None);
        let third = SourceSubscription::fixed(inner.clone(), third_sender, main_source, None);
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
        first.record_written_frame();
        second.record_written_frame();
        first.record_discarded_frames(2);
        second
            .sender
            .queue_stats
            .recovery_drops
            .store(3, Ordering::Relaxed);
        inner.queue_recovery_drops.store(3, Ordering::Relaxed);

        let health = webrtc.health_snapshot();

        assert_eq!(health.active_sessions, 3);
        assert_eq!(health.adaptive_sessions, 0);
        assert_eq!(health.multi_track_sessions, 0);
        assert_eq!(health.multi_tracks, 0);
        assert_eq!(health.fixed_sessions, 3);
        assert_eq!(health.active_main, 2);
        assert_eq!(health.active_sub, 1);
        assert_eq!(health.requested_auto, 0);
        assert_eq!(health.requested_high, 0);
        assert_eq!(health.requested_low, 0);
        assert_eq!(health.estimated_bitrate_min_bps, None);
        assert_eq!(health.estimated_bitrate_avg_bps, None);
        assert_eq!(health.estimated_bitrate_max_bps, None);
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

        drop((first, second, third));
    }

    #[test]
    fn local_ice_candidates_exclude_unusable_ipv4_addresses() {
        assert!(usable_ipv4_ice_address(Ipv4Addr::LOCALHOST));
        assert!(usable_ipv4_ice_address(Ipv4Addr::new(192, 168, 1, 10)));
        assert!(!usable_ipv4_ice_address(Ipv4Addr::new(169, 254, 1, 10)));
        assert!(!usable_ipv4_ice_address(Ipv4Addr::BROADCAST));
        assert!(!usable_ipv4_ice_address(Ipv4Addr::new(224, 0, 0, 1)));
        assert!(!usable_ipv4_ice_address(Ipv4Addr::UNSPECIFIED));
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
    fn camera_reset_invalidates_cached_frames_and_advances_source_sequences() {
        let publisher = Publisher::default();
        let source = test_source();
        let other_source = Source {
            camera_ip: "192.0.2.2".parse().unwrap(),
            stream: StreamKind::Sub,
        };
        for source in [source, other_source] {
            publisher.publish(
                source,
                VideoCodec::H264,
                true,
                Instant::now(),
                None,
                Bytes::from_static(&[1]),
            );
        }
        let (tx, _rx) = bounded(1);
        let subscriber_keyframe = Arc::new(Mutex::new(Some(live_frame(true))));
        publisher
            .inner
            .sources
            .lock()
            .unwrap()
            .get_mut(&source)
            .unwrap()
            .subscribers
            .push(SessionSender {
                id: SessionId(1),
                track_id: None,
                tx,
                queue_stats: Arc::new(SessionQueueStats::default()),
                queue_high_water: publisher.inner.queue_high_water.clone(),
                latest_keyframe: subscriber_keyframe.clone(),
                poller: Arc::new(Poller::new().unwrap()),
                shutdown: Arc::new(AtomicBool::new(false)),
            });

        publisher.reset_camera(source.camera_ip);

        let sources = publisher.inner.sources.lock().unwrap();
        assert_eq!(sources[&source].next_sequence, 2);
        assert!(sources[&source].keyframe.is_none());
        assert_eq!(sources[&other_source].next_sequence, 1);
        assert!(sources[&other_source].keyframe.is_some());
        drop(sources);
        assert!(subscriber_keyframe.lock().unwrap().is_none());
        let previews = publisher.inner.camera_preview_keyframes.lock().unwrap();
        assert!(!previews.contains_key(&source.camera_ip));
        assert!(previews.contains_key(&other_source.camera_ip));
    }

    #[test]
    fn cached_keyframe_waits_for_a_contiguous_live_gop() {
        let mut gate = KeyframeGate::new();
        let cached_keyframe = live_frame(true);
        let live_keyframe = live_frame(true);
        let live_p_frame = live_frame(false);

        assert!(!gate.has_live_gop());
        assert!(gate.allows(FrameOrigin::Cached, cached_keyframe.is_keyframe));
        gate.mark_written(FrameOrigin::Cached);
        assert!(!gate.has_live_gop());
        assert!(!gate.allows(FrameOrigin::Cached, cached_keyframe.is_keyframe));
        for _ in 0..3 {
            assert!(!gate.allows(FrameOrigin::Live, live_p_frame.is_keyframe));
        }

        assert!(gate.allows(FrameOrigin::Live, live_keyframe.is_keyframe));
        gate.mark_written(FrameOrigin::Live);
        assert!(gate.has_live_gop());
        for _ in 0..3 {
            assert!(gate.allows(FrameOrigin::Live, live_p_frame.is_keyframe));
        }
    }

    #[test]
    fn adaptive_startup_prefers_cached_then_live_substream() {
        let webrtc = WebRtc::default();
        let camera_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let main_source = Source {
            camera_ip,
            stream: StreamKind::Main,
        };
        let sub_source = Source {
            camera_ip,
            stream: StreamKind::Sub,
        };
        for (source, payload) in [
            (main_source, Bytes::from_static(&[1])),
            (main_source, Bytes::from_static(&[2])),
        ] {
            webrtc.live.publish(
                source,
                VideoCodec::H264,
                true,
                Instant::now(),
                None,
                payload,
            );
        }
        assert!(
            !webrtc
                .live
                .inner
                .camera_preview_keyframes
                .lock()
                .unwrap()
                .contains_key(&camera_ip)
        );
        for (source, payload) in [
            (sub_source, Bytes::from_static(&[3])),
            (main_source, Bytes::from_static(&[4])),
            (sub_source, Bytes::from_static(&[5])),
        ] {
            webrtc.live.publish(
                source,
                VideoCodec::H264,
                true,
                Instant::now(),
                None,
                payload,
            );
        }
        {
            let sources = webrtc.live.inner.sources.lock().unwrap();
            assert_eq!(
                sources[&main_source]
                    .keyframe
                    .as_ref()
                    .unwrap()
                    .data
                    .avcc
                    .as_ref(),
                &[4]
            );
            assert_eq!(
                sources[&sub_source]
                    .keyframe
                    .as_ref()
                    .unwrap()
                    .data
                    .avcc
                    .as_ref(),
                &[5]
            );
        }
        let preview = webrtc
            .live
            .inner
            .camera_preview_keyframes
            .lock()
            .unwrap()
            .get(&camera_ip)
            .cloned()
            .unwrap();
        assert_eq!(preview.frame.data.avcc.as_ref(), &[5]);

        let (tx, rx) = bounded(4);
        let prepared_keyframe = Arc::new(Mutex::new(None));
        let sender = SessionSender {
            id: SessionId(1),
            track_id: Some(TrackId::parse("camera-0".to_owned()).unwrap()),
            tx,
            queue_stats: Arc::new(SessionQueueStats::default()),
            queue_high_water: webrtc.live.inner.queue_high_water.clone(),
            latest_keyframe: prepared_keyframe.clone(),
            poller: Arc::new(Poller::new().unwrap()),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        let subscription = SourceSubscription::adaptive(
            webrtc.live.inner.clone(),
            sender,
            main_source,
            Some(sub_source),
            Arc::new(SessionControl::new(StreamQuality::High, StreamKind::Main)),
            None,
            "camera".to_owned(),
        );
        assert_eq!(subscription.active_source, sub_source);
        assert_eq!(subscription.startup_fallback(true), None);
        assert_eq!(subscription.startup_fallback(false), Some(main_source));
        {
            let sources = webrtc.live.inner.sources.lock().unwrap();
            assert_eq!(sources[&sub_source].subscribers.len(), 1);
            assert!(sources[&main_source].subscribers.is_empty());
        }
        subscription.prepare_keyframe(&rx);
        let prepared = prepared_keyframe.lock().unwrap().clone().unwrap();
        assert_eq!(prepared.data.avcc.as_ref(), &[5]);
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
            poller,
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
        let control = Arc::new(SessionControl::new(StreamQuality::High, StreamKind::Main));
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
            assert_eq!(sources[&low_source].subscribers.len(), 1);
            assert_eq!(
                sources
                    .get(&high_source)
                    .map_or(0, |state| state.subscribers.len()),
                0
            );
        }

        subscription.begin_switch(high_source);
        assert_eq!(subscription.active_source, low_source);
        assert_eq!(subscription.pending_source, Some(high_source));
        {
            let sources = inner
                .sources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(sources[&high_source].subscribers.len(), 1);
            assert_eq!(sources[&low_source].subscribers.len(), 1);
        }
        assert!(!subscription.finish_switch_on_frame(high_source, false));
        assert_eq!(subscription.active_source, low_source);
        assert!(subscription.finish_switch_on_frame(high_source, true));
        assert_eq!(subscription.active_source, high_source);
        assert_eq!(subscription.pending_source, None);

        let sources = inner
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(sources[&high_source].subscribers.len(), 1);
        assert_eq!(sources[&low_source].subscribers.len(), 0);
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
            poller,
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
        let control = Arc::new(SessionControl::new(StreamQuality::Auto, StreamKind::Sub));
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
    fn canonical_session_channels_use_the_documented_sctp_topology() {
        let [control, reliable, unreliable] = session_channel_configs();

        assert_eq!(control.label, "control-channel");
        assert_eq!(control.negotiated, Some(0));
        assert!(control.ordered);
        assert_eq!(control.reliability, Reliability::Reliable);
        assert_eq!(reliable.label, "reliable-data");
        assert_eq!(reliable.negotiated, Some(1));
        assert!(reliable.ordered);
        assert_eq!(reliable.reliability, Reliability::Reliable);
        assert_eq!(unreliable.label, "unreliable-data");
        assert_eq!(unreliable.negotiated, Some(2));
        assert!(!unreliable.ordered);
        assert_eq!(
            unreliable.reliability,
            Reliability::MaxRetransmits { retransmits: 0 }
        );
    }

    #[test]
    fn reliable_event_data_is_decoded_and_bound_to_its_api_session() {
        struct DataHandler {
            received: Mutex<
                Option<(
                    SessionId,
                    crate::api::proto::DataChannelKind,
                    crate::api::proto::Message,
                )>,
            >,
        }

        impl ControlRequestHandler for DataHandler {
            fn handle(&self, request: crate::api::proto::Request) -> ControlDispatch {
                failed_control_dispatch(
                    request.request_id,
                    ErrorCode::UnsupportedRequest,
                    "test handler does not process control requests",
                )
            }

            fn handle_data_for_session(
                &self,
                session_id: SessionId,
                channel: crate::api::proto::DataChannelKind,
                message: crate::api::proto::Message,
            ) -> Result<(), ControlHandlerError> {
                *self.received.lock().unwrap() = Some((session_id, channel, message));
                Ok(())
            }
        }

        let message = test_event_data_message();
        let handler = DataHandler {
            received: Mutex::new(None),
        };

        api_data_message(
            true,
            &message.encode_to_vec(),
            Some(&handler),
            SessionId(42),
            crate::api::proto::DataChannelKind::ReliableData,
        )
        .unwrap();

        let (session_id, channel, received) = handler.received.lock().unwrap().take().unwrap();
        assert_eq!(session_id, SessionId(42));
        assert_eq!(channel, crate::api::proto::DataChannelKind::ReliableData);
        assert_eq!(received, message);
    }

    #[test]
    fn inbound_data_rejects_text_messages() {
        let error = api_data_message(
            false,
            b"not binary",
            None,
            SessionId(42),
            crate::api::proto::DataChannelKind::ReliableData,
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(error.message, "data messages must be binary");
    }

    #[test]
    fn inbound_data_rejects_malformed_protobuf() {
        let error = api_data_message(
            true,
            &[0xff],
            None,
            SessionId(42),
            crate::api::proto::DataChannelKind::ReliableData,
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(error.message, "invalid data message");
    }

    #[test]
    fn inbound_data_rejects_empty_envelopes() {
        let error = api_data_message(
            true,
            &crate::api::proto::Message::default().encode_to_vec(),
            None,
            SessionId(42),
            crate::api::proto::DataChannelKind::ReliableData,
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(error.message, "data message has no payload");
    }

    #[test]
    fn inbound_data_rejects_oversized_messages_before_decode() {
        let error = api_data_message(
            true,
            &vec![0; MAX_DATA_MESSAGE_BYTES + 1],
            None,
            SessionId(42),
            crate::api::proto::DataChannelKind::ReliableData,
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(error.message, "data message exceeds 64 KiB");
    }

    #[test]
    fn inbound_data_rejects_valid_messages_without_a_handler() {
        let error = api_data_message(
            true,
            &test_event_data_message().encode_to_vec(),
            None,
            SessionId(42),
            crate::api::proto::DataChannelKind::ReliableData,
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::Unavailable);
        assert_eq!(error.message, "data service is unavailable");
    }

    #[test]
    fn api_session_notification_commands_enter_the_control_queue() {
        let (sender, receiver) = bounded(1);
        let notification = crate::api::proto::Notification {
            event: Some(
                crate::api::proto::notification::Event::EventPublicationState(
                    crate::api::proto::EventPublicationState {
                        publication_id: "publication-1".to_owned(),
                        status: crate::api::proto::EventPublicationStatus::Expired as i32,
                        ..Default::default()
                    },
                ),
            ),
        };
        sender
            .send(ApiSessionCommand::Notification(Box::new(
                notification.clone(),
            )))
            .unwrap();
        let mut media = ApiMediaRuntime::default();

        drain_api_session_commands(&receiver, &mut media);

        assert_eq!(media.control_notifications.pop_front(), Some(notification));
    }

    #[test]
    fn live_event_delivery_chunks_shared_bytes_and_cancels_by_subscription() {
        let payload = vec![7; API_EVENT_ATTACHMENT_CHUNK_BYTES + 1];
        let event = crate::api::proto::Event {
            event_id: "event-1".to_owned(),
            revision: 2,
            subscription_id: Some("events-1".to_owned()),
            attachments: vec![crate::api::proto::EventAttachmentDescriptor {
                attachment_id: "snapshot-1".to_owned(),
                attachment_type: "snapshot".to_owned(),
                content_type: "image/jpeg".to_owned(),
                byte_len: Some(payload.len() as u64),
                ordinal: 0,
                timestamp: None,
                text: None,
            }],
            canonical_attachment_id: Some("snapshot-1".to_owned()),
            ..Default::default()
        };
        let mut media = ApiMediaRuntime::default();

        media
            .enqueue_event(OutboundEventDelivery {
                event: event.clone(),
                attachment_target: Some(DataChannelTarget::Reliable),
                attachment_bytes: Some(Arc::from(payload.clone())),
            })
            .unwrap();

        assert!(matches!(
            media.control_notifications.front(),
            Some(crate::api::proto::Notification {
                event: Some(crate::api::proto::notification::Event::LiveEvent(delivered))
            }) if delivered == &event
        ));
        let mut reassembled = Vec::new();
        for (index, queued) in media.outbound.iter().enumerate() {
            let QueuedApiData::Message(queued) = queued else {
                panic!("live event output must contain only attachment messages");
            };
            let message = crate::api::proto::Message::decode(queued.payload.as_slice()).unwrap();
            let Some(crate::api::proto::message::Message::Event(event)) = message.message else {
                panic!("live event output must use an event message");
            };
            let Some(crate::api::proto::event_message::Message::Attachment(chunk)) = event.message
            else {
                panic!("live event output must use an attachment chunk");
            };
            assert!(matches!(
                chunk.context,
                Some(crate::api::proto::event_attachment_chunk::Context::SubscriptionId(
                    ref subscription_id
                )) if subscription_id == "events-1"
            ));
            assert_eq!(chunk.chunk_index, index as u32);
            assert_eq!(chunk.chunk_count, 2);
            reassembled.extend_from_slice(&chunk.payload);
        }
        assert_eq!(reassembled, payload);

        media.cancel_event_subscription("events-1");
        assert!(media.control_notifications.is_empty());
        assert_eq!(media.control_notification_bytes, 0);
        assert!(media.outbound.is_empty());
        assert_eq!(media.outbound_bytes, 0);
    }

    #[test]
    fn control_notification_queue_has_count_and_byte_limits() {
        let mut media = ApiMediaRuntime::default();
        for _ in 0..API_CONTROL_NOTIFICATION_MAX_MESSAGES {
            media
                .enqueue_control_notification(crate::api::proto::Notification::default())
                .unwrap();
        }
        assert!(
            media
                .enqueue_control_notification(crate::api::proto::Notification::default())
                .is_err()
        );
        assert_eq!(
            media.control_notifications.len(),
            API_CONTROL_NOTIFICATION_MAX_MESSAGES
        );

        let mut media = ApiMediaRuntime::default();
        let oversized = crate::api::proto::Notification {
            event: Some(crate::api::proto::notification::Event::LiveEvent(
                crate::api::proto::Event {
                    text: Some("x".repeat(API_CONTROL_NOTIFICATION_MAX_BYTES)),
                    ..Default::default()
                },
            )),
        };
        assert!(media.enqueue_control_notification(oversized).is_err());
        assert!(media.control_notifications.is_empty());
        assert_eq!(media.control_notification_bytes, 0);
    }

    #[test]
    fn live_event_enqueue_is_nonblocking_when_a_session_queue_is_full() {
        let webrtc = WebRtc::new();
        let session_id = SessionId::from_u64(7);
        let poller = Arc::new(Poller::new().unwrap());
        let (data_tx, data_rx) = bounded(1);
        let control = Arc::new(ApiSessionControl {
            session_id,
            inner: webrtc.live.inner.clone(),
            recording_demand: None,
            poller,
            shutdown: Arc::new(AtomicBool::new(false)),
            completion: SessionCompletion::default(),
            control_handler: Arc::new(RwLock::new(None)),
            data_tx,
            pending_event_bytes: Arc::new(AtomicUsize::new(0)),
            media_camera_ips: Mutex::new(HashSet::new()),
            background_operation_in_flight: Arc::new(AtomicBool::new(false)),
        });
        webrtc
            .live
            .inner
            .sessions
            .insert_api(session_id, control.clone());
        let delivery = || OutboundEventDelivery {
            event: crate::api::proto::Event {
                event_id: "event-1".to_owned(),
                revision: 1,
                subscription_id: Some("events-1".to_owned()),
                ..Default::default()
            },
            attachment_target: None,
            attachment_bytes: None,
        };

        assert!(
            webrtc
                .try_enqueue_api_event(session_id, delivery())
                .unwrap()
        );
        assert!(control.pending_event_bytes.load(Ordering::Acquire) > 0);
        let mut media = ApiMediaRuntime::default();
        drain_api_session_commands(&data_rx, &mut media);
        assert_eq!(control.pending_event_bytes.load(Ordering::Acquire), 0);
        control
            .data_tx
            .send(ApiSessionCommand::Notification(Box::default()))
            .unwrap();
        assert!(
            !webrtc
                .try_enqueue_api_event(session_id, delivery())
                .unwrap()
        );
    }

    #[test]
    fn pending_event_reservation_releases_when_the_session_queue_closes() {
        let pending_bytes = Arc::new(AtomicUsize::new(7));
        let (sender, receiver) = bounded(1);
        sender
            .send(ApiSessionCommand::Event {
                delivery: Box::new(OutboundEventDelivery {
                    event: crate::api::proto::Event::default(),
                    attachment_target: None,
                    attachment_bytes: None,
                }),
                reservation: PendingEventReservation {
                    bytes: 7,
                    pending_bytes: pending_bytes.clone(),
                },
            })
            .unwrap();

        discard_api_session_commands(&receiver);

        assert_eq!(pending_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn camera_reset_advances_generation_and_notifies_the_control_handler() {
        struct ResetHandler {
            reset: Mutex<Option<IpAddr>>,
        }

        impl ControlRequestHandler for ResetHandler {
            fn handle(&self, request: crate::api::proto::Request) -> ControlDispatch {
                failed_control_dispatch(
                    request.request_id,
                    ErrorCode::UnsupportedRequest,
                    "test handler does not process control requests",
                )
            }

            fn source_reset(&self, camera_ip: IpAddr) {
                *self.reset.lock().unwrap() = Some(camera_ip);
            }
        }

        let webrtc = WebRtc::new();
        let handler = Arc::new(ResetHandler {
            reset: Mutex::new(None),
        });
        let handler_trait: Arc<dyn ControlRequestHandler> = handler.clone();
        webrtc.set_control_handler(Arc::downgrade(&handler_trait));
        let camera_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let api_control = |session_id, media_camera_ips| {
            Arc::new(ApiSessionControl {
                session_id,
                inner: webrtc.live.inner.clone(),
                recording_demand: None,
                poller: Arc::new(Poller::new().unwrap()),
                shutdown: Arc::new(AtomicBool::new(false)),
                completion: SessionCompletion::default(),
                control_handler: Arc::new(RwLock::new(None)),
                data_tx: bounded(1).0,
                pending_event_bytes: Arc::new(AtomicUsize::new(0)),
                media_camera_ips: Mutex::new(media_camera_ips),
                background_operation_in_flight: Arc::new(AtomicBool::new(false)),
            })
        };
        let affected = api_control(SessionId::from_u64(7), HashSet::from([camera_ip]));
        let unrelated = api_control(
            SessionId::from_u64(8),
            HashSet::from([IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))]),
        );
        webrtc
            .live
            .inner
            .sessions
            .insert_api(affected.session_id, affected.clone());
        webrtc
            .live
            .inner
            .sessions
            .insert_api(unrelated.session_id, unrelated.clone());

        webrtc.live().reset_camera(camera_ip);

        assert_eq!(webrtc.camera_generation(camera_ip), 1);
        assert_eq!(*handler.reset.lock().unwrap(), Some(camera_ip));
        assert!(affected.shutdown.load(Ordering::Acquire));
        assert!(!unrelated.shutdown.load(Ordering::Acquire));
        webrtc.live.inner.sessions.remove_api(affected.session_id);
        webrtc.live.inner.sessions.remove_api(unrelated.session_id);
    }

    #[test]
    fn background_camera_operations_dispatch_without_blocking_the_api_session() {
        use crate::api::proto::{
            CameraConfigurationCommand, CommitEventPublication, DiscoverCameras,
            EventPublicationCommand, ProbeCameraStreams, Request, camera_configuration_command,
            event_publication_command, request,
        };

        struct DiscoveryHandler {
            started: Sender<()>,
            release: Receiver<()>,
        }

        impl ControlRequestHandler for DiscoveryHandler {
            fn handle(&self, request: Request) -> ControlDispatch {
                self.started.send(()).unwrap();
                self.release.recv().unwrap();
                failed_control_dispatch(
                    request.request_id,
                    ErrorCode::UnsupportedRequest,
                    "test discovery response",
                )
            }
        }

        let inner = Arc::new(Inner::default());
        let poller = Arc::new(Poller::new().unwrap());
        let discovery_in_flight = Arc::new(AtomicBool::new(true));
        let control = ApiSessionControl {
            session_id: SessionId(1),
            inner,
            recording_demand: None,
            poller,
            shutdown: Arc::new(AtomicBool::new(false)),
            completion: SessionCompletion::default(),
            control_handler: Arc::new(RwLock::new(None)),
            data_tx: bounded(1).0,
            pending_event_bytes: Arc::new(AtomicUsize::new(0)),
            media_camera_ips: Mutex::new(HashSet::new()),
            background_operation_in_flight: discovery_in_flight.clone(),
        };
        let request = Request {
            request_id: 42,
            command: Some(request::Command::CameraConfigurationCommand(
                CameraConfigurationCommand {
                    action: Some(camera_configuration_command::Action::Discover(
                        DiscoverCameras {
                            subnets: vec![137],
                            networks: Vec::new(),
                            discovery_id: String::new(),
                        },
                    )),
                },
            )),
        };
        assert!(is_background_control_request(&request));
        let probe_request = Request {
            request_id: 43,
            command: Some(request::Command::CameraConfigurationCommand(
                CameraConfigurationCommand {
                    action: Some(camera_configuration_command::Action::ProbeStreams(
                        ProbeCameraStreams {
                            ip: "192.0.2.50".to_owned(),
                            username: "operator".to_owned(),
                            password: "secret".to_owned(),
                            onvif_port: Some(8000),
                            ..Default::default()
                        },
                    )),
                },
            )),
        };
        assert!(is_background_control_request(&probe_request));
        let commit_request = Request {
            request_id: 44,
            command: Some(request::Command::EventPublicationCommand(
                EventPublicationCommand {
                    action: Some(event_publication_command::Action::Commit(
                        CommitEventPublication {
                            publication_id: "publication-1".to_owned(),
                            wait_timeout: None,
                        },
                    )),
                },
            )),
        };
        assert!(is_background_control_request(&commit_request));

        let (started_tx, started_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(0);
        let (dispatch_tx, dispatch_rx) = bounded(1);
        spawn_background_control_operation(
            request,
            Arc::new(DiscoveryHandler {
                started: started_tx,
                release: release_rx,
            }),
            &control,
            dispatch_tx,
        )
        .unwrap();

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(dispatch_rx.try_recv().is_err());
        release_tx.send(()).unwrap();

        let dispatch = dispatch_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(dispatch.response.request_id, 42);
        assert!(!discovery_in_flight.load(Ordering::Acquire));
    }

    #[test]
    fn control_requests_receive_correlated_fail_closed_responses() {
        use crate::api::proto::{CameraControlCommand, Request, request};

        struct UnsupportedHandler;

        impl ControlRequestHandler for UnsupportedHandler {
            fn handle(&self, request: Request) -> ControlDispatch {
                ControlDispatch {
                    response: ControlResponse {
                        request_id: request.request_id,
                        result: Some(control_response::Result::Error(ControlError {
                            code: ErrorCode::UnsupportedRequest as i32,
                            message: "unsupported test command".to_owned(),
                            details: Vec::new(),
                        })),
                    },
                    after_send: None,
                    data_messages: Vec::new(),
                    notifications: Vec::new(),
                }
            }
        }

        let request = ControlEnvelope {
            message: Some(control_envelope::Message::Request(Request {
                request_id: 42,
                command: Some(request::Command::CameraControlCommand(
                    CameraControlCommand { action: None },
                )),
            })),
        };

        let reply = control_reply(true, &request.encode_to_vec(), Some(&UnsupportedHandler));
        let Some(control_envelope::Message::Response(response)) = reply.envelope.message else {
            panic!("control request must produce a response envelope");
        };
        assert_eq!(response.request_id, 42);
        let Some(control_response::Result::Error(error)) = response.result else {
            panic!("unsupported control request must fail closed");
        };
        assert_eq!(error.code, ErrorCode::UnsupportedRequest as i32);
    }

    #[test]
    fn control_output_retries_backpressure_without_losing_dispatch_order() {
        let action_ran = Arc::new(AtomicBool::new(false));
        let action_result = action_ran.clone();
        let mut dispatch = envelope_dispatch(failed_control_dispatch(
            42,
            ErrorCode::Rejected,
            "test response",
        ));
        dispatch.data_messages.push(OutboundDataMessage {
            target: DataChannelTarget::Reliable,
            group: "test-data".to_owned(),
            message: crate::api::proto::Message::default(),
        });
        dispatch
            .notifications
            .push(crate::api::proto::Notification::default());
        dispatch.after_send = Some(Box::new(move || {
            action_result.store(true, Ordering::Release);
        }));

        let (dispatch_tx, dispatch_rx) = bounded(1);
        let mut control_runtime = ApiControlRuntime {
            dispatch_tx,
            dispatch_rx,
            outbound: VecDeque::new(),
            outbound_bytes: 0,
            outbound_messages: 0,
            after_send: Vec::new(),
        };
        let mut media = ApiMediaRuntime::default();
        media.enqueue_stream_state(
            &TrackId::parse("camera-0".to_owned()).unwrap(),
            StreamKind::Sub,
        );
        enqueue_control_dispatch(dispatch, &mut control_runtime).unwrap();

        let mut blocked_writes = 0;
        flush_control_channel_outputs(&mut control_runtime, &mut media, |_| {
            blocked_writes += 1;
            Ok(false)
        })
        .unwrap();
        assert_eq!(blocked_writes, 1);
        assert_eq!(control_runtime.outbound.len(), 4);
        assert!(media.outbound.is_empty());
        assert_eq!(media.control_notifications.len(), 1);
        assert!(control_runtime.after_send.is_empty());
        assert!(!action_ran.load(Ordering::Acquire));

        let mut envelopes = Vec::new();
        flush_control_channel_outputs(&mut control_runtime, &mut media, |payload| {
            envelopes.push(ControlEnvelope::decode(payload).unwrap());
            Ok(true)
        })
        .unwrap();

        assert!(control_runtime.outbound.is_empty());
        assert_eq!(control_runtime.outbound_bytes, 0);
        assert_eq!(control_runtime.outbound_messages, 0);
        assert_eq!(envelopes.len(), 3);
        assert!(matches!(
            &envelopes[0].message,
            Some(control_envelope::Message::Response(response)) if response.request_id == 42
        ));
        assert!(matches!(
            &envelopes[1].message,
            Some(control_envelope::Message::Notification(_))
        ));
        assert!(matches!(
            &envelopes[2].message,
            Some(control_envelope::Message::Notification(
                crate::api::proto::Notification {
                    event: Some(
                        crate::api::proto::notification::Event::SubscriptionStreamState(
                            crate::api::proto::SubscriptionStreamState {
                                subscription_id,
                                active_variant_id,
                            }
                        )
                    )
                }
            )) if subscription_id == "camera-0" && active_variant_id == "sub"
        ));
        assert!(media.control_notifications.is_empty());
        assert_eq!(media.outbound.len(), 1);
        assert_eq!(media.outbound[0].group(), "test-data");
        assert_eq!(control_runtime.after_send.len(), 1);
        for action in std::mem::take(&mut control_runtime.after_send) {
            action();
        }
        assert!(action_ran.load(Ordering::Acquire));
    }

    #[test]
    fn api_media_quality_update_reuses_mid_without_dropping_the_active_source() {
        let inner = Arc::new(Inner::default());
        let poller = Arc::new(Poller::new().unwrap());
        let shutdown = Arc::new(AtomicBool::new(false));
        let camera_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let session = ApiSessionControl {
            session_id: SessionId(1),
            inner: inner.clone(),
            recording_demand: None,
            poller,
            shutdown,
            completion: SessionCompletion::default(),
            control_handler: Arc::new(RwLock::new(None)),
            data_tx: bounded(1).0,
            pending_event_bytes: Arc::new(AtomicUsize::new(0)),
            media_camera_ips: Mutex::new(HashSet::new()),
            background_operation_in_flight: Arc::new(AtomicBool::new(false)),
        };
        let mut media = ApiMediaRuntime {
            available_video_mids: vec![Mid::from("video_0")],
            tracks: Vec::new(),
            outbound: VecDeque::new(),
            outbound_bytes: 0,
            control_notifications: VecDeque::new(),
            control_notification_bytes: 0,
        };
        let request = crate::api::proto::SubscribeMedia {
            subscription_id: "front-door".to_owned(),
            source_session_id: "camera:front-door".to_owned(),
            kind: crate::api::proto::MediaKind::Video as i32,
            requested_delivery_transport: crate::api::proto::DeliveryTransport::Rtp as i32,
            video_quality: crate::api::proto::VideoQuality::Auto as i32,
            variant_id: String::new(),
        };

        let first = media
            .subscribe(
                &session,
                &request,
                MediaSubscriptionPlan {
                    source_session_id: request.source_session_id.clone(),
                    camera_ip,
                    has_sub_stream: true,
                    recording_label: "front-door".to_owned(),
                    quality: StreamQuality::Auto,
                    delivery_transport: crate::api::proto::DeliveryTransport::Rtp,
                    codec: crate::api::proto::CodecDescriptor {
                        name: "h264".to_owned(),
                        parameters: HashMap::new(),
                    },
                    format: crate::api::proto::MediaDataFormat::default(),
                    selected_variant_id: "sub".to_owned(),
                },
            )
            .unwrap();
        assert!(matches!(
            first.delivery,
            Some(crate::api::proto::subscription_result::Delivery::Rtp(
                crate::api::proto::RtpDelivery { ref mid }
            )) if mid == "video_0"
        ));
        assert!(media.available_video_mids.is_empty());
        assert_eq!(
            inner.sources.lock().unwrap()[&Source {
                camera_ip,
                stream: StreamKind::Sub
            }]
                .subscribers
                .len(),
            1
        );
        assert!(matches!(
            media.control_notifications.pop_front(),
            Some(crate::api::proto::Notification {
                event: Some(
                    crate::api::proto::notification::Event::SubscriptionStreamState(
                        crate::api::proto::SubscriptionStreamState {
                            subscription_id,
                            active_variant_id,
                        }
                    )
                )
            }) if subscription_id == request.subscription_id && active_variant_id == "sub"
        ));

        let replacement = media
            .subscribe(
                &session,
                &crate::api::proto::SubscribeMedia {
                    video_quality: crate::api::proto::VideoQuality::High as i32,
                    ..request.clone()
                },
                MediaSubscriptionPlan {
                    source_session_id: request.source_session_id.clone(),
                    camera_ip,
                    has_sub_stream: true,
                    recording_label: "front-door".to_owned(),
                    quality: StreamQuality::High,
                    delivery_transport: crate::api::proto::DeliveryTransport::Rtp,
                    codec: crate::api::proto::CodecDescriptor {
                        name: "h264".to_owned(),
                        parameters: HashMap::new(),
                    },
                    format: crate::api::proto::MediaDataFormat::default(),
                    selected_variant_id: "main".to_owned(),
                },
            )
            .unwrap();
        assert!(matches!(
            replacement.delivery,
            Some(crate::api::proto::subscription_result::Delivery::Rtp(
                crate::api::proto::RtpDelivery { ref mid }
            )) if mid == "video_0"
        ));
        assert_eq!(
            media.tracks[0].runtime.subscription.requested_quality(),
            Some(StreamQuality::High)
        );
        assert!(media.control_notifications.is_empty());
        assert_eq!(
            inner.sources.lock().unwrap()[&Source {
                camera_ip,
                stream: StreamKind::Sub
            }]
                .subscribers
                .len(),
            1
        );
        assert_eq!(
            inner
                .sources
                .lock()
                .unwrap()
                .get(&Source {
                    camera_ip,
                    stream: StreamKind::Main
                })
                .map_or(0, |source| source.subscribers.len()),
            0
        );

        media.unsubscribe(&session, &[request.subscription_id]);
        assert_eq!(media.available_video_mids, vec![Mid::from("video_0")]);
        assert!(
            inner
                .sources
                .lock()
                .unwrap()
                .values()
                .all(|source| source.subscribers.is_empty())
        );
    }

    #[test]
    fn reliable_data_media_subscription_binds_and_fragments_encoded_frames() {
        let inner = Arc::new(Inner::default());
        let poller = Arc::new(Poller::new().unwrap());
        let camera_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let session = ApiSessionControl {
            session_id: SessionId(1),
            inner,
            recording_demand: None,
            poller,
            shutdown: Arc::new(AtomicBool::new(false)),
            completion: SessionCompletion::default(),
            control_handler: Arc::new(RwLock::new(None)),
            data_tx: bounded(1).0,
            pending_event_bytes: Arc::new(AtomicUsize::new(0)),
            media_camera_ips: Mutex::new(HashSet::new()),
            background_operation_in_flight: Arc::new(AtomicBool::new(false)),
        };
        let mut media = ApiMediaRuntime::default();
        let request = crate::api::proto::SubscribeMedia {
            subscription_id: "detector-input".to_owned(),
            source_session_id: "camera:front-door".to_owned(),
            kind: crate::api::proto::MediaKind::Video as i32,
            requested_delivery_transport: crate::api::proto::DeliveryTransport::ReliableData as i32,
            video_quality: crate::api::proto::VideoQuality::Low as i32,
            variant_id: String::new(),
        };
        let format = crate::api::proto::MediaDataFormat {
            format: Some(crate::api::proto::media_data_format::Format::Video(
                crate::api::proto::VideoDataFormat {
                    width: 640,
                    height: 360,
                    decoder_config: Vec::new(),
                },
            )),
        };

        let result = media
            .subscribe(
                &session,
                &request,
                MediaSubscriptionPlan {
                    source_session_id: request.source_session_id.clone(),
                    camera_ip,
                    has_sub_stream: true,
                    recording_label: "front-door".to_owned(),
                    quality: StreamQuality::Low,
                    delivery_transport: crate::api::proto::DeliveryTransport::ReliableData,
                    codec: crate::api::proto::CodecDescriptor {
                        name: "h264".to_owned(),
                        parameters: HashMap::new(),
                    },
                    format: format.clone(),
                    selected_variant_id: "sub".to_owned(),
                },
            )
            .unwrap();

        let Some(crate::api::proto::subscription_result::Delivery::MediaData(delivery)) =
            result.delivery
        else {
            panic!("reliable media subscription must return a data binding");
        };
        assert_eq!(delivery.stream_binding_id, "media:detector-input");
        assert_eq!(
            delivery.channel,
            crate::api::proto::DataChannelKind::ReliableData as i32
        );
        assert_eq!(delivery.format, Some(format));
        assert!(media.available_video_mids.is_empty());
        assert_eq!(
            media.tracks[0].runtime.subscription.active_source.stream,
            StreamKind::Sub
        );

        let TrackDelivery::ReliableData(binding) = &mut media.tracks[0].runtime.delivery else {
            panic!("reliable media track must retain its data binding");
        };
        let received_at = Instant::now() - Duration::from_millis(25);
        let mut frame = live_frame_at(true, received_at, API_MEDIA_FRAME_CHUNK_BYTES + 7);
        frame.timestamp = Some(Duration::from_secs(10));
        let messages = encode_media_data_frame(binding, &frame).unwrap();
        assert_eq!(messages.len(), 2);
        let mut payload = Vec::new();
        let mut first_timestamp = None;
        for (index, message) in messages.into_iter().enumerate() {
            assert_eq!(message.target, DataChannelTarget::Reliable);
            let Some(crate::api::proto::message::Message::Video(video)) = message.message.message
            else {
                panic!("media data must use the video envelope");
            };
            let Some(crate::api::proto::video_message::Message::Frame(fragment)) = video.message
            else {
                panic!("video data must contain a frame fragment");
            };
            assert_eq!(fragment.stream_binding_id, delivery.stream_binding_id);
            assert_eq!(fragment.frame_id, 1);
            assert_eq!(fragment.fragment_index, u32::try_from(index).unwrap());
            assert_eq!(fragment.fragment_count, 2);
            assert!(fragment.key_frame);
            assert_eq!(fragment.configuration_revision, 1);
            assert!(fragment.timestamp.is_some());
            first_timestamp = first_timestamp.or(fragment.timestamp);
            payload.extend(fragment.payload);
        }
        assert_eq!(payload, frame.data.avcc);
        assert_eq!(binding.next_frame_id, 2);
        let mut next_frame = live_frame_at(false, received_at, 4);
        next_frame.timestamp = Some(Duration::from_millis(10_250));
        let next = encode_media_data_frame(binding, &next_frame).unwrap();
        let Some(crate::api::proto::message::Message::Video(video)) = &next[0].message.message
        else {
            panic!("media data must use the video envelope");
        };
        let Some(crate::api::proto::video_message::Message::Frame(next)) = &video.message else {
            panic!("video data must contain a frame fragment");
        };
        let first_timestamp = first_timestamp.unwrap();
        let timestamp_millis = |timestamp: &prost_types::Timestamp| {
            timestamp.seconds.saturating_mul(1_000) + i64::from(timestamp.nanos / 1_000_000)
        };
        assert_eq!(
            timestamp_millis(next.timestamp.as_ref().unwrap()) - timestamp_millis(&first_timestamp),
            250
        );
        assert_eq!(binding.next_frame_id, 3);
        let h265_frame = MediaFrame {
            codec: VideoCodec::H265,
            is_keyframe: true,
            received_at: Instant::now(),
            timestamp: Some(Duration::from_secs(11)),
            data: Arc::new(MediaFrameData::new(fixture_video_keyframe(
                "cc-4k-640x360-h265.mp4",
                mp4::MediaType::H265,
            ))),
        };
        let reconfigured = encode_media_data_frame(binding, &h265_frame).unwrap();
        assert!(!reconfigured.is_empty());
        let Some(crate::api::proto::message::Message::Video(video)) =
            &reconfigured[0].message.message
        else {
            panic!("media data must use the video envelope");
        };
        let Some(crate::api::proto::video_message::Message::Frame(frame)) = &video.message else {
            panic!("video data must contain a frame fragment");
        };
        assert_eq!(frame.configuration_revision, 2);
        assert_eq!(binding.codec.name, "h265");
        let configuration = binding.pending_configuration.take().unwrap();
        assert_eq!(configuration.configuration_revision, 2);
        assert_eq!(configuration.codec.as_ref().unwrap().name, "h265");
        let Some(crate::api::proto::media_data_format::Format::Video(format)) =
            configuration.format.unwrap().format
        else {
            panic!("media reconfiguration must contain a video format");
        };
        assert_eq!((format.width, format.height), (640, 360));
        assert!(!format.decoder_config.is_empty());

        media.unsubscribe(&session, &[request.subscription_id]);
        assert!(session.media_camera_ips.lock().unwrap().is_empty());
        let main_request = crate::api::proto::SubscribeMedia {
            subscription_id: "detector-main".to_owned(),
            video_quality: crate::api::proto::VideoQuality::High as i32,
            ..request
        };
        media
            .subscribe(
                &session,
                &main_request,
                MediaSubscriptionPlan {
                    source_session_id: main_request.source_session_id.clone(),
                    camera_ip,
                    has_sub_stream: true,
                    recording_label: "front-door".to_owned(),
                    quality: StreamQuality::High,
                    delivery_transport: crate::api::proto::DeliveryTransport::ReliableData,
                    codec: crate::api::proto::CodecDescriptor {
                        name: "h265".to_owned(),
                        parameters: HashMap::new(),
                    },
                    format: crate::api::proto::MediaDataFormat::default(),
                    selected_variant_id: "main".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(
            media.tracks[0].runtime.subscription.active_source.stream,
            StreamKind::Main
        );
    }

    #[test]
    fn api_session_accepts_the_documented_data_channel_offer() {
        struct SessionCloseHandler {
            closed_tx: Sender<SessionId>,
        }

        impl ControlRequestHandler for SessionCloseHandler {
            fn handle(&self, request: crate::api::proto::Request) -> ControlDispatch {
                failed_control_dispatch(
                    request.request_id,
                    ErrorCode::UnsupportedRequest,
                    "test handler does not process control requests",
                )
            }

            fn session_closed(&self, session_id: SessionId) {
                self.closed_tx.send(session_id).unwrap();
            }
        }

        let mut offerer = rtc_config().build(Instant::now());
        let mut changes = offerer.sdp_api();
        for config in session_channel_configs() {
            changes.add_channel_with_config(config);
        }
        let (offer, _) = changes.apply().unwrap();
        let webrtc = WebRtc::new();
        let (closed_tx, closed_rx) = bounded(1);
        let handler: Arc<dyn ControlRequestHandler> = Arc::new(SessionCloseHandler { closed_tx });
        webrtc.set_control_handler(Arc::downgrade(&handler));

        let session = webrtc.accept_api_offer(offer).unwrap();
        let answer = session.answer.to_sdp_string();
        assert!(answer.lines().any(|line| line.starts_with("m=application")));
        assert!(answer.lines().any(|line| line == "a=ice-lite"));
        assert!(webrtc.active_api_session_ids().contains(&session.id));

        let connected = webrtc.health_snapshot();
        assert_eq!(connected.active_sessions, 1);
        assert_eq!(connected.multi_track_sessions, 1);
        assert_eq!(connected.multi_tracks, 0);
        assert_eq!(connected.fixed_sessions, 0);

        let source = test_source();
        let (tx, _rx) = bounded(1);
        let subscription = SourceSubscription::fixed(
            webrtc.live.inner.clone(),
            SessionSender {
                id: session.id,
                track_id: Some(TrackId::parse("camera-0".to_owned()).unwrap()),
                tx,
                queue_stats: Arc::new(SessionQueueStats::default()),
                queue_high_water: webrtc.live.inner.queue_high_water.clone(),
                latest_keyframe: Arc::new(Mutex::new(None)),
                poller: Arc::new(Poller::new().unwrap()),
                shutdown: Arc::new(AtomicBool::new(false)),
            },
            source,
            None,
        );
        let streaming = webrtc.health_snapshot();
        assert_eq!(streaming.active_sessions, 1);
        assert_eq!(streaming.multi_track_sessions, 1);
        assert_eq!(streaming.multi_tracks, 1);
        assert_eq!(streaming.fixed_sessions, 0);

        drop(subscription);
        assert!(webrtc.close_api_session(session.id));
        assert_eq!(
            closed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            session.id
        );
        assert!(!webrtc.active_api_session_ids().contains(&session.id));
        assert!(!webrtc.close_api_session(session.id));
        assert!(webrtc.live.inner.sessions.threads_are_empty());
        webrtc.shutdown();
    }

    #[test]
    fn dropping_one_track_keeps_sibling_subscription_active() {
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
}
